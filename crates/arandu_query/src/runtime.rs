//! Function-local runtime staging. Declaration HIR carries call modes,
//! constants and nominal metadata; it never lowers a sibling function body.

use crate::ctfe::item_staged_typing as item_typing;
use crate::db::HashEq;
use crate::passes::{declaration_signatures, item_source_input, PreparedHir};
use crate::{ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::types::FunctionInstance;
use arandu_middle::{Diagnostic, Severity, SymbolId};

mod build;
mod capture;
mod contracts;
mod decl;
mod program;
mod shape;
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
            shape::hash_shape(&mut hash, argument);
        }
        hash.finalize()
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
    build::build_instance_hir(
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
    build::build_instance_hir(db, instance, true, &context)
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
    decl::declaration_hir_from(db, file, false)
}

/// Pre-body staging context. Uses the same declaration lowering and constant
/// checker, but cannot request full resolution, staged bodies or runtime units.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "header_hir", file = ?file.file_id(db)))]
pub fn header_hir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<DeclarationHir> {
    decl::declaration_hir_from(db, file, true)
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
    if symbol.is_synthetic() {
        return capture::thread_capture_function_hir(db, file, symbol);
    }
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
