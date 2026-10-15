//! Function-local runtime staging. Declaration HIR carries call modes,
//! constants and nominal metadata; it never lowers a sibling function body.

use crate::ctfe::item_staged_typing as item_typing;
use crate::db::HashEq;
use crate::passes::{declaration_signatures, item_source_input, parse, PreparedHir};
use crate::{ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::types::{FunctionInstance, TypeShape};
use arandu_middle::{Diagnostic, Severity, SymbolId};

mod contracts;
mod program;
pub use contracts::{instance_contracts, runtime_unit, InstanceContracts};
pub use program::{runtime_program, RuntimeProgram};

#[derive(Debug)]
pub struct DeclarationHir {
    pub artifacts: PreparedHir,
    pub(crate) fingerprint: blake3::Hash,
}

#[salsa::interned]
#[derive(Debug)]
pub struct Instance<'db> {
    pub file: SourceFile,
    #[returns(ref)]
    pub key: FunctionInstance,
}

#[derive(Debug)]
pub struct InstanceHir {
    pub artifacts: PreparedHir,
    pub function: Option<SymbolId>,
    pub instances: Vec<(SymbolId, FunctionInstance)>,
    pub generated_symbols: Vec<SymbolId>,
}

#[derive(Debug)]
pub struct RuntimeUnit {
    pub result: Result<arandu_mir::FunctionUnit, Vec<Diagnostic>>,
    pub context: std::sync::Arc<arandu_semantics::TypeCheckResult>,
    pub instances: Vec<(SymbolId, FunctionInstance)>,
    pub generated_symbols: Vec<SymbolId>,
    pub borrow_summary: Option<arandu_middle::types::ReturnBorrowSummary>,
    /// Retains annotated flow for IDE diagnostics even when final validation
    /// fails. It is not publishable executable code; `result` owns that gate.
    pub analysis_function: Option<std::sync::Arc<arandu_middle::amir::AmirFunc>>,
}

/// Concrete analysis identities never depend on synthetic symbols allocated
/// by executable composition. Types/literals remain in runtime_unit's domain.
#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "instance_amir")
)]
pub fn instance_amir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
) -> Option<HashEq<arandu_middle::amir::AmirFunc>> {
    runtime_unit(db, instance)
        .analysis_function
        .as_ref()
        .map(|function| HashEq::from_arc(std::sync::Arc::clone(function)))
}

impl StableHash for FunctionInstance {
    fn stable_hash(&self) -> blake3::Hash {
        // A canonical fresh interner is not sufficient: identical outer ranges
        // may contain different children. Hash the full structural arguments.
        let mut hash = blake3::Hasher::new();
        hash.update(b"FunctionInstance/v1");
        hash.update(&self.definition.file_id.to_le_bytes());
        hash.update(&self.definition.local_id.0.to_le_bytes());
        hash.update(&(self.arguments.len() as u64).to_le_bytes());
        for argument in &self.arguments {
            hash_shape(&mut hash, argument);
        }
        hash.finalize()
    }
}

fn hash_shape(hash: &mut blake3::Hasher, shape: &TypeShape) {
    use TypeShape::*;
    let tag = match shape {
        Primitive(_) => 0,
        Named(_, _) => 1,
        Func(_, _) => 2,
        Nullable(_) => 3,
        Slice(_) => 4,
        Array(_, _) => 5,
        ConstArray(_, _) => 6,
        Const(_) => 7,
        FrozenConst(_) => 24,
        ConstParam(_) => 8,
        Ptr(_) => 9,
        Ref(_) => 10,
        RefMut(_) => 11,
        GenRef => 12,
        Tuple(_) => 13,
        Result(_, _) => 14,
        Option(_) => 15,
        Coroutine(_) => 16,
        Poll(_) => 17,
        Range(_) => 18,
        Err => 19,
        Void => 20,
        IntLiteral => 21,
        FloatLiteral => 22,
        Error => 23,
    };
    hash.update(&[tag]);
    match shape {
        Primitive(primitive) => {
            hash.update(&[*primitive as u8]);
        }
        Named(symbol, arguments) => {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash.update(&(arguments.len() as u64).to_le_bytes());
            for argument in arguments {
                hash_shape(hash, argument);
            }
        }
        Func(arguments, result) => {
            hash.update(&(arguments.len() as u64).to_le_bytes());
            for argument in arguments {
                hash_shape(hash, argument);
            }
            hash_shape(hash, result);
        }
        Tuple(arguments) => {
            hash.update(&(arguments.len() as u64).to_le_bytes());
            for argument in arguments {
                hash_shape(hash, argument);
            }
        }
        Array(length, inner) => {
            hash.update(&length.to_le_bytes());
            hash_shape(hash, inner);
        }
        ConstArray(symbol, inner) => {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash_shape(hash, inner);
        }
        FrozenConst(value) => {
            hash.update(&value.canonical_bytes());
        }
        Const(value) => {
            hash.update(&value.to_le_bytes());
        }
        ConstParam(symbol) => {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
        }
        Nullable(inner) | Slice(inner) | Ptr(inner) | Ref(inner) | RefMut(inner)
        | Option(inner) | Coroutine(inner) | Poll(inner) | Range(inner) => hash_shape(hash, inner),
        Result(ok, error) => {
            hash_shape(hash, ok);
            hash_shape(hash, error);
        }
        GenRef | Err | Void | IntLiteral | FloatLiteral | Error => {}
    }
}

impl StableHash for InstanceHir {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"InstanceHir/v1");
        hash.update(self.artifacts.stable_hash().as_bytes());
        if let Some(function) = self.function {
            hash.update(&[1]);
            hash.update(&function.file_id.to_le_bytes());
            hash.update(&function.local_id.0.to_le_bytes());
        } else {
            hash.update(&[0]);
        }
        for (symbol, key) in &self.instances {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash.update(key.stable_hash().as_bytes());
        }
        for symbol in &self.generated_symbols {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
        }
        hash.finalize()
    }
}

impl StableHash for RuntimeUnit {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"RuntimeUnit/v1");
        hash.update(self.context.stable_hash().as_bytes());
        match &self.result {
            Ok(unit) => {
                hash.update(&[1]);
                hash.update(unit.function.stable_hash().as_bytes());
                for literal in &unit.literals.entries {
                    crate::stable_hash::hash_literal_entry(&mut hash, literal);
                }
                hash.update(unit.diagnostics.stable_hash().as_bytes());
                hash.update(&[u8::from(unit.no_fallback)]);
                for block in &unit.debug_blocks {
                    hash.update(&(block.block.as_usize() as u64).to_le_bytes());
                    hash.update(&block.span.file_id.to_le_bytes());
                    hash.update(&block.span.start.to_le_bytes());
                    hash.update(&block.span.end.to_le_bytes());
                }
                for binding in &unit.debug_bindings {
                    hash.update(&(binding.temp.as_usize() as u64).to_le_bytes());
                    hash.update(&(binding.local.as_usize() as u64).to_le_bytes());
                }
            }
            Err(diagnostics) => {
                hash.update(&[0]);
                hash.update(diagnostics.stable_hash().as_bytes());
            }
        }
        for (symbol, key) in &self.instances {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash.update(key.stable_hash().as_bytes());
        }
        for symbol in &self.generated_symbols {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
        }
        if let Some(summary) = &self.borrow_summary {
            hash.update(&[1]);
            crate::stable_hash::hash_return_borrow_summary(&mut hash, summary);
        } else {
            hash.update(&[0]);
        }
        if let Some(function) = &self.analysis_function {
            hash.update(&[1]);
            hash.update(function.stable_hash().as_bytes());
        } else {
            hash.update(&[0]);
        }
        hash.finalize()
    }
}

fn instance_hir_cycle<'db>(
    db: &'db dyn ArandCompilerDb,
    _id: salsa::Id,
    instance: Instance<'db>,
) -> HashEq<InstanceHir> {
    // Recovery may run while headers and the selected root are themselves
    // active. Read only the interned identity, never ask another body/header
    // query for a "better" fallback and recreate the same dependency cycle.
    let key = instance.key(db);
    let diagnostics = vec![Diagnostic::error(
        arandu_middle::DiagCode::T044ComptimeEvaluationFailed,
        "function instantiation depends on its own compile-time selection",
        arandu_middle::Span::new(key.definition.file_id, 0, 0),
    )
    .with_primary_label("cyclic compile-time dependency")];
    let mut checked = arandu_semantics::TypeCheckResult::empty();
    checked.diagnostics = diagnostics.clone();
    HashEq::new(InstanceHir {
        artifacts: PreparedHir {
            hir: None,
            type_check: checked,
            diagnostics,
            source_fingerprint: key.stable_hash(),
        },
        function: None,
        instances: Vec::new(),
        generated_symbols: Vec::new(),
    })
}

#[salsa::tracked(cycle_result = instance_hir_cycle)]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "instance_hir")
)]
pub fn instance_hir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
) -> HashEq<InstanceHir> {
    build_instance_hir(
        db,
        instance,
        false,
        &crate::ctfe::DependencyContext::default(),
    )
}

/// Runtime-independent instance lowering used by the CTFE call closure.
fn instance_ctfe_hir_cycle<'db>(
    db: &'db dyn ArandCompilerDb,
    id: salsa::Id,
    instance: Instance<'db>,
    _context: crate::ctfe::DependencyContext,
) -> HashEq<InstanceHir> {
    instance_hir_cycle(db, id, instance)
}

#[salsa::tracked(cycle_result = instance_ctfe_hir_cycle)]
pub(crate) fn instance_ctfe_hir_in_context<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    context: crate::ctfe::DependencyContext,
) -> HashEq<InstanceHir> {
    build_instance_hir(db, instance, true, &context)
}

fn build_instance_hir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    for_ctfe: bool,
    context: &crate::ctfe::DependencyContext,
) -> HashEq<InstanceHir> {
    let key = instance.key(db);
    let source = item_source_input(db, *instance.file(db), key.definition);
    let has_static_loop = if source.may_have_comptime && key.arguments.is_empty() {
        let mut body_span = None;
        source.program.for_each_decl_recursive(|_, declaration| {
            if arandu_semantics::item_source_span(declaration).start == source.item_start {
                if let arandu_parser::TopLevelDecl::Func(function) = declaration {
                    body_span = Some(function.body.span);
                }
            }
        });
        body_span.is_some_and(|body| {
            source.program.pool.stmts.iter().any(|stmt| {
                matches!(
                    stmt,
                    arandu_parser::Stmt::For {
                        is_comptime: true,
                        ..
                    }
                ) && body.start <= stmt.span().start
                    && stmt.span().end <= body.end
            })
        })
    } else {
        false
    };
    let dependent_header = !for_ctfe
        && !key.arguments.is_empty()
        && crate::passes::declaration_signatures(db, *instance.file(db))
            .type_info
            .deferred_headers
            .contains(&key.definition);
    let staged = dependent_header
        || (source.may_have_comptime && (!key.arguments.is_empty() || has_static_loop));
    let template = if for_ctfe {
        crate::ctfe::instances::instance_ctfe_hir(db, instance, context)
    } else if staged {
        crate::ctfe::instance_staged_hir(db, instance)
    } else {
        HashEq::share(function_hir(db, *instance.file(db), key.definition))
    };
    let mut checked = template.type_check.clone();
    let mut hir = template.hir.as_ref().map(|source| {
        let mut hir = arandu_middle::hir::HirProgram {
            span: source.span,
            module: source.module.clone(),
            decls: Vec::new(),
            pool: arandu_middle::hir::HirPool::new(),
        };
        arandu_semantics::link_hir_module(&mut checked, &mut hir, &template.type_check, source);
        hir
    });
    let mut diagnostics = template.diagnostics.clone();
    let mut function = None;
    let mut instances = Vec::new();
    let mut generated_symbols = Vec::new();
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(template.stable_hash().as_bytes());
    fingerprint.update(key.stable_hash().as_bytes());
    if let Some(hir) = &mut hir {
        db.unwind_if_revision_cancelled();
        // Header linking is memoized per module closure, not repeated in the
        // retained function HIR and then copied again for every instance.
        if for_ctfe {
            match crate::ctfe::globals::declaration_context(
                db,
                *instance.file(db),
                &source.program,
                &mut checked,
            ) {
                Ok(context) => {
                    let metadata = checked.clone();
                    arandu_semantics::link_hir_module(&mut checked, hir, &metadata, &context);
                    let program = parse(db, *instance.file(db));
                    if let Ok(program) = &**program {
                        let mut span = None;
                        program.for_each_decl_recursive(|_, declaration| {
                            if arandu_semantics::primary_def_key(declaration)
                                .and_then(|node| checked.resolved.definitions.get(&node))
                                == Some(&key.definition)
                            {
                                span = Some(arandu_semantics::item_source_span(declaration));
                            }
                        });
                        if let Some(span) = span {
                            if let Err(error) = crate::ctfe::globals::link_referenced_globals(
                                db,
                                program,
                                span,
                                &mut checked,
                                hir,
                            ) {
                                crate::ctfe::public::append_failure(
                                    &mut diagnostics,
                                    crate::ctfe::RootEvalError::Build(error),
                                    span,
                                );
                            }
                        }
                    }
                }
                Err(error) => crate::ctfe::public::append_failure(
                    &mut diagnostics,
                    crate::ctfe::RootEvalError::Build(error),
                    hir.span,
                ),
            }
        } else {
            let headers = declaration_context(db, *instance.file(db));
            fingerprint.update(headers.stable_hash().as_bytes());
            if let Some(context) = &headers.hir {
                arandu_semantics::link_hir_module(&mut checked, hir, &headers.type_check, context);
            } else {
                diagnostics.extend(headers.diagnostics.iter().cloned());
            }
        }
        // A concrete argument may come from the caller's module, not from the
        // generic definition's imports (e.g. Vec<String>). Bring only its
        // declaration context, never that module's sibling bodies.
        let mut files = std::collections::BTreeSet::new();
        for argument in &key.arguments {
            if argument
                .for_each_symbol(|symbol| {
                    if checked.symbols.try_get(symbol).is_none() {
                        files.insert(symbol.file_id);
                    }
                })
                .is_err()
            {
                diagnostics.push(Diagnostic::error(
                    arandu_middle::DiagCode::G002GenericInstantiationLimit,
                    "instance arguments exceed the structural type bounds",
                    hir.span,
                ));
            }
        }
        let mut pending = files.into_iter().collect::<Vec<_>>();
        let mut visited = rustc_hash::FxHashSet::default();
        while let Some(file_id) = pending.pop() {
            db.unwind_if_revision_cancelled();
            if !visited.insert(file_id) {
                continue;
            }
            let Some(context_file) = db.source_file_by_id(file_id) else {
                continue;
            };
            if for_ctfe {
                let parsed = crate::passes::parse(db, context_file);
                let Ok(program) = &**parsed else {
                    continue;
                };
                match crate::ctfe::globals::declaration_context(
                    db,
                    context_file,
                    program,
                    &mut checked,
                ) {
                    Ok(context) => {
                        let metadata = checked.clone();
                        arandu_semantics::link_hir_module(&mut checked, hir, &metadata, &context);
                    }
                    Err(error) => crate::ctfe::public::append_failure(
                        &mut diagnostics,
                        crate::ctfe::RootEvalError::Build(error),
                        hir.span,
                    ),
                }
                continue;
            }
            let declarations = declaration_hir(db, context_file);
            fingerprint.update(declarations.stable_hash().as_bytes());
            if let Some(context) = &declarations.artifacts.hir {
                arandu_semantics::link_hir_module(
                    &mut checked,
                    hir,
                    &declarations.artifacts.type_check,
                    context,
                );
            } else {
                diagnostics.extend(declarations.artifacts.diagnostics.iter().cloned());
            }
            for (_, imported) in crate::passes::module_import_edges(db, context_file)
                .iter()
                .rev()
            {
                if let Some(file_id) = imported {
                    pending.push(*file_id);
                }
            }
        }
        // Narrow CTFE declaration contexts contain local headers only. Retain
        // demanded imported intrinsic declarations before monomorphization;
        // otherwise a layout query becomes an unavailable external call.
        if for_ctfe {
            if let Err(error) = crate::ctfe::link_extern_headers(db, &mut checked, hir) {
                crate::ctfe::public::append_failure(
                    &mut diagnostics,
                    crate::ctfe::RootEvalError::Build(error),
                    hir.span,
                );
            }
        }
        match arandu_semantics::passes::monomorphize::instantiate_function(&mut checked, hir, key) {
            Ok(concrete) => {
                function = Some(concrete.function);
                instances = concrete.instances;
                generated_symbols = concrete.generated_symbols;
                if staged {
                    generated_symbols.extend(if for_ctfe {
                        crate::ctfe::instances::instance_ctfe_symbols(db, instance, context)
                    } else {
                        crate::ctfe::instance_staged_symbols(db, instance)
                    });
                    generated_symbols
                        .sort_unstable_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
                    generated_symbols.dedup();
                }
            }
            Err(errors) => diagnostics.extend(errors),
        }
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        hir = None;
    }
    HashEq::new(InstanceHir {
        artifacts: PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint: fingerprint.finalize(),
        },
        function,
        instances,
        generated_symbols,
    })
}

#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "runtime_raw_unit")
)]
pub fn runtime_raw_unit<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
) -> HashEq<RuntimeUnit> {
    let concrete = instance_hir(db, instance);
    let result = match (&concrete.artifacts.hir, concrete.function) {
        (Some(hir), Some(symbol)) => hir
            .decls
            .iter()
            .find_map(|&id| match hir.pool.decl(id) {
                arandu_middle::hir::HirDecl::Func(function)
                    if function.symbol == symbol && function.body.is_some() =>
                {
                    Some(function)
                }
                _ => None,
            })
            .ok_or_else(|| {
                vec![Diagnostic::ice(
                    arandu_middle::DiagCode::ICEL001,
                    "concrete unit has no selected function",
                    hir.span,
                )]
            })
            .and_then(|function| {
                arandu_mir::lower_function_unit(
                    &concrete.artifacts.type_check,
                    hir,
                    function,
                    *db.target_config().data_layout(db),
                )
            }),
        _ => {
            let mut diagnostics = concrete.artifacts.diagnostics.clone();
            if diagnostics.is_empty() {
                diagnostics.push(Diagnostic::ice(
                    arandu_middle::DiagCode::ICEL001,
                    "runtime instance has no typed function body",
                    arandu_middle::Span::new(instance.key(db).definition.file_id, 0, 0),
                ));
            }
            Err(diagnostics)
        }
    };
    HashEq::new(RuntimeUnit {
        result,
        context: std::sync::Arc::new(concrete.artifacts.type_check.clone()),
        instances: concrete.instances.clone(),
        generated_symbols: concrete.generated_symbols.clone(),
        borrow_summary: None,
        analysis_function: None,
    })
}

impl StableHash for DeclarationHir {
    fn stable_hash(&self) -> blake3::Hash {
        self.fingerprint
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "declaration_hir", file = ?file.file_id(db),
))]
pub fn declaration_hir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<DeclarationHir> {
    declaration_hir_from(db, file, false)
}

/// Pre-body staging context. Uses the same declaration lowering and constant
/// checker, but cannot request full resolution, staged bodies or runtime units.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "header_hir", file = ?file.file_id(db)))]
pub fn header_hir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<DeclarationHir> {
    declaration_hir_from(db, file, true)
}

fn declaration_hir_from(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    headers_only: bool,
) -> HashEq<DeclarationHir> {
    let declared = if headers_only {
        crate::passes::header_signatures(db, file)
    } else {
        declaration_signatures(db, file)
    };
    let parsed = parse(db, file);
    let mut checked = (**declared).clone();
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"DeclarationHir/v1");
    let mut hir = None;
    let mut diagnostics = Vec::new();
    if let Ok(program) = &**parsed {
        let mut bodies = Vec::new();
        program.for_each_decl_recursive(|_, declaration| {
            if let arandu_parser::TopLevelDecl::Func(function) = declaration {
                bodies.push(function.body.span);
            }
        });
        // Resolution visits all bodies. Their errors belong to their own
        // function units, not to the declaration-only HIR context.
        checked.diagnostics.retain(|diagnostic| {
            !bodies.iter().any(|span| {
                diagnostic.span.file_id == span.file_id
                    && diagnostic.span.start >= span.start
                    && diagnostic.span.end <= span.end
            })
        });
        program.for_each_decl_recursive(|_, declaration| {
            if !matches!(declaration, arandu_parser::TopLevelDecl::Const(_)) {
                return;
            }
            if let Some(symbol) = arandu_semantics::primary_def_key(declaration)
                .and_then(|key| checked.resolved.definitions.get(&key))
                .copied()
            {
                if headers_only {
                    let mut item = arandu_semantics::check_item_body_only(
                        declared,
                        program,
                        symbol,
                        crate::passes::database_target_info(db),
                    );
                    crate::ctfe::globals::stage_initializer(
                        &mut item,
                        program,
                        symbol,
                        *db.target_config().data_layout(db),
                    );
                    checked.type_info_mut().merge_from(&item.type_info);
                    checked.diagnostics.extend(item.diagnostics);
                } else {
                    let item = item_typing(db, file, symbol);
                    checked.type_info_mut().merge_from(&item.type_info);
                    checked.diagnostics.extend(item.diagnostics.iter().cloned());
                }
                fingerprint.update(item_source_input(db, file, symbol).stable_hash().as_bytes());
            }
        });
        let text = file.text(db);
        for import in &program.imports {
            let span = import.span();
            if let Some(source) = text.get(span.start as usize..span.end as usize) {
                fingerprint.update(source.as_bytes());
            }
        }
        match arandu_semantics::lower_declarations_to_hir(&mut checked, program) {
            Ok(declarations) => {
                for &id in &declarations.decls {
                    let declaration = declarations.pool.decl(id);
                    let span = declaration.span();
                    fingerprint.update(&span.file_id.to_le_bytes());
                    fingerprint.update(&span.start.to_le_bytes());
                    fingerprint.update(&span.end.to_le_bytes());
                    if let arandu_middle::hir::HirDecl::Func(function) = declaration {
                        fingerprint
                            .update(&[u8::from(function.is_async), u8::from(function.no_fallback)]);
                        for parameter in declarations.pool.params_list(function.params) {
                            let receiver = match parameter.receiver_kind {
                                None => 0,
                                Some(arandu_middle::hir::ReceiverKind::Shared) => 1,
                                Some(arandu_middle::hir::ReceiverKind::Mut) => 2,
                                Some(arandu_middle::hir::ReceiverKind::Own) => 3,
                            };
                            fingerprint.update(&[u8::from(parameter.is_receiver), receiver]);
                        }
                    }
                }
                hir = Some(declarations);
            }
            Err(errors) => diagnostics = errors,
        }
    }
    fingerprint.update(crate::stable_hash::type_signature_hash(&checked).as_bytes());
    fingerprint.update(diagnostics.stable_hash().as_bytes());
    fingerprint.update(&[u8::from(hir.is_some())]);
    let fingerprint = fingerprint.finalize();
    HashEq::new(DeclarationHir {
        artifacts: PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint: fingerprint,
        },
        fingerprint,
    })
}

/// Shared body-free declaration closure. Body edits can revalidate individual
/// declarations but cut off before rebuilding this linked header context.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "declaration_context", file = ?file.file_id(db)))]
pub fn declaration_context(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<PreparedHir> {
    let root = declaration_hir(db, file);
    let mut checked = root.artifacts.type_check.clone();
    let mut hir = root
        .artifacts
        .hir
        .as_ref()
        .map(|source| arandu_middle::hir::HirProgram {
            span: source.span,
            module: source.module.clone(),
            decls: Vec::new(),
            pool: arandu_middle::hir::HirPool::new(),
        });
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"DeclarationContext/v1");
    let mut diagnostics = Vec::new();
    let mut visited = rustc_hash::FxHashSet::default();
    let mut pending = vec![file];
    while let Some(context_file) = pending.pop() {
        db.unwind_if_revision_cancelled();
        if !visited.insert(*context_file.file_id(db)) {
            continue;
        }
        let declarations = declaration_hir(db, context_file);
        fingerprint.update(declarations.stable_hash().as_bytes());
        diagnostics.extend(declarations.artifacts.diagnostics.iter().cloned());
        if let (Some(dest), Some(source)) = (&mut hir, &declarations.artifacts.hir) {
            arandu_semantics::link_hir_module(
                &mut checked,
                dest,
                &declarations.artifacts.type_check,
                source,
            );
        }
        for (_, imported) in crate::passes::module_import_edges(db, context_file)
            .iter()
            .rev()
        {
            if let Some(imported) = imported.and_then(|id| db.source_file_by_id(id)) {
                pending.push(imported);
            }
        }
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        hir = None;
    }
    HashEq::new(PreparedHir {
        hir,
        type_check: checked,
        diagnostics,
        source_fingerprint: fingerprint.finalize(),
    })
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "function_hir", file = ?file.file_id(db), function = ?symbol,
))]
pub fn function_hir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<PreparedHir> {
    let source = item_source_input(db, file, symbol);
    let item = item_typing(db, file, symbol);
    let declared = declaration_signatures(db, file);
    let mut checked = (**item).clone();
    let mut span = None;
    source.program.for_each_decl_recursive(|_, declaration| {
        if arandu_semantics::primary_def_key(declaration)
            .and_then(|key| checked.resolved.definitions.get(&key))
            == Some(&symbol)
        {
            span = Some(arandu_semantics::item_source_span(declaration));
        }
    });
    checked.diagnostics.extend(
        declared
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                span.is_some_and(|span| {
                    diagnostic.span.file_id == span.file_id
                        && diagnostic.span.start >= span.start
                        && diagnostic.span.end <= span.end
                }) || source.program.imports.iter().any(|import| {
                    let span = import.span();
                    diagnostic.span.file_id == span.file_id
                        && diagnostic.span.start >= span.start
                        && diagnostic.span.end <= span.end
                })
            })
            .cloned(),
    );
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"FunctionHir/v1");
    fingerprint.update(source.stable_hash().as_bytes());
    let mut diagnostics = Vec::new();
    let hir = match arandu_semantics::lower_function_to_hir(&mut checked, &source.program, symbol) {
        Ok(hir) => hir,
        Err(errors) => {
            diagnostics = errors;
            None
        }
    };
    HashEq::new(PreparedHir {
        hir,
        type_check: checked,
        diagnostics,
        source_fingerprint: fingerprint.finalize(),
    })
}
