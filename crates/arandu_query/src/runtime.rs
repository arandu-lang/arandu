//! Function-local runtime staging. Declaration HIR carries call modes,
//! constants and nominal metadata; it never lowers a sibling function body.

use crate::ctfe::item_staged_typing as item_typing;
use crate::db::{FingerprintEq, HashEq};
use crate::passes::{declaration_signatures, item_source_input, PreparedHir};
use crate::{ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::types::FunctionInstance;
use arandu_middle::{Diagnostic, Severity, SymbolId};

mod build;
pub(crate) use build::item_staging;
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
    /// [`Self::fingerprint`] without declaration positions; see
    /// [`declaration_surface`].
    pub(crate) surface: blake3::Hash,
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
    /// Positions are relative to the definition's item (RFC 0042): the
    /// template came from the item-local [`function_hir`].
    pub local: bool,
}

#[derive(Debug)]
pub struct RuntimeUnit {
    pub result: Result<arandu_semantics::FunctionUnit, Vec<Diagnostic>>,
    pub context: std::sync::Arc<arandu_semantics::TypeCheckResult>,
    /// Identity of the [`InstanceHir`] that built `context`. The fingerprint
    /// uses it instead of rehashing the declaration closure in `context`.
    context_identity: blake3::Hash,
    pub instances: Vec<(SymbolId, FunctionInstance)>,
    pub generated_symbols: Vec<SymbolId>,
    pub borrow_summary: Option<arandu_middle::types::ReturnBorrowSummary>,
    /// Retains annotated flow for IDE diagnostics even when final validation
    /// fails. It is not publishable executable code; `result` owns that gate.
    pub analysis_function: Option<std::sync::Arc<arandu_middle::amir::AmirFunc>>,
    /// Inherited from [`InstanceHir::local`]: `runtime_program` and the IDE
    /// place the unit's positions at its item before publishing them.
    pub local: bool,
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
        hash.update(&[u8::from(self.local)]);
        hash.finalize()
    }
}

impl StableHash for RuntimeUnit {
    fn stable_hash(&self) -> blake3::Hash {
        self.hash_with_context(b"RuntimeUnit/v1", self.context.stable_hash())
    }
}

impl RuntimeUnit {
    fn into_query_output(
        self,
        db: &dyn ArandCompilerDb,
        kind: &'static str,
    ) -> FingerprintEq<Self> {
        // The fingerprint is the unit's own content with its context named by
        // the instance that built it; when that instance is equal it is
        // backdated and the unit never re-executes, so a content comparison
        // could only hash the context of every re-executed unit.
        let fingerprint = self.fingerprint();
        FingerprintEq::by_fingerprint(db, kind, self, fingerprint)
    }

    /// Input fingerprint: the unit's own content with the context replaced by
    /// the identity of the instance that built it. O(unit), not O(program).
    fn fingerprint(&self) -> blake3::Hash {
        self.hash_with_context(b"RuntimeUnit/fingerprint/v1", self.context_identity)
    }

    fn hash_with_context(&self, tag: &[u8], context: blake3::Hash) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(tag);
        hash.update(context.as_bytes());
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
        hash.update(&[u8::from(self.local)]);
        hash.finalize()
    }
}

fn instance_hir_cycle<'db>(
    db: &'db dyn ArandCompilerDb,
    _id: salsa::Id,
    instance: Instance<'db>,
) -> FingerprintEq<InstanceHir> {
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
    FingerprintEq::exact(InstanceHir {
        artifacts: PreparedHir {
            hir: None,
            type_check: checked,
            diagnostics,
            source_fingerprint: key.stable_hash(),
        },
        function: None,
        instances: Vec::new(),
        generated_symbols: Vec::new(),
        local: false,
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
) -> FingerprintEq<InstanceHir> {
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
) -> FingerprintEq<InstanceHir> {
    instance_hir_cycle(db, id, instance)
}

#[salsa::tracked(cycle_result = instance_ctfe_hir_cycle)]
pub(crate) fn instance_ctfe_hir_in_context<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    context: crate::ctfe::DependencyContext,
) -> FingerprintEq<InstanceHir> {
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
) -> FingerprintEq<RuntimeUnit> {
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
                arandu_semantics::lower_function_unit(
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
    RuntimeUnit {
        result,
        context: std::sync::Arc::new(concrete.artifacts.type_check.clone()),
        context_identity: concrete.stable_hash(),
        instances: concrete.instances.clone(),
        generated_symbols: concrete.generated_symbols.clone(),
        borrow_summary: None,
        analysis_function: None,
        local: concrete.local,
    }
    .into_query_output(db, "runtime_raw_unit")
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

/// [`declaration_hir`] as importers observe it: identified by its surface,
/// without declaration positions. A body that changes length moves every later
/// declaration of its file; importers link these headers by `SymbolId` and
/// pool IDs, never by position, so such an edit must not invalidate them (the
/// same contract as [`crate::passes::exported_symbols`]). Positions of a
/// file's declarations are authoritative only through that file's own
/// queries.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "declaration_surface", file = ?file.file_id(db),
))]
pub fn declaration_surface(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<DeclarationHir> {
    let declarations = declaration_hir(db, file);
    HashEq::with_identity(
        std::sync::Arc::clone(&declarations.value),
        declarations.surface,
    )
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
    declaration_context_from(db, file, declaration_hir(db, file))
}

/// [`declaration_context`] for units in item coordinates (RFC 0042): the
/// file's own headers enter by [`declaration_shape`], without positions, so a
/// body that moves later items leaves it equal.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "local_declaration_context", file = ?file.file_id(db)))]
pub fn local_declaration_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<PreparedHir> {
    declaration_context_from(db, file, declaration_shape(db, file))
}

/// [`declaration_hir`] without the positions of this file: its HIR and symbol
/// spans carry the `Span::new(0, 0, 0)` placeholder and its position-keyed
/// tables are empty. Identified by [`DeclarationHir::surface`].
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "declaration_shape", file = ?file.file_id(db)))]
pub fn declaration_shape(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<DeclarationHir> {
    let declarations = declaration_hir(db, file);
    let file_id = *file.file_id(db);
    let hir = declarations.artifacts.hir.as_ref().map(|source| {
        let mut hir = arandu_middle::hir::HirProgram {
            span: source.span,
            module: source.module.clone(),
            decls: source.decls.clone(),
            pool: source.pool.clone(),
        };
        hir.map_spans(&mut |span| {
            if span.file_id == file_id {
                arandu_middle::Span::new(0, 0, 0)
            } else {
                span
            }
        });
        hir
    });
    let surface = declarations.surface;
    HashEq::with_identity(
        std::sync::Arc::new(DeclarationHir {
            artifacts: PreparedHir {
                hir,
                type_check: crate::passes::without_positions(&declarations.artifacts.type_check),
                diagnostics: declarations.artifacts.diagnostics.clone(),
                source_fingerprint: surface,
            },
            fingerprint: surface,
            surface,
        }),
        surface,
    )
}

fn declaration_context_from(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    root: &HashEq<DeclarationHir>,
) -> HashEq<PreparedHir> {
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
        // Imported headers enter by surface; only this file's own positions
        // belong to its context identity.
        let declarations = if context_file == file {
            root
        } else {
            declaration_surface(db, context_file)
        };
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
) -> FingerprintEq<PreparedHir> {
    if symbol.is_synthetic() {
        return FingerprintEq::from_content(&capture::thread_capture_function_hir(
            db, file, symbol,
        ));
    }
    if let Some(local) = local_function_hir(db, file, symbol) {
        return local;
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
    let source_fingerprint = fingerprint.finalize();
    let mut diagnostics = Vec::new();
    let hir = match arandu_semantics::lower_function_to_hir(&mut checked, &source.program, symbol) {
        Ok(hir) => hir,
        Err(errors) => {
            diagnostics = errors;
            None
        }
    };
    // The output is a deterministic function of the item source, its typing,
    // the module declarations (diagnostics) and the selected symbol.
    let mut identity = blake3::Hasher::new();
    identity.update(b"FunctionHir/fingerprint/v1");
    identity.update(source_fingerprint.as_bytes());
    identity.update(item.stable_hash().as_bytes());
    identity.update(declared.identity().as_bytes());
    identity.update(&symbol.file_id.to_le_bytes());
    identity.update(&symbol.local_id.0.to_le_bytes());
    FingerprintEq::fingerprinted(
        db,
        "function_hir",
        PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint,
        },
        identity.finalize(),
    )
}

/// A [`RuntimeUnit::local`] unit with every position placed in its file:
/// shifted by the start of its definition's item, and its file's symbols at
/// their declared positions. `None` for a unit already in file coordinates.
pub(crate) fn place_unit(
    db: &dyn ArandCompilerDb,
    key: &FunctionInstance,
    unit: &RuntimeUnit,
) -> Option<RuntimeUnit> {
    let (mut placed, at, declared) = place_unit_body(db, key, unit)?;
    // The unit context names this file's symbols without their positions.
    let mut context = unit.context.as_ref().clone();
    context.symbols = std::sync::Arc::new(context.symbols.map_spans(|symbol| {
        declared
            .try_get(symbol.id)
            .map_or_else(|| at.span(symbol.span), |declared| declared.span)
    }));
    context.diagnostics = place_diagnostics(&at, &context.diagnostics);
    // The composed file view merges the unit's expression tables.
    context.type_info_mut().place_at(&at);
    placed.context = std::sync::Arc::new(context);
    placed.local = false;
    placed.analysis_function = unit
        .analysis_function
        .as_ref()
        .map(|function| place_function(&at, function));
    Some(placed)
}

fn place_diagnostics(
    at: &arandu_typeck::type_checker::info::ItemPlacement,
    diagnostics: &[Diagnostic],
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let mut diagnostic = diagnostic.clone();
            crate::passes::for_each_span(&mut diagnostic, |span| *span = at.span(*span));
            diagnostic
        })
        .collect()
}

fn place_function(
    at: &arandu_typeck::type_checker::info::ItemPlacement,
    function: &std::sync::Arc<arandu_middle::amir::AmirFunc>,
) -> std::sync::Arc<arandu_middle::amir::AmirFunc> {
    let mut function = function.as_ref().clone();
    function.map_spans(&mut |span| at.span(span));
    std::sync::Arc::new(function)
}

/// [`place_unit`] of the unit's result only, with its placement and the
/// declared symbols of its file: its context stays in item coordinates (and
/// `local`), for a consumer that places it as it reads it (the composition),
/// and its analysis function as memoized.
pub(crate) fn place_unit_body<'db>(
    db: &'db dyn ArandCompilerDb,
    key: &FunctionInstance,
    unit: &RuntimeUnit,
) -> Option<(
    RuntimeUnit,
    arandu_typeck::type_checker::info::ItemPlacement,
    &'db arandu_middle::SymbolTable,
)> {
    if !unit.local {
        return None;
    }
    let file = db.source_file_by_id(key.definition.file_id)?;
    let (_, at) = crate::passes::item_local_typing(db, file, key.definition).as_ref()?;
    let span = |span| at.span(span);
    let result = match &unit.result {
        Ok(placed) => {
            let mut placed = placed.clone();
            placed.function = place_function(at, &placed.function);
            for block in &mut placed.debug_blocks {
                block.span = span(block.span);
            }
            placed.diagnostics = place_diagnostics(at, &placed.diagnostics);
            Ok(placed)
        }
        Err(diagnostics) => Err(place_diagnostics(at, diagnostics)),
    };
    Some((
        RuntimeUnit {
            result,
            context: std::sync::Arc::clone(&unit.context),
            context_identity: unit.context_identity,
            instances: unit.instances.clone(),
            generated_symbols: unit.generated_symbols.clone(),
            borrow_summary: unit.borrow_summary.clone(),
            analysis_function: unit.analysis_function.clone(),
            local: unit.local,
        },
        at.clone(),
        &crate::passes::declaration_signatures(db, file).symbols,
    ))
}

/// Whether [`function_hir`] of `symbol` is lowered in its item's coordinates.
pub(crate) fn lowers_locally(db: &dyn ArandCompilerDb, file: SourceFile, symbol: SymbolId) -> bool {
    !symbol.is_synthetic()
        && crate::passes::item_context(db, file, symbol).is_some()
        && declaration_shape(db, file).artifacts.hir.is_some()
}

/// [`function_hir`] lowered from the item AST against its
/// [`crate::passes::item_context`]: every position is relative to the start
/// of the item (RFC 0042). `None` when the item has no local context.
fn local_function_hir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> Option<FingerprintEq<PreparedHir>> {
    if !lowers_locally(db, file, symbol) {
        return None;
    }
    let (context, id) = crate::passes::item_context(db, file, symbol).as_ref()?;
    let item = crate::passes::item_ast(db, file, *id);
    let defaults = crate::passes::defaults_program(db, file, symbol)?;
    let program = match defaults {
        Some(program) => &program.program,
        None => match &*item.value {
            Ok(program) => program,
            Err(_) => return None,
        },
    };
    let module = declared_module(db, file);
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"FunctionHir/local/v1");
    fingerprint.update(item.stable_hash().as_bytes());
    if let Some(program) = defaults {
        fingerprint.update(program.stable_hash().as_bytes());
    }
    fingerprint.update(&symbol.file_id.to_le_bytes());
    fingerprint.update(&symbol.local_id.0.to_le_bytes());
    let source_fingerprint = fingerprint.finalize();
    let mut checked = (**context).clone();
    let mut diagnostics = Vec::new();
    let hir = match arandu_semantics::lower_function_to_hir(&mut checked, program, symbol) {
        Ok(hir) => hir.map(|mut hir| {
            // The module declaration is an item of its own.
            hir.module.clone_from(module);
            hir
        }),
        Err(errors) => {
            diagnostics = errors;
            None
        }
    };
    let mut identity = blake3::Hasher::new();
    identity.update(b"FunctionHir/local/fingerprint/v1");
    identity.update(source_fingerprint.as_bytes());
    identity.update(context.stable_hash().as_bytes());
    match module {
        Some(module) => {
            identity.update(&[1]);
            identity.update(module.as_bytes());
        }
        None => {
            identity.update(&[0]);
        }
    }
    Some(FingerprintEq::fingerprinted(
        db,
        "function_hir",
        PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint,
        },
        identity.finalize(),
    ))
}

/// Dotted path of the file's `module` declaration.
#[salsa::tracked(returns(ref))]
fn declared_module(db: &dyn ArandCompilerDb, file: SourceFile) -> Option<String> {
    match &**crate::passes::parse(db, file) {
        Ok(program) => program.module.as_ref().map(|module| module.path.join(".")),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance<'db>(db: &'db crate::DatabaseImpl, file: SourceFile, name: &str) -> Instance<'db> {
        let declarations = crate::passes::declaration_signatures(db, file);
        let parsed = crate::passes::parse(db, file);
        let Ok(program) = &**parsed else {
            panic!("valid source");
        };
        let definition = arandu_semantics::body_item_symbols(program, &declarations.resolved)
            .into_iter()
            .find(|id| declarations.symbols.get(*id).name == name)
            .unwrap_or_else(|| panic!("function `{name}`"));
        Instance::new(
            db,
            file,
            FunctionInstance {
                definition,
                arguments: Vec::new(),
            },
        )
    }

    #[test]
    fn units_inherit_the_instance_context_identity_and_track_imported_headers() {
        use salsa::Setter;

        let mut db = crate::DatabaseImpl::new();
        let library = db.new_file(
            "calc.aru".into(),
            "module calc\npublic func double(value: int): int { return value * 2 }".into(),
        );
        let file = db.new_file(
            "main.aru".into(),
            "import calc\nfunc main(): int { return calc.double(21) }".into(),
        );
        let identity = |db: &crate::DatabaseImpl| {
            let id = instance(db, file, "main");
            let concrete = instance_hir(db, id);
            let raw = runtime_raw_unit(db, id);
            let unit = runtime_unit(db, id);
            assert!(unit.result.is_ok(), "{:?}", unit.result);
            for checked in [&raw, &unit] {
                assert_eq!(checked.context_identity, concrete.stable_hash());
            }
            concrete.stable_hash()
        };
        let before = identity(&db);
        library.set_text(&mut db).to(std::sync::Arc::from(
            "module calc\npublic func double(value: int, extra: int): int { return value * 2 + extra }",
        ));
        file.set_text(&mut db).to(std::sync::Arc::from(
            "import calc\nfunc main(): int { return calc.double(21, 0) }",
        ));
        assert_ne!(identity(&db), before);
    }
}
