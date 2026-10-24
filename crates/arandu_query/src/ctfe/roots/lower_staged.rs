use std::sync::Arc;

use arandu_middle::{hir::HirDecl, types::ArType, Severity, Span, SymbolId};
use arandu_mir::ctfe::CtfeFunction;
use arandu_parser::{Block, Program, Stmt};

use super::super::BuildFailure;
use super::{CtfeRoot, CtfeRootRequest, RootEvalError, RootExpectedType, RootSelector};
use crate::{ArandCompilerDb, SourceFile};

enum LoweredRoot {
    Block(arandu_middle::hir::HirBlockId),
    Expression(arandu_middle::hir::HirExprId),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_staged_root<'db, 'prog>(
    db: &'db dyn ArandCompilerDb,
    root: CtfeRoot<'db>,
    file: SourceFile,
    owner: SymbolId,
    layout: arandu_middle::DataLayout,
    selector: &RootSelector,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    iterations: &[(u32, crate::ctfe::FrozenConstant, u64)],
    program: &'prog Program,
    body: Option<&'prog Block>,
    selected: super::select::Selected<'prog>,
    span: Span,
    mut initial: arandu_semantics::TypeCheckResult,
    condition: bool,
) -> Result<Arc<CtfeFunction>, BuildFailure> {
    if matches!(selector, RootSelector::PublicComptime(_)) {
        let branches = if let Some(instance) = instance {
            super::super::branches::select_branches(db, file, owner, Some(instance))
        } else {
            super::super::item_static_branches(db, file, owner).clone()
        };
        let arguments =
            super::super::arguments::select_arguments(db, file, owner, instance, &branches);
        let names = Arc::make_mut(&mut initial.resolved);
        names
            .comptime_arguments
            .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
        names.typed_comptime_arguments.extend(
            arguments
                .typed_values
                .iter()
                .map(|(&key, value)| (key, value.clone())),
        );
        initial
            .diagnostics
            .extend(arguments.diagnostics.iter().cloned());
    }
    if matches!(
        selector,
        RootSelector::ConstArgument(_)
            | RootSelector::StaticIfCondition(_)
            | RootSelector::StaticForBound { .. }
            | RootSelector::StaticForDomain(_)
    ) {
        // Stage children of the selected header only. Requesting all body
        // arguments here would re-enter parent-first branch/domain queries.
        let body_span = body.ok_or(BuildFailure::MissingFunction)?.span;
        let arguments = super::select::const_arguments(&program.pool, body_span);
        let nesting = arguments
            .iter()
            .filter(|(outer, _)| super::select::contains(*outer, span))
            .count();
        if nesting > 64 {
            return Err(BuildFailure::Diagnostics(vec![
                arandu_middle::Diagnostic::error(
                    arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                    "computed generic argument nesting exceeds 64 levels",
                    span,
                ),
            ]));
        }
        for (ordinal, &(child, _)) in arguments.iter().enumerate() {
            if child == span || !super::select::contains(span, child) {
                continue;
            }
            let ordinal = u32::try_from(ordinal).map_err(|_| BuildFailure::InvalidRoot)?;
            let selector = instance.map_or(RootSelector::ConstArgument(ordinal), |instance| {
                RootSelector::InInstance {
                    instance: instance.clone(),
                    selector: Box::new(RootSelector::ConstArgument(ordinal)),
                }
            });
            let child_root = CtfeRoot::new_in_context(
                db,
                file,
                owner,
                super::select::in_occurrence(selector, iterations),
                None,
                root.dependency(db).clone(),
            );
            let budget = super::super::public::staging_budget(
                super::super::config::public_budget(db),
                arguments.len(),
                super::super::public::iteration_count(iterations),
            );
            let value =
                super::eval::ctfe_eval_root(db, CtfeRootRequest::new(db, child_root, budget))
                    .as_ref()
                    .map_err(|error| match error {
                        RootEvalError::Build(error) => error.clone(),
                        RootEvalError::Evaluation(error) => BuildFailure::Evaluation(error.kind),
                    })?;
            let names = Arc::make_mut(&mut initial.resolved);
            names.comptime_arguments.insert(
                child.into(),
                match value {
                    arandu_middle::ctfe::ConstValue::Integer(integer) => {
                        integer.to_const_generic().ok()
                    }
                    _ => None,
                },
            );
            if !matches!(value, arandu_middle::ctfe::ConstValue::Integer(integer) if integer.to_const_generic().is_ok())
            {
                names
                    .typed_comptime_arguments
                    .insert(child.into(), value.clone());
            }
        }
    }
    if instance.is_none()
        && (matches!(selector, RootSelector::ConstArgument(_))
            || (matches!(selector, RootSelector::StaticIfCondition(_))
                && initial.type_info.generic_params.contains_key(&owner)))
    {
        // An instance-independent argument in a template is safe to freeze
        // once. A dependent argument needs a future per-instance staging
        // boundary; never evaluate a template parameter as an uninitialized
        // VM local or erase it into an arbitrary constant key.
        let dependency = initial
            .resolved
            .value_refs
            .iter()
            .chain(initial.resolved.type_refs.iter())
            .map(|(key, id)| (*key, *id))
            .chain(
                initial
                    .resolved
                    .expr_symbols
                    .iter()
                    .zip(&program.pool.expr_spans)
                    .filter_map(|(symbol, span)| {
                        symbol.map(|symbol| (arandu_middle::NodeKey::from(*span), symbol))
                    }),
            )
            .filter(|(key, _)| span.start <= key.start && key.end <= span.end)
            .filter_map(|(key, id)| {
                let symbol = initial.symbols.try_get(id)?;
                matches!(
                    symbol.kind,
                    arandu_middle::SymbolKind::ConstParam | arandu_middle::SymbolKind::TypeParam
                )
                .then_some((key, symbol))
            })
            .min_by_key(|(key, _)| (key.start, key.end));
        if let Some((key, symbol)) = dependency {
            return Err(BuildFailure::Diagnostics(vec![
                arandu_middle::Diagnostic::error(arandu_middle::DiagCode::T042UnsupportedComptime,
                    "this compile-time expression depends on template parameters and requires concrete instance staging",
                    Span::new(span.file_id, key.start, key.end))
                    .with_primary_label("this value depends on the generic instance")
                    .with_label(symbol.span, "template parameter declared here")
                    .with_note("instance-independent computed arguments are supported; dependent evaluation requires per-instance staging"),
            ]));
        }
    }
    let mut iteration_symbols = Vec::new();
    for (ordinal, value, _) in iterations {
        let loops = super::select::static_fors(
            &program.pool,
            body.ok_or(BuildFailure::MissingFunction)?.span,
        );
        let statement = loops
            .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
            .ok_or(BuildFailure::InvalidRoot)?;
        let Stmt::For {
            clause: arandu_parser::ForClause::In { bindings, .. },
            body: loop_body,
            ..
        } = statement
        else {
            return Err(BuildFailure::InvalidRoot);
        };
        if !super::select::contains(loop_body.span, span) {
            return Err(BuildFailure::InvalidRoot);
        }
        let binding = bindings.first().ok_or(BuildFailure::InvalidRoot)?;
        let symbol = *initial
            .resolved
            .definitions
            .get(&binding.span.into())
            .ok_or(BuildFailure::InvalidRoot)?;
        let ty = value
            .shape
            .intern(&initial.type_info.type_interner)
            .map_err(|_| BuildFailure::InvalidRoot)?;
        initial.type_info_mut().decl_types.insert(symbol, ty);
        iteration_symbols.push((symbol, value));
    }
    let exempt: Vec<_> = iteration_symbols
        .iter()
        .map(|(symbol, _)| *symbol)
        .collect();
    if let Some(capture) = arandu_typeck::type_checker::check::find_ctfe_runtime_capture_except(
        &initial,
        &program.pool,
        span,
        &exempt,
    ) {
        return Err(BuildFailure::RuntimeCapture(Box::new(
            super::super::CapturedRuntimeValue {
                symbol: capture.symbol,
                name: Arc::from(initial.symbols.get(capture.symbol).name.as_str()),
                use_span: capture.use_span,
                declaration_span: capture.declaration_span,
            },
        )));
    }
    super::super::headers::install_referenced_headers_in_context(
        db,
        &program.pool,
        span,
        &mut initial,
        root.dependency(db),
    )?;
    super::super::globals::install_referenced_globals_in_context(
        db,
        program,
        span,
        &mut initial,
        root.dependency(db),
    )?;
    let diagnostics = initial
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            super::select::contains(span, diagnostic.span)
                || program
                    .imports
                    .iter()
                    .any(|import| super::select::contains(import.span(), diagnostic.span))
        })
        .cloned()
        .collect();
    initial.diagnostics = diagnostics;
    let expected = if condition {
        Some(ArType::Primitive(arandu_middle::types::Primitive::Bool))
    } else {
        root.expected(db).as_ref().map(|ty| match ty {
            RootExpectedType::Unit => ArType::Void,
            RootExpectedType::Primitive(primitive) => ArType::Primitive(*primitive),
            RootExpectedType::Structural(shape) => shape
                .intern(&initial.type_info.type_interner)
                .ok()
                .map(|id| initial.type_info.type_interner.resolve(id))
                .unwrap_or(ArType::Error),
        })
    };
    let substitution = instance
        .map(|instance| super::select::instance_substitution(&initial, instance))
        .transpose()?
        .unwrap_or_default();
    use arandu_typeck::type_checker::check::{check_ctfe_root_with_substitution, CtfeInitialRoot};
    let input = match selected {
        super::select::Selected::Block(block) => CtfeInitialRoot::Block(block),
        super::select::Selected::Expression(expression) => CtfeInitialRoot::Expression(expression),
    };
    let (mut checked, mut typed) = check_ctfe_root_with_substitution(
        &initial,
        &program.pool,
        input,
        expected.clone(),
        crate::passes::database_target_info(db),
        &substitution,
    );
    for _ in 0..super::super::MAX_QUERY_DEPENDENCY_DEPTH {
        match super::super::contracts::freeze_requests(db, &mut checked, root.dependency(db))
            .map_err(BuildFailure::Diagnostics)?
        {
            0 => break,
            _ => {
                (checked, typed) = check_ctfe_root_with_substitution(
                    &checked,
                    &program.pool,
                    input,
                    expected.clone(),
                    crate::passes::database_target_info(db),
                    &substitution,
                );
            }
        }
    }
    if !checked.type_info.header_requests.is_empty() {
        checked.diagnostics.push(arandu_middle::Diagnostic::error(
            arandu_middle::DiagCode::T045ComptimeLimitExceeded,
            "concrete header discovery exceeds its continuation limit",
            span,
        ));
    }
    if checked
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(BuildFailure::Diagnostics(checked.diagnostics));
    }
    db.unwind_if_revision_cancelled();
    let mut hir = super::super::globals::declaration_context(db, file, program, &mut checked)?;
    super::super::globals::link_referenced_globals(db, program, span, &mut checked, &mut hir)?;
    let mut lowered = match selected {
        super::select::Selected::Block(block) => {
            let block =
                arandu_semantics::lower_block_to_hir(&mut checked, &program.pool, &mut hir, block)
                    .map_err(BuildFailure::Diagnostics)?;
            LoweredRoot::Block(block)
        }
        super::select::Selected::Expression(expression) => {
            let expression = arandu_semantics::lower_expression_to_hir(
                &mut checked,
                &program.pool,
                &mut hir,
                expression,
            )
            .map_err(BuildFailure::Diagnostics)?;
            LoweredRoot::Expression(expression)
        }
    };
    if let Some(instance) = instance {
        let substitution = super::select::instance_substitution(&checked, instance)?;
        lowered = match lowered {
            LoweredRoot::Expression(expression) => LoweredRoot::Expression(
                arandu_semantics::passes::monomorphize::specialize_root_expression(
                    &mut checked,
                    &mut hir,
                    expression,
                    &substitution,
                )
                .map_err(|diagnostic| BuildFailure::Diagnostics(vec![diagnostic]))?,
            ),
            LoweredRoot::Block(block) => LoweredRoot::Block(
                arandu_semantics::passes::monomorphize::specialize_root_block(
                    &mut checked,
                    &mut hir,
                    block,
                    &substitution,
                )
                .map_err(|diagnostic| BuildFailure::Diagnostics(vec![diagnostic]))?,
            ),
        };
    }
    let replacements: Vec<_> = hir
        .pool
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let arandu_middle::hir::HirExprKind::Path { symbol } = expression.kind else {
                return None;
            };
            let (_, frozen) = iteration_symbols
                .iter()
                .find(|(binding, _)| *binding == symbol)?;
            Some((
                arandu_middle::hir::HirExprId::from_usize(index),
                expression.ty,
                expression.span,
                frozen.value.clone(),
            ))
        })
        .collect();
    for (id, ty, span, value) in replacements {
        let replacement = arandu_semantics::passes::lower_hir::materialize_ctfe_value(
            &value,
            ty,
            &checked.type_info,
            &mut hir.pool,
            layout,
            span,
        )
        .map_err(|_| BuildFailure::InvalidRoot)?;
        *hir.pool.expr_mut(id) = replacement;
    }
    super::super::link_extern_headers(db, &mut checked, &mut hir)?;
    let calls = arandu_semantics::passes::monomorphize::specialize_root_callees(
        &mut checked,
        &mut hir,
        match lowered {
            LoweredRoot::Expression(expression) => {
                arandu_semantics::passes::monomorphize::InstantiationRoot::Expression(expression)
            }
            LoweredRoot::Block(block) => {
                arandu_semantics::passes::monomorphize::InstantiationRoot::Block(block)
            }
        },
    )
    .map_err(BuildFailure::Diagnostics)?;
    let global_header = arandu_middle::hir::HirFunc {
        symbol: owner,
        params: hir.pool.alloc_param_list(&[]),
        return_type: typed.return_type,
        body: None,
        span,
        is_async: false,
        no_fallback: false,
    };
    let header = hir
        .decls
        .iter()
        .find_map(|&id| match hir.pool.decl(id) {
            HirDecl::Func(function) if function.symbol == owner => Some(function),
            _ => None,
        })
        .or_else(|| {
            matches!(
                selector,
                RootSelector::GlobalInitializer | RootSelector::HeaderArgument(_)
            )
            .then_some(&global_header)
        })
        .ok_or(BuildFailure::MissingFunction)?;
    db.unwind_if_revision_cancelled();
    let unit = match lowered {
        LoweredRoot::Expression(expression) => {
            arandu_semantics::lower_expression_unit(&checked, &hir, header, expression, layout)
        }
        LoweredRoot::Block(block) => arandu_semantics::lower_block_unit(
            &checked,
            &hir,
            header,
            block,
            typed.return_type,
            typed.value_tail.is_some(),
            layout,
        ),
    }
    .map_err(BuildFailure::Diagnostics)?;
    CtfeFunction::new_with_provider(
        Arc::unwrap_or_clone(unit.function),
        unit.literals,
        &checked.type_info.type_interner,
        layout,
        checked.type_info.as_ref(),
    )
    .and_then(|unit| {
        unit.bind_instance(
            instance
                .cloned()
                .unwrap_or(arandu_middle::types::FunctionInstance {
                    definition: owner,
                    arguments: Vec::new(),
                }),
            calls,
        )
    })
    .map(Arc::new)
    .map_err(BuildFailure::Evaluation)
}
