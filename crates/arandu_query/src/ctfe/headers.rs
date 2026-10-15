//! Declaration-header obligations, separate from function-body staging.
use super::{ConstArguments, CtfeRoot, CtfeRootRequest, RootSelector};
use crate::{db::HashEq, ArandCompilerDb, SourceFile};
use arandu_middle::{ctfe::ConstValue, NodeKey, ResolvedNames, Span, SymbolId};
use arandu_parser::{ast_pool::ExprId, Program, TopLevelDecl, TypeExpr};
use std::sync::Arc;

pub(crate) fn header_arguments(
    program: &Program,
    names: &ResolvedNames,
    owner: SymbolId,
) -> Vec<(Span, ExprId)> {
    let mut span = None;
    let mut bodies = Vec::new();
    program.for_each_decl_recursive(|_, declaration| {
        if let TopLevelDecl::Func(function) = declaration {
            bodies.push(function.body.span);
        }
        if arandu_semantics::primary_def_key(declaration)
            .and_then(|key| names.definitions.get(&key))
            == Some(&owner)
        {
            span = match declaration {
                TopLevelDecl::Const(_)
                | TopLevelDecl::TypeAlias(_)
                | TopLevelDecl::Func(_)
                | TopLevelDecl::Struct(_)
                | TopLevelDecl::Enum(_)
                | TopLevelDecl::Interface(_)
                | TopLevelDecl::Extern(_) => Some(arandu_semantics::item_source_span(declaration)),
                // A namespace contains declarations; their obligations belong
                // to each child, never to an additional namespace root.
                TopLevelDecl::Submodule(_) | TopLevelDecl::Error(_) => None,
            };
        }
    });
    let Some(span) = span else {
        return Vec::new();
    };
    let mut arguments: Vec<_> = program
        .pool
        .type_exprs
        .iter()
        .filter_map(|ty| {
            let TypeExpr::ConstExpression {
                span: child,
                expression,
            } = ty
            else {
                return None;
            };
            (child.start >= span.start
                && child.end <= span.end
                && !bodies
                    .iter()
                    .any(|body| body.start <= child.start && child.end <= body.end))
            .then_some((*child, *expression))
        })
        .collect();
    arguments.sort_by_key(|(span, _)| (span.start, span.end));
    arguments
}

fn cycle(
    db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    file: SourceFile,
    _owner: SymbolId,
) -> HashEq<ConstArguments> {
    HashEq::new(ConstArguments {
        diagnostics: vec![arandu_middle::Diagnostic::error(
            arandu_middle::DiagCode::T044ComptimeEvaluationFailed,
            "cyclic declaration-header constant dependency",
            Span::new(*file.file_id(db), 0, 0),
        )],
        ..ConstArguments::default()
    })
}

#[salsa::tracked(cycle_result = cycle)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db, file), fields(query = "item_header_arguments", item = ?owner))]
pub(crate) fn item_header_arguments(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
) -> HashEq<ConstArguments> {
    HashEq::share(evaluate_declaration_arguments(
        db,
        file,
        arandu_middle::types::FunctionInstance {
            definition: owner,
            arguments: Vec::new(),
        },
        super::DependencyContext::default(),
    ))
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db, file, context), fields(query = "evaluate_declaration_arguments", item = ?instance.definition))]
pub(crate) fn evaluate_declaration_arguments(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    instance: arandu_middle::types::FunctionInstance,
    context: super::DependencyContext,
) -> HashEq<ConstArguments> {
    let owner = instance.definition;
    let source = crate::passes::item_source_input(db, file, owner);
    let seed = crate::passes::seed_header_signatures(db, file);
    let roots = header_arguments(&source.program, &seed.resolved, owner);
    let mut result = ConstArguments::default();
    if roots.is_empty() {
        return HashEq::new(result);
    }
    let context = match context.enter(
        super::dependency::DependencyKey::Header(instance.clone()),
        roots[0].0,
    ) {
        Ok(context) => context,
        Err(diagnostic) => {
            result.diagnostics.push(diagnostic);
            return HashEq::new(result);
        }
    };
    if roots.len() > 4096 {
        result
            .values
            .extend(roots.iter().map(|(span, _)| ((*span).into(), None)));
        result.diagnostics.push(
            arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                "declaration contains too many computed header arguments",
                Span::new(*file.file_id(db), source.item_start, source.item_start),
            )
            .with_primary_label("at most 4096 header arguments per declaration"),
        );
        return HashEq::new(result);
    }
    for (ordinal, (span, _)) in roots.iter().enumerate() {
        result.values.insert(NodeKey::from(*span), None);
        let Ok(ordinal) = u32::try_from(ordinal) else {
            break;
        };
        let selector = if instance.arguments.is_empty() {
            RootSelector::HeaderArgument(ordinal)
        } else {
            RootSelector::InInstance {
                instance: instance.clone(),
                selector: Box::new(RootSelector::HeaderArgument(ordinal)),
            }
        };
        let root = CtfeRoot::new_in_context(db, file, owner, selector, None, context.clone());
        match super::ctfe_eval_root(
            db,
            CtfeRootRequest::new(
                db,
                root,
                super::public::staging_budget(super::config::public_budget(db), roots.len(), 1),
            ),
        ) {
            Ok(ConstValue::Integer(integer)) if integer.to_const_generic().is_ok() => {
                result
                    .values
                    .insert((*span).into(), integer.to_const_generic().ok());
            }
            Ok(
                value @ (ConstValue::Bool(_) | ConstValue::Aggregate(_) | ConstValue::Integer(_)),
            ) => {
                result.typed_values.insert((*span).into(), value.clone());
            }
            Ok(_) => result.diagnostics.push(arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T003IncompatibleCallArg,
                "unsupported header constant value",
                *span,
            )),
            Err(error) => {
                super::public::append_failure(&mut result.diagnostics, error.clone(), *span)
            }
        }
    }
    HashEq::new(result)
}

#[salsa::tracked]
pub(crate) fn module_arguments(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<ConstArguments> {
    let parsed = crate::passes::parse(db, file);
    let Ok(program) = &**parsed else {
        return HashEq::new(ConstArguments::default());
    };
    if !program
        .pool
        .type_exprs
        .iter()
        .any(|ty| matches!(ty, TypeExpr::ConstExpression { .. }))
    {
        return HashEq::new(ConstArguments::default());
    }
    let headers = crate::passes::resolved_headers(db, file);
    let mut result = ConstArguments::default();
    let mut owners = Vec::new();
    program.for_each_decl_recursive(|_, declaration| {
        if let Some(symbol) = arandu_semantics::primary_def_key(declaration)
            .and_then(|key| headers.declarations.resolved.definitions.get(&key))
            .copied()
        {
            if !header_arguments(program, &headers.declarations.resolved, symbol).is_empty() {
                owners.push(symbol);
            }
        }
    });
    owners.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    owners.dedup();
    let seed = crate::passes::seed_header_signatures(db, file);
    for owner in owners {
        if seed.type_info.deferred_headers.contains(&owner) {
            result.values.extend(
                header_arguments(program, &seed.resolved, owner)
                    .iter()
                    .map(|(span, _)| ((*span).into(), None)),
            );
            continue;
        }
        let arguments = item_header_arguments(db, file, owner);
        result
            .values
            .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
        result.typed_values.extend(
            arguments
                .typed_values
                .iter()
                .map(|(&key, value)| (key, value.clone())),
        );
        result
            .diagnostics
            .extend(arguments.diagnostics.iter().cloned());
    }
    HashEq::new(result)
}

pub(crate) fn staged_signatures(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> arandu_typeck::TypeCheckResult {
    let arguments = module_arguments(db, file);
    if arguments.values.is_empty()
        && arguments.typed_values.is_empty()
        && arguments.diagnostics.is_empty()
    {
        return (**crate::passes::seed_header_signatures(db, file)).clone();
    }
    let parsed = crate::passes::parse(db, file);
    let Ok(program) = &**parsed else {
        return arandu_typeck::TypeCheckResult::empty();
    };
    let mut resolution = crate::passes::resolved_headers(db, file)
        .declarations
        .clone();
    let names = Arc::make_mut(&mut resolution.resolved);
    names
        .comptime_arguments
        .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
    names.typed_comptime_arguments.extend(
        arguments
            .typed_values
            .iter()
            .map(|(&key, value)| (key, value.clone())),
    );
    let mut checked = crate::passes::signatures_from_program(db, file, program, &resolution, true);
    checked
        .diagnostics
        .extend(arguments.diagnostics.iter().cloned());
    checked
}

pub(crate) fn owner_signatures_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    context: &super::DependencyContext,
) -> arandu_typeck::TypeCheckResult {
    let source = crate::passes::item_source_input(db, file, owner);
    let seed = crate::passes::seed_header_signatures(db, file);
    if header_arguments(&source.program, &seed.resolved, owner).is_empty() {
        return (**seed).clone();
    }
    let arguments = evaluate_declaration_arguments(
        db,
        file,
        arandu_middle::types::FunctionInstance {
            definition: owner,
            arguments: Vec::new(),
        },
        context.clone(),
    );
    let mut resolution = crate::passes::resolved_headers(db, file)
        .declarations
        .clone();
    let names = Arc::make_mut(&mut resolution.resolved);
    names
        .comptime_arguments
        .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
    names.typed_comptime_arguments.extend(
        arguments
            .typed_values
            .iter()
            .map(|(&key, value)| (key, value.clone())),
    );
    let mut checked =
        crate::passes::signatures_from_program(db, file, &source.program, &resolution, true);
    checked
        .diagnostics
        .extend(arguments.diagnostics.iter().cloned());
    checked
}

/// Demand only nominal declarations named by this root/helper. A field-length
/// obligation cannot force staging of every unrelated declaration in the file.
pub(crate) fn install_referenced_headers_in_context(
    db: &dyn ArandCompilerDb,
    pool: &arandu_parser::ast_pool::AstPool,
    span: Span,
    checked: &mut arandu_typeck::TypeCheckResult,
    context: &super::DependencyContext,
) -> Result<(), super::BuildFailure> {
    let mut symbols: Vec<_> = checked
        .resolved
        .type_refs
        .iter()
        .filter_map(|(key, &symbol)| {
            (span.start <= key.start
                && key.end <= span.end
                && checked.symbols.try_get(symbol).is_some_and(|entry| {
                    matches!(
                        entry.kind,
                        arandu_middle::SymbolKind::Struct
                            | arandu_middle::SymbolKind::Enum
                            | arandu_middle::SymbolKind::TypeAlias
                    )
                }))
            .then_some(symbol)
        })
        .collect();
    // A call can return a nominal type whose name is absent from the root's
    // syntax. Its signature is part of the demanded header contract as well.
    for (symbol, child) in checked.resolved.expr_symbols.iter().zip(&pool.expr_spans) {
        if span.start <= child.start && child.end <= span.end {
            if let Some(ty) = symbol.and_then(|symbol| checked.type_info.decl_type_id(symbol)) {
                if let Ok(shape) =
                    arandu_middle::types::TypeShape::from_id(ty, &checked.type_info.type_interner)
                {
                    let _ = shape.for_each_symbol(|symbol| symbols.push(symbol));
                }
            }
        }
    }
    symbols.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    symbols.dedup();
    for symbol in symbols {
        let file = db
            .source_file_by_id(symbol.file_id)
            .ok_or(super::BuildFailure::InvalidRoot)?;
        let source = crate::passes::item_source_input(db, file, symbol);
        let seed = crate::passes::seed_header_signatures(db, file);
        if seed.type_info.deferred_headers.contains(&symbol)
            || header_arguments(&source.program, &seed.resolved, symbol).is_empty()
        {
            continue;
        }
        let owner = owner_signatures_in_context(db, file, symbol, context);
        let obligations = header_arguments(&source.program, &seed.resolved, symbol);
        let arguments = evaluate_declaration_arguments(
            db,
            file,
            arandu_middle::types::FunctionInstance {
                definition: symbol,
                arguments: Vec::new(),
            },
            context.clone(),
        );
        let errors: Vec<_> = owner
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.severity == arandu_middle::Severity::Error
                    && (arguments.diagnostics.contains(diagnostic)
                        || obligations.iter().any(|(span, _)| {
                            span.file_id == diagnostic.span.file_id
                                && span.start <= diagnostic.span.start
                                && diagnostic.span.end <= span.end
                        }))
            })
            .cloned()
            .collect();
        if !errors.is_empty() {
            return Err(super::BuildFailure::Diagnostics(errors));
        }
        let names = Arc::make_mut(&mut checked.resolved);
        for (span, _) in &obligations {
            let key = NodeKey::from(*span);
            if let Some(value) = owner.resolved.comptime_arguments.get(&key) {
                names.comptime_arguments.insert(key, *value);
            }
            if let Some(value) = owner.resolved.typed_comptime_arguments.get(&key) {
                names.typed_comptime_arguments.insert(key, value.clone());
            }
        }
        checked.diagnostics.retain(|diagnostic| {
            diagnostic.code != arandu_middle::DiagCode::T042UnsupportedComptime
                || !obligations.iter().any(|(span, _)| {
                    span.file_id == diagnostic.span.file_id
                        && span.start <= diagnostic.span.start
                        && diagnostic.span.end <= span.end
                })
        });
        let mut shard =
            arandu_typeck::TypeInfo::with_interner(owner.type_info.type_interner.clone());
        if let Some(ty) = owner.type_info.decl_type_id(symbol) {
            shard.record_decl_type(symbol, ty);
        }
        if let Some(fields) = owner.type_info.struct_fields.get(&symbol) {
            shard.struct_fields.insert(symbol, Arc::clone(fields));
        }
        if let Some(parameters) = owner.type_info.generic_params.get(&symbol) {
            shard.generic_params.insert(symbol, Arc::clone(parameters));
            for parameter in parameters.iter() {
                if let Some(ty) = owner.type_info.decl_type_id(*parameter) {
                    shard.record_decl_type(*parameter, ty);
                }
                if let Some(&ty) = owner.type_info.generic_defaults.get(parameter) {
                    shard.generic_defaults.insert(*parameter, ty);
                }
            }
        }
        for (&variant, (parent, shape)) in &owner.type_info.enum_variants {
            if *parent == symbol {
                shard
                    .enum_variants
                    .insert(variant, (*parent, shape.clone()));
                if let Some(&tag) = owner.type_info.enum_variant_tags.get(&variant) {
                    shard.enum_variant_tags.insert(variant, tag);
                }
                if let Some(ty) = owner.type_info.decl_type_id(variant) {
                    shard.record_decl_type(variant, ty);
                }
            }
        }
        checked.type_info_mut().merge_from(&shard);
    }
    Ok(())
}

/// A template header remains an explicit deferred obligation until concrete
/// arguments are available. None is a placeholder, never an evaluated value.
pub(crate) fn defer_generic_headers(
    program: &Program,
    checker: &mut arandu_typeck::TypeChecker<'_>,
) {
    let mut owners = Vec::new();
    program.for_each_decl_recursive(|_, declaration| {
        let generic = match declaration {
            TopLevelDecl::Func(d) => !d.generic_params.is_empty(),
            TopLevelDecl::Struct(d) => !d.generic_params.is_empty(),
            TopLevelDecl::Enum(d) => !d.generic_params.is_empty(),
            TopLevelDecl::TypeAlias(d) => !d.generic_params.is_empty(),
            TopLevelDecl::Interface(d) => !d.generic_params.is_empty(),
            TopLevelDecl::Const(_)
            | TopLevelDecl::Extern(_)
            | TopLevelDecl::Submodule(_)
            | TopLevelDecl::Error(_) => false,
        };
        if let Some(&owner) = arandu_semantics::primary_def_key(declaration)
            .and_then(|key| checker.resolved.definitions.get(&key))
            .filter(|_| {
                matches!(
                    declaration,
                    TopLevelDecl::Func(_)
                        | TopLevelDecl::Struct(_)
                        | TopLevelDecl::Enum(_)
                        | TopLevelDecl::TypeAlias(_)
                )
            })
        {
            let roots = header_arguments(program, &checker.resolved, owner);
            let dependent = roots.iter().any(|(span, _)| {
                let is_parameter = |symbol| {
                    checker.symbols.try_get(symbol).is_some_and(|entry| {
                        matches!(
                            entry.kind,
                            arandu_middle::SymbolKind::TypeParam
                                | arandu_middle::SymbolKind::ConstParam
                        )
                    })
                };
                checker
                    .resolved
                    .expr_symbols
                    .iter()
                    .zip(&program.pool.expr_spans)
                    .any(|(symbol, use_span)| {
                        span.start <= use_span.start
                            && use_span.end <= span.end
                            && symbol.is_some_and(is_parameter)
                    })
                    || checker.resolved.type_refs.iter().any(|(key, &symbol)| {
                        span.start <= key.start && key.end <= span.end && is_parameter(symbol)
                    })
                    || checker.resolved.value_refs.iter().any(|(key, &symbol)| {
                        span.start <= key.start && key.end <= span.end && is_parameter(symbol)
                    })
            });
            let span = arandu_semantics::item_source_span(declaration);
            let body = match declaration {
                TopLevelDecl::Func(function) => Some(function.body.span),
                _ => None,
            };
            let dependencies: Vec<_> = checker
                .resolved
                .type_refs
                .iter()
                .filter_map(|(key, &symbol)| {
                    (span.start <= key.start
                        && key.end <= span.end
                        && !body.is_some_and(|body| body.start <= key.start && key.end <= body.end)
                        && symbol != owner)
                        .then_some(symbol)
                })
                .collect();
            owners.push((
                owner,
                roots,
                generic && matches!(declaration, TopLevelDecl::Func(_)),
                dependent,
                dependencies,
            ));
        }
    });
    // A field/signature can depend on a calculated alias or nominal header
    // even when this declaration has no explicit comptime expression itself.
    loop {
        let mut changed = false;
        for (owner, _, _, dependent, dependencies) in &owners {
            if *dependent
                || dependencies
                    .iter()
                    .any(|symbol| checker.type_info.deferred_headers.contains(symbol))
            {
                changed |= checker.type_info.deferred_headers.insert(*owner);
            }
        }
        if !changed {
            break;
        }
    }
    for (owner, roots, function, _, _) in owners {
        if !checker.type_info.deferred_headers.contains(&owner) {
            continue;
        }
        let names = Arc::make_mut(&mut checker.resolved);
        if function {
            names.deferred_comptime_functions.insert(owner);
        }
        for (span, _) in roots {
            names.comptime_arguments.entry(span.into()).or_insert(None);
        }
    }
}
