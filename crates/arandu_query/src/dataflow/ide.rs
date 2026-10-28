use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::amir::{AmirRvalue, AmirStmt, BlockId};
use arandu_middle::{DiagCode, Diagnostic, SymbolId};

#[cfg(any(test, debug_assertions))]
use super::ITEM_IDE_DIAGS_EXEC_COUNT;
use super::{
    analysis_amir, block_borrow_facts, block_dataflow_facts, file_signature_ide_diagnostics,
    item_attribute_diagnostics, item_flow_diagnostics, item_ide_diagnostics, memoized_source_unit,
    source_runtime_unit, IdeDiagnostic, IdeHint, IdeReplacement, ItemFlow,
};

pub(super) fn collect_item_ide_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> HashEq<Vec<IdeDiagnostic>> {
    #[cfg(any(test, debug_assertions))]
    ITEM_IDE_DIAGS_EXEC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let body_tc = crate::ctfe::item_staged_typing(db, file, item_sym);
    let runtime = source_runtime_unit(db, item_sym);
    let runtime = runtime.as_deref();
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

    // Flow checks run on the memoized unit; only their positions are placed
    // here (RFC 0042), so a moved item does not check its body again.
    let flow = item_flow_diagnostics(db, file, item_sym);
    let at = if flow.local {
        crate::passes::item_local_typing(db, file, item_sym)
            .as_ref()
            .map(|(_, at)| *at)
    } else {
        None
    };
    let mut item_start = None;
    for (index, diagnostic) in flow.diagnostics.iter().enumerate() {
        let mut diagnostic = diagnostic.clone();
        if let Some(at) = at {
            diagnostic.place(at);
        }
        if flow.fallback_fixes.contains(&(index as u32)) {
            let start = *item_start.get_or_insert_with(|| {
                crate::passes::item_source_input(db, file, item_sym).item_start
            });
            diagnostic.hints.push(fallback_fix(db, file, start));
        }
        out.push(diagnostic);
    }
    // Populate block facts only (do NOT call block_diagnostics — cycle risk).
    for bi in 0..analysis_amir(db, file, item_sym).blocks.len() {
        let bid = BlockId::from_usize(bi);
        let _ = block_dataflow_facts(db, file, item_sym, bid);
        let _ = block_borrow_facts(db, file, item_sym, bid);
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
                let start = *item_start.get_or_insert_with(|| {
                    crate::passes::item_source_input(db, file, item_sym).item_start
                });
                converted.hints.push(fallback_fix(db, file, start));
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

pub(super) fn collect_file_ide_diagnostics(
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

fn fallback_fix(db: &dyn ArandCompilerDb, file: SourceFile, item_start: u32) -> IdeHint {
    IdeHint {
        message: "forbid this implicit fallback in the containing function".into(),
        replacement: Some(IdeReplacement {
            file_id: *file.file_id(db),
            start: item_start,
            end: item_start,
            new_text: "@NoFallback\n".into(),
        }),
    }
}

/// Definite-init, move, borrow and escape checks of one source function, in
/// the coordinates of its memoized unit.
pub(super) fn collect_item_flow(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> HashEq<ItemFlow> {
    let mut flow = ItemFlow::default();
    let amir = analysis_amir(db, file, item_sym);
    if amir.blocks.is_empty() {
        return HashEq::new(flow);
    }
    let runtime = memoized_source_unit(db, item_sym).map(|(_, unit)| unit);
    let runtime = runtime.as_deref();
    flow.local = runtime.is_some_and(|unit| unit.local);
    // Dense TypeIds are local to the producer. Never interpret this body
    // using the declaration interner or the aggregated module's context.
    let aggregate;
    let context = if let Some(unit) = runtime {
        &*unit.context
    } else {
        aggregate = crate::passes::lower_amir(db, file);
        &aggregate.type_check
    };
    let out = &mut flow.diagnostics;
    // `func_amir` is post-promotion, so the original borrow no longer
    // escapes when the ordinary escape checker runs. Preserve G5
    // observability from the explicit GenInsert semantic marker instead
    // of parsing presentation text or depending on a monolithic query.
    // Dormant for implicit promotion until R1: the pipeline no longer
    // promotes escapes (RFC 0001 amendment), so no GenInsert reaches here.
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
            flow.fallback_fixes.push(out.len() as u32);
            out.push(IdeDiagnostic::from_diag(
                &diagnostic,
                Some(item_sym),
                Some(block.id),
            ));
        }
    }
    for (bid, d) in
        arandu_mir::definite_init::check_definite_init_by_block(amir, context.symbols.as_ref())
    {
        out.push(IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid)));
    }
    for (bid, d) in arandu_mir::move_checker::check_moves_by_block(amir, context.symbols.as_ref()) {
        out.push(IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid)));
    }
    for (bid, d) in arandu_mir::borrow_check::check_borrows_by_block_with_types(
        amir,
        context.symbols.as_ref(),
        &context.type_info.type_interner,
    ) {
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
        if d.code == DiagCode::O004GenerationalFallback
            && d.severity == arandu_middle::Severity::Note
            && !no_fallback
        {
            flow.fallback_fixes.push(out.len() as u32);
        }
        out.push(IdeDiagnostic::from_diag(&d, Some(item_sym), Some(bid)));
    }
    HashEq::new(flow)
}
