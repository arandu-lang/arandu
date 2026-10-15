//! Per-function / per-block analysis queries (A1 / F4 granularity).
//!
//! Bodies reuse pure functions from `arandu_mir` — Salsa only memoizes.
//! Source functions consume independently checked runtime units with their own
//! type/symbol domains. Aggregated instance IDs remain a compatibility view.

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::amir::{AmirFunc, AmirRvalue, AmirStmt, BlockId};
use arandu_middle::{DiagCode, Diagnostic, SymbolId};

/// Compact, hash-stable summary of block-local dataflow.
///
/// Full lattices stay inside `arandu_mir`; we memoize counts so HashEq and LSP
/// delta keys stay small.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataflowFacts {
    pub block: BlockId,
    /// Locals live-in at block entry.
    pub live_in_count: u32,
    /// Locals live-out at block exit.
    pub live_out_count: u32,
    /// Definitely-initialized locals at block entry (definite-init IN).
    pub init_in_count: u32,
    /// Moved / maybe-moved locals at block entry (move-checker IN).
    pub moved_in_count: u32,
    /// Statement count in the block (structural fingerprint).
    pub stmt_count: u32,
}

/// Function-wide liveness summary (per-block live-in / live-out cardinalities).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivenessMap {
    pub live_in_counts: Vec<u32>,
    pub live_out_counts: Vec<u32>,
}

/// F2.1/F2.2 compact per-block may-borrow summary (Salsa memo / HashEq).
///
/// Full bitsets + loans live in `arandu_mir::borrow_facts`; the query keeps
/// cardinalities so early-cutoff stays cheap. `*_out` reflects F2.2 live-range
/// kill (loan ends when no reference holder is live).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowFacts {
    pub block: BlockId,
    /// Locals that may be under a shared (`&`) loan at block entry.
    pub shared_in_count: u32,
    /// Locals that may be under an exclusive (`&mut`) loan at block entry.
    pub exclusive_in_count: u32,
    /// Number of `Borrow`/`BorrowMut` sites inside this block.
    pub borrow_sites: u32,
    /// May-borrowed at block exit (F2.2: after holder live-range ends).
    pub shared_out_count: u32,
    pub exclusive_out_count: u32,
}

/// Stable IDE diagnostic (hashable for early cutoff / publish fingerprint).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdeLabel {
    pub file_id: u32,
    pub start: u32,
    pub end: u32,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdeReplacement {
    pub file_id: u32,
    pub start: u32,
    pub end: u32,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdeHint {
    pub message: String,
    pub replacement: Option<IdeReplacement>,
}

/// Stable IDE diagnostic (hashable for early cutoff / publish fingerprint).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdeDiagnostic {
    pub code: String,
    pub severity: u8,
    pub message: String,
    pub primary_label: Option<String>,
    pub file_id: u32,
    pub start: u32,
    pub end: u32,
    pub labels: Vec<IdeLabel>,
    pub notes: Vec<String>,
    pub hints: Vec<IdeHint>,
    /// Owning function, if partitioned.
    pub func: Option<SymbolId>,
    /// Owning block within that function, if partitioned.
    pub block: Option<BlockId>,
}

impl IdeDiagnostic {
    pub fn from_diag(d: &Diagnostic, func: Option<SymbolId>, block: Option<BlockId>) -> Self {
        Self {
            code: d.code.to_string(),
            severity: d.severity as u8,
            message: d.message.clone(),
            primary_label: d.primary_label.as_deref().cloned(),
            file_id: d.span.file_id,
            start: d.span.start,
            end: d.span.end,
            labels: d
                .labels
                .iter()
                .map(|label| IdeLabel {
                    file_id: label.span.file_id,
                    start: label.span.start,
                    end: label.span.end,
                    message: label.message.clone(),
                })
                .collect(),
            notes: d.notes.clone(),
            hints: d
                .hints
                .iter()
                .map(|hint| IdeHint {
                    message: hint.message.clone(),
                    replacement: hint.replacement.as_ref().map(|replacement| IdeReplacement {
                        file_id: replacement.span.file_id,
                        start: replacement.span.start,
                        end: replacement.span.end,
                        new_text: replacement.new_text.clone(),
                    }),
                })
                .collect(),
            func,
            block,
        }
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_func_symbols",
    file = ?file.file_id(db),
))]
pub fn file_func_symbols(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<Vec<SymbolId>> {
    // IDE identities are definitions, not allocations in an executable's
    // aggregate symbol domain. Concrete specializations use Instance keys.
    let declarations = crate::passes::declaration_signatures(db, file);
    let parsed = crate::passes::parse(db, file);
    let mut ids = match &**parsed {
        Ok(program) => arandu_semantics::free_func_symbols(program, &declarations.resolved)
            .into_iter()
            .filter(|symbol| !declarations.type_info.generic_params.contains_key(symbol))
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    ids.sort_by_key(|s| (s.file_id, s.local_id.0));
    HashEq::new(ids)
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "func_amir",
    file = ?file.file_id(db),
    func = ?func_sym,
))]
pub fn func_amir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
) -> HashEq<AmirFunc> {
    if let Some(unit) = source_runtime_unit(db, func_sym) {
        return unit.analysis_function.as_ref().map_or_else(
            || HashEq::new(empty_func(func_sym)),
            |function| HashEq::from_arc(std::sync::Arc::clone(function)),
        );
    }
    // Uninstantiated templates and unknown/synthetic aggregate IDs have no
    // executable body in this source API.
    // Recover without discovering/lowering an entire executable. Callers
    // with concrete instances must use runtime::instance_amir instead.
    HashEq::new(empty_func(func_sym))
}

fn source_runtime_unit(
    db: &dyn ArandCompilerDb,
    symbol: SymbolId,
) -> Option<&HashEq<crate::runtime::RuntimeUnit>> {
    let source = db.source_file_by_id(symbol.file_id)?;
    let declarations = crate::passes::declaration_signatures(db, source);
    if declarations.symbols.try_get(symbol)?.kind != arandu_middle::SymbolKind::Func
        || declarations.type_info.generic_params.contains_key(&symbol)
    {
        return None;
    }
    Some(crate::runtime::runtime_unit(
        db,
        crate::runtime::Instance::new(
            db,
            source,
            arandu_middle::types::FunctionInstance {
                definition: symbol,
                arguments: Vec::new(),
            },
        ),
    ))
}

fn empty_func(symbol: SymbolId) -> AmirFunc {
    use arandu_middle::types::TypeId;
    AmirFunc {
        symbol,
        return_type: TypeId::from_usize(0),
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![],
        blocks: vec![],
        block_params: vec![],
        stmts: Default::default(),
        cfg: Default::default(),
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "liveness_facts",
    file = ?file.file_id(db),
    func = ?func_sym,
))]
pub fn liveness_facts(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
) -> HashEq<LivenessMap> {
    let func = func_amir(db, file, func_sym);
    if func.blocks.is_empty() {
        return HashEq::new(LivenessMap {
            live_in_counts: vec![],
            live_out_counts: vec![],
        });
    }
    let live = arandu_mir::liveness::analyze_local_liveness(func);
    let n = func.blocks.len();
    let mut live_in_counts = Vec::with_capacity(n);
    let mut live_out_counts = Vec::with_capacity(n);
    for i in 0..n {
        let bid = BlockId::from_usize(i);
        live_in_counts.push(live.live_in(bid).len() as u32);
        live_out_counts.push(live.live_out(bid).len() as u32);
    }
    HashEq::new(LivenessMap {
        live_in_counts,
        live_out_counts,
    })
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "block_dataflow_facts",
    file = ?file.file_id(db),
    func = ?func_sym,
    block = ?block,
))]
pub fn block_dataflow_facts(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
    block: BlockId,
) -> HashEq<DataflowFacts> {
    let func = func_amir(db, file, func_sym);
    let live = liveness_facts(db, file, func_sym);
    let i = block.as_usize();
    let live_in_count = live.live_in_counts.get(i).copied().unwrap_or(0);
    let live_out_count = live.live_out_counts.get(i).copied().unwrap_or(0);

    let init_counts = arandu_mir::definite_init::init_in_counts(func);
    let moved_counts = arandu_mir::move_checker::moved_in_counts(func);
    let init_in_count = init_counts.get(i).copied().unwrap_or(0);
    let moved_in_count = moved_counts.get(i).copied().unwrap_or(0);
    let stmt_count = func.blocks.get(i).map(|b| b.statements.len).unwrap_or(0);

    HashEq::new(DataflowFacts {
        block,
        live_in_count,
        live_out_count,
        init_in_count,
        moved_in_count,
        stmt_count,
    })
}

/// Function-wide may-borrow summaries (one pure dataflow run; HashEq early-cutoff).
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "func_borrow_summaries",
    file = ?file.file_id(db),
    func = ?func_sym,
))]
pub fn func_borrow_summaries(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
) -> HashEq<Vec<arandu_mir::borrow_facts::BlockBorrowSummary>> {
    let func = func_amir(db, file, func_sym);
    if func.blocks.is_empty() {
        return HashEq::new(vec![]);
    }
    HashEq::new(arandu_mir::borrow_facts::block_borrow_summaries(func))
}

/// F2.1: may-borrow facts for one basic block (memoized independently).
///
/// Indexes into [`func_borrow_summaries`] so the heavy dataflow runs once per
/// function; the per-block query is the stable key for DX.5 / dependents (M2).
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "block_borrow_facts",
    file = ?file.file_id(db),
    func = ?func_sym,
    block = ?block,
))]
pub fn block_borrow_facts(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
    block: BlockId,
) -> HashEq<BorrowFacts> {
    let summaries = func_borrow_summaries(db, file, func_sym);
    let i = block.as_usize();
    let s = summaries
        .get(i)
        .copied()
        .unwrap_or(arandu_mir::borrow_facts::BlockBorrowSummary {
            shared_in: 0,
            exclusive_in: 0,
            borrow_sites: 0,
            shared_out: 0,
            exclusive_out: 0,
        });
    HashEq::new(BorrowFacts {
        block,
        shared_in_count: s.shared_in,
        exclusive_in_count: s.exclusive_in,
        borrow_sites: s.borrow_sites,
        shared_out_count: s.shared_out,
        exclusive_out_count: s.exclusive_out,
    })
}

/// Counts `item_ide_diagnostics` executions (P3 delta tests).
#[cfg(any(test, debug_assertions))]
pub static ITEM_IDE_DIAGS_EXEC_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(any(test, debug_assertions))]
pub static ITEM_ATTRIBUTE_VALIDATION_EXEC_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Semantic annotation validation for one top-level item.
///
/// The dependency is content-addressed by [`crate::passes::item_source_input`],
/// so changing an annotation on one item does not invalidate sibling items.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_attribute_diagnostics",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_attribute_validation(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> HashEq<Vec<Diagnostic>> {
    #[cfg(any(test, debug_assertions))]
    ITEM_ATTRIBUTE_VALIDATION_EXEC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let item = crate::passes::item_source_input(db, file, item_sym);
    let resolved = crate::passes::resolve(db, file);
    let program = item.program.as_ref();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        let matches = arandu_semantics::primary_def_key(decl)
            .is_some_and(|key| resolved.resolved.definitions.get(&key) == Some(&item_sym))
            || matches!(
                decl,
                arandu_parser::TopLevelDecl::Extern(ext)
                    if ext.members.iter().any(|member| {
                        resolved
                            .resolved
                            .definitions
                            .get(&arandu_middle::NodeKey::from(member.span))
                            == Some(&item_sym)
                    })
            );
        if !matches {
            continue;
        }
        let annotations =
            arandu_semantics::attributes::validate_decl_attributes(decl, &program.pool);
        // Attribute/test signatures depend only on declarations. Consulting
        // flow contracts here would reintroduce a borrow-interface edge into
        // declaration-based body staging through file_typing's attribute pass.
        let signatures = crate::passes::declaration_signatures(db, file);
        let test_validation = arandu_semantics::testing::validate_test_case(
            decl,
            &annotations,
            item_sym,
            signatures.type_info.as_ref(),
        );
        let benchmark_validation = arandu_semantics::testing::validate_benchmark_case(
            decl,
            &annotations,
            item_sym,
            signatures.type_info.as_ref(),
            signatures.symbols.as_ref(),
        );
        diagnostics.extend(annotations.diagnostics);
        diagnostics.extend(test_validation.diagnostics);
        diagnostics.extend(benchmark_validation.diagnostics);
        break;
    }

    diagnostics.sort_by(|a, b| {
        (a.span.start, a.span.end, a.code.as_str(), &a.message).cmp(&(
            b.span.start,
            b.span.end,
            b.code.as_str(),
            &b.message,
        ))
    });
    diagnostics.dedup();
    HashEq::new(diagnostics)
}

/// IDE projection of [`item_attribute_validation`].
#[salsa::tracked]
pub fn item_attribute_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> HashEq<Vec<IdeDiagnostic>> {
    HashEq::new(
        item_attribute_validation(db, file, item_sym)
            .iter()
            .map(|diagnostic| IdeDiagnostic::from_diag(diagnostic, Some(item_sym), None))
            .collect(),
    )
}

/// P3: diagnostics for **one** top-level item (body typeck + AMIR analysis if func).
///
/// Depends on [`crate::ctfe::item_staged_typing`] (fine-grained) and, for functions,
/// [`func_amir`] whose HashEq is content-stable across sibling edits.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_ide_diagnostics",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_ide_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> HashEq<Vec<IdeDiagnostic>> {
    #[cfg(any(test, debug_assertions))]
    ITEM_IDE_DIAGS_EXEC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let body_tc = crate::ctfe::item_staged_typing(db, file, item_sym);
    let runtime = source_runtime_unit(db, item_sym);
    let mut out: Vec<IdeDiagnostic> = body_tc
        .diagnostics
        .iter()
        .map(|d| IdeDiagnostic::from_diag(d, Some(item_sym), None))
        .collect();
    out.extend(
        item_attribute_diagnostics(db, file, item_sym)
            .iter()
            .cloned(),
    );

    // AMIR analysis for function items (empty AmirFunc if not a func / not lowered).
    // Diags are tagged with the real AMIR block of the bad use (honesty: span→block).
    let amir = func_amir(db, file, item_sym);
    if !amir.blocks.is_empty() {
        // Dense TypeIds are local to the producer. Never interpret this body
        // using the declaration interner or the aggregated module's context.
        let aggregate;
        let context = if let Some(unit) = runtime {
            &*unit.context
        } else {
            aggregate = crate::passes::lower_amir(db, file);
            &aggregate.type_check
        };
        let item = crate::passes::item_source_input(db, file, item_sym);
        // `func_amir` is post-promotion, so the original borrow no longer
        // escapes when the ordinary escape checker runs. Preserve G5
        // observability from the explicit GenInsert semantic marker instead
        // of parsing presentation text or depending on a monolithic query.
        for block in &amir.blocks {
            for stmt in amir.block_stmts(block.id) {
                let AmirStmt::Assign {
                    rhs: AmirRvalue::GenInsert { origin, .. },
                    ..
                } = stmt
                else {
                    continue;
                };
                let diagnostic = Diagnostic::note(
                    arandu_middle::DiagCode::O004GenerationalFallback,
                    "generational fallback: compiler-managed owner promotion",
                    *origin,
                )
                .with_label(*origin, "escape crosses the stack-only borrow boundary")
                .with_note(format!(
                    "escape path: source borrow -> AMIR block {} -> GenRef storage",
                    block.id.as_usize()
                ))
                .with_note("the promoted owner remains checked by generation at every access")
                .with_hint(
                    "stack-first alternatives: shorten the borrow, pass the owner, return owned data, or use an explicit heap type",
                );
                let mut diagnostic =
                    IdeDiagnostic::from_diag(&diagnostic, Some(item_sym), Some(block.id));
                diagnostic.hints.push(IdeHint {
                    message: "forbid this implicit fallback in the containing function".into(),
                    replacement: Some(IdeReplacement {
                        file_id: *file.file_id(db),
                        start: item.item_start,
                        end: item.item_start,
                        new_text: "@NoFallback\n".into(),
                    }),
                });
                out.push(diagnostic);
            }
        }
        for (bid, d) in
            arandu_mir::definite_init::check_definite_init_by_block(amir, context.symbols.as_ref())
        {
            out.push(IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid)));
        }
        for (bid, d) in
            arandu_mir::move_checker::check_moves_by_block(amir, context.symbols.as_ref())
        {
            out.push(IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid)));
        }
        for (bid, d) in
            arandu_mir::borrow_check::check_borrows_by_block(amir, context.symbols.as_ref())
        {
            out.push(IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid)));
        }
        // Escape / O004: global `--no-generational-fallback` applies; per-func
        // `@NoFallback` is applied in `lower_to_amir` (HIR flag).
        let no_fallback =
            arandu_base::NO_GENERATIONAL_FALLBACK.load(std::sync::atomic::Ordering::Relaxed);
        for (bid, d) in arandu_mir::escape_analysis::check_escapes_by_block(
            amir,
            context.symbols.as_ref(),
            &context.type_info.type_interner,
            arandu_mir::escape_analysis::EscapeCheckOptions {
                no_fallback,
                // The per-item IDE pass must use the same exported contract as
                // full lowering. Dropping it here made a valid `ref` parameter
                // passthrough reappear as O004/O010 only in the editor.
                return_borrow: runtime
                    .and_then(|unit| unit.borrow_summary.clone())
                    .or_else(|| {
                        context
                            .type_info
                            .return_borrow_summaries
                            .get(&item_sym)
                            .cloned()
                    }),
            },
        ) {
            let mut diagnostic = IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid));
            if d.code == DiagCode::O004GenerationalFallback
                && d.severity == arandu_middle::Severity::Note
                && !no_fallback
            {
                diagnostic.hints.push(IdeHint {
                    message: "forbid this implicit fallback in the containing function".into(),
                    replacement: Some(IdeReplacement {
                        file_id: *file.file_id(db),
                        start: item.item_start,
                        end: item.item_start,
                        new_text: "@NoFallback\n".into(),
                    }),
                });
            }
            out.push(diagnostic);
        }

        // Populate block facts only (do NOT call block_diagnostics — cycle risk).
        for bi in 0..amir.blocks.len() {
            let bid = BlockId::from_usize(bi);
            let _ = block_dataflow_facts(db, file, item_sym, bid);
            let _ = block_borrow_facts(db, file, item_sym, bid);
        }
    }

    if let Some(runtime) = runtime {
        let diagnostics = match &runtime.result {
            Ok(unit) => &unit.diagnostics,
            Err(errors) => errors,
        };
        for diagnostic in diagnostics {
            let mut converted = IdeDiagnostic::from_diag(diagnostic, Some(item_sym), None);
            if diagnostic.code == DiagCode::O004GenerationalFallback
                && diagnostic.severity == arandu_middle::Severity::Note
            {
                let item = crate::passes::item_source_input(db, file, item_sym);
                converted.hints.push(IdeHint {
                    message: "forbid this implicit fallback in the containing function".into(),
                    replacement: Some(IdeReplacement {
                        file_id: *file.file_id(db),
                        start: item.item_start,
                        end: item.item_start,
                        new_text: "@NoFallback\n".into(),
                    }),
                });
            }
            if !out.iter().any(|existing| {
                existing.start == converted.start
                    && existing.end == converted.end
                    && existing.code == converted.code
                    && existing.message == converted.message
            }) {
                out.push(converted);
            }
        }
    }

    out.sort_by(|a, b| {
        (a.start, a.end, &a.code, &a.message).cmp(&(b.start, b.end, &b.code, &b.message))
    });
    out.dedup();
    HashEq::new(out)
}

/// Alias: analysis diags for a function item (same memo as [`item_ide_diagnostics`]).
#[inline]
pub fn func_analysis_diags(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
) -> HashEq<Vec<IdeDiagnostic>> {
    item_ide_diagnostics(db, file, func_sym).clone()
}

/// Diagnostics attributed to one basic block (entry carries item diags until AMIR stmt spans exist).
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "block_diagnostics",
    file = ?file.file_id(db),
    func = ?func_sym,
    block = ?block,
))]
pub fn block_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
    block: BlockId,
) -> HashEq<Vec<IdeDiagnostic>> {
    let _facts = block_dataflow_facts(db, file, func_sym, block);
    let _borrow = block_borrow_facts(db, file, func_sym, block);
    // Body typeck diags live on entry (no AST block ids); AMIR diags filter by block.
    let body_tc = crate::ctfe::item_staged_typing(db, file, func_sym);
    let amir = func_amir(db, file, func_sym);
    let runtime = source_runtime_unit(db, func_sym);
    let aggregate;
    let context = if let Some(unit) = runtime {
        &*unit.context
    } else {
        aggregate = crate::passes::lower_amir(db, file);
        &aggregate.type_check
    };

    let mut out = Vec::new();
    if block.as_usize() == 0 {
        for d in &body_tc.diagnostics {
            out.push(IdeDiagnostic::from_diag(d, Some(func_sym), Some(block)));
        }
        out.extend(
            item_attribute_diagnostics(db, file, func_sym)
                .iter()
                .cloned()
                .map(|mut diagnostic| {
                    diagnostic.block = Some(block);
                    diagnostic
                }),
        );
    }
    if !amir.blocks.is_empty() {
        for (bid, d) in
            arandu_mir::definite_init::check_definite_init_by_block(amir, context.symbols.as_ref())
        {
            if bid == block {
                out.push(IdeDiagnostic::from_diag(&d, Some(func_sym), Some(bid)));
            }
        }
        for (bid, d) in
            arandu_mir::move_checker::check_moves_by_block(amir, context.symbols.as_ref())
        {
            if bid == block {
                out.push(IdeDiagnostic::from_diag(&d, Some(func_sym), Some(bid)));
            }
        }
        for (bid, d) in
            arandu_mir::borrow_check::check_borrows_by_block(amir, context.symbols.as_ref())
        {
            if bid == block {
                out.push(IdeDiagnostic::from_diag(&d, Some(func_sym), Some(bid)));
            }
        }
    }
    HashEq::new(out)
}

/// File-level signature / resolve diagnostics not tied to a body item.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_signature_ide_diagnostics",
    file = ?file.file_id(db),
))]
pub fn file_signature_ide_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<Vec<IdeDiagnostic>> {
    let sigs = crate::passes::declaration_signatures(db, file);
    let mut out: Vec<IdeDiagnostic> = sigs
        .diagnostics
        .iter()
        .map(|d| IdeDiagnostic::from_diag(d, None, None))
        .collect();
    out.sort_by(|a, b| {
        (a.start, a.end, &a.code, &a.message).cmp(&(b.start, b.end, &b.code, &b.message))
    });
    out.dedup();
    HashEq::new(out)
}

/// Full IDE diagnostic set: union of per-item memos + signature-level diags (P3).
///
/// Compose is O(items); each item early-cutoffs independently via
/// [`item_ide_diagnostics`]. LSP still publishes a full list (protocol), but
/// recomputation is incremental.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_ide_diagnostics",
    file = ?file.file_id(db),
))]
pub fn file_ide_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<Vec<IdeDiagnostic>> {
    let program_res = crate::passes::parse(db, file);
    let signatures = crate::passes::declaration_signatures(db, file);

    let mut out: Vec<IdeDiagnostic> = Vec::new();
    let mut covered = std::collections::HashSet::new();

    if let Ok(program) = &**program_res {
        let items = arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref());
        for &item_sym in &items {
            let diags = item_ide_diagnostics(db, file, item_sym);
            for d in diags.iter() {
                let key = (d.start, d.end, d.code.clone(), d.message.clone());
                if covered.insert(key) {
                    out.push(d.clone());
                }
            }
        }
    } else {
        // Parse failed: never show an empty Problems panel. Surface the full
        // recovering-parse diagnostics (lex + syntax) instead of nothing.
        let parse_diags = crate::passes::parse_diagnostics(db, file);
        for d in parse_diags.iter() {
            let diag = IdeDiagnostic::from_diag(d, None, None);
            let key = (
                diag.start,
                diag.end,
                diag.code.clone(),
                diag.message.clone(),
            );
            if covered.insert(key) {
                out.push(diag);
            }
        }
    }

    // Signature / resolve diags (imports, duplicate types, …).
    let sig_diags = file_signature_ide_diagnostics(db, file);
    for d in sig_diags.iter() {
        let key = (d.start, d.end, d.code.clone(), d.message.clone());
        if covered.insert(key) {
            out.push(d.clone());
        }
    }

    // Lowering accumulators include ownership diagnostics produced before an
    // invalid function can be published as AMIR, plus pre-promotion O004/O010
    // evidence. Preserve them at the file composition boundary; otherwise the
    // CLI rejects O002/O003/O006 while the IDE incorrectly reports a clean file.
    let _ = crate::passes::lower_amir(db, file);
    let lower_diags = crate::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    let item_ranges = crate::passes::syntax_tree(db, file).item_ranges();
    for accumulated in lower_diags {
        if !matches!(
            accumulated.0.code,
            arandu_middle::DiagCode::O002MoveWhileBorrowed
                | arandu_middle::DiagCode::O003MutableBorrowConflict
                | arandu_middle::DiagCode::O004GenerationalFallback
                | arandu_middle::DiagCode::O006DestroyWhileBorrowed
                | arandu_middle::DiagCode::O010EscapeOfBorrowedValue
        ) {
            continue;
        }
        let mut diagnostic = IdeDiagnostic::from_diag(&accumulated.0, None, None);
        if accumulated.0.code == arandu_middle::DiagCode::O004GenerationalFallback
            && accumulated.0.severity == arandu_middle::Severity::Note
        {
            if let Some((start, _)) = item_ranges
                .iter()
                .copied()
                .find(|(start, end)| *start <= diagnostic.start && diagnostic.end <= *end)
            {
                diagnostic.hints.push(IdeHint {
                    message: "forbid this implicit fallback in the containing function".into(),
                    replacement: Some(IdeReplacement {
                        file_id: *file.file_id(db),
                        start,
                        end: start,
                        new_text: "@NoFallback\n".into(),
                    }),
                });
            }
        }
        let key = (
            diagnostic.start,
            diagnostic.end,
            diagnostic.code.clone(),
            diagnostic.message.clone(),
        );
        if covered.insert(key) {
            out.push(diagnostic);
        }
    }

    out.sort_by(|a, b| {
        (a.start, a.end, &a.code, &a.message).cmp(&(b.start, b.end, &b.code, &b.message))
    });
    HashEq::new(out)
}

/// Fingerprint of the full IDE diagnostic list (for LSP publish skip).
#[must_use]
pub fn ide_diags_fingerprint(diags: &[IdeDiagnostic]) -> blake3::Hash {
    use crate::stable_hash::StableHash;
    diags.to_vec().stable_hash()
}

/// Per-item diagnostic fingerprint (P3 LSP cache key).
#[must_use]
pub fn item_ide_diags_fingerprint(diags: &[IdeDiagnostic]) -> blake3::Hash {
    ide_diags_fingerprint(diags)
}
