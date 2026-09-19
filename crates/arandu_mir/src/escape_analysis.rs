//! F2.3 — Escape analysis + O004 generational-fallback notes (G2 fused).
//!
//! ## Decision rule (design gold)
//!
//! ```text
//! if live_range(ref) is bound inside this function's CFG
//!    (does not escape via return / heap / closure):
//!     → static borrow window (F2.2); no O004
//! else:
//!     → generational / escape path:
//!         • always emit O004 (inspectable — never silent magic)
//!         • return of `ref local` → also O010 (hard error; cannot gen-fix a stack frame)
//!         • with @NoFallback / --no-generational-fallback → O004 is Error (G2)
//! ```
//!
//! G2 is **not** a silent global “strict mode”: it only promotes O004 notes to
//! errors in scopes that opt in (function attribute or CLI flag).
//!
//! Market context (Vale-style gen refs): objects carry a generation; refs
//! remember it; free bumps generation. Arandu keeps that as a **controlled
//! fallback** after static OSSA fails — stack-first by default, inspectable
//! when the compiler must leave the pure static world.

use crate::NO_GENERATIONAL_FALLBACK;
use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::borrow_facts::analyze_borrow_facts;
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::types::ArType;
use crate::{Span, SymbolTable};
use arandu_middle::types::ReturnBorrowSummary;
use std::sync::atomic::Ordering;

/// How a reference escapes the current function's static window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeKind {
    /// Returning a reference derived from a stack local.
    Return,
    /// Storing a stack-derived ref into memory that outlives the frame (heap /
    /// aggregate) — candidate for generational fallback.
    HeapStore,
}

#[derive(Debug, Clone)]
pub struct EscapeEvent {
    pub kind: EscapeKind,
    pub place_local: LocalId,
    pub span: Span,
    pub block: BlockId,
    /// Human reason for Magia Inspecionável / `arandu explain`.
    pub reason: &'static str,
}

/// Options for one function's escape check.
#[derive(Debug, Clone, Default)]
pub struct EscapeCheckOptions {
    /// From `@NoFallback` on the function (G2 opt-in).
    pub no_fallback: bool,
    /// Provenance exported by type checking for a borrowed return.
    pub return_borrow: Option<ReturnBorrowSummary>,
}

impl EscapeCheckOptions {
    #[must_use]
    pub fn effective_no_fallback(&self) -> bool {
        self.no_fallback || NO_GENERATIONAL_FALLBACK.load(Ordering::Relaxed)
    }
}

/// Run escape analysis and produce O004 / O010 diagnostics.
pub fn check_escapes(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
) -> Vec<Diagnostic> {
    check_escapes_by_block(func, symbols, interner, opts)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_escapes`], tagged with AMIR block.
#[must_use]
pub fn check_escapes_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() {
        return Vec::new();
    }

    let events = find_escapes(func, interner);
    let no_fb = opts.effective_no_fallback();
    let mut diags = Vec::new();

    for ev in events {
        if return_escape_is_declared(&ev, opts.return_borrow.as_ref()) {
            continue;
        }
        let name = local_name(ev.place_local, func, symbols);
        match ev.kind {
            EscapeKind::Return => {
                // Returning &local is always a hard error (O010). O004 explains
                // the static-window failure (Magia Inspecionável).
                diags.push((
                    ev.block,
                    Diagnostic::error(
                        DiagCode::O010EscapeOfBorrowedValue,
                        format!(
                            "escape of borrowed value: returning reference to local variable '{name}'"
                        ),
                        ev.span,
                    )
                    .with_label(ev.span, "reference to local would dangle after return")
                    .with_note(ev.reason)
                    .with_note(
                        "keep the owner alive in the caller, or return owned data instead of `ref`",
                    ),
                ));
                diags.push((
                    ev.block,
                    o004_diag(&name, ev.span, ev.block, ev.reason, /*as_error*/ no_fb),
                ));
            }
            EscapeKind::HeapStore => {
                // Controlled fallback path: O004 is Note by default; G2 promotes to Error.
                // Projected refs cannot be represented by the current scalar
                // handle without either snapshot semantics or losing the base
                // owner/path relation. Reject instead of compiling stale data.
                let projected = has_projected_borrow(func, ev.place_local);
                diags.push((
                    ev.block,
                    o004_diag(
                        &name,
                        ev.span,
                        ev.block,
                        ev.reason,
                        /*as_error*/ no_fb || projected,
                    )
                    .with_note(if projected {
                        "projected escaping borrows require a first-class owner/path handle; snapshot promotion is intentionally forbidden"
                    } else {
                        "compiler-managed owner promotion is available for this root place"
                    }),
                ));
                if no_fb {
                    // Extra hint only when hard-failing.
                }
            }
        }
    }

    diags
}

fn return_escape_is_declared(event: &EscapeEvent, summary: Option<&ReturnBorrowSummary>) -> bool {
    event.kind == EscapeKind::Return
        && summary.is_some_and(|summary| {
            summary.dependencies.iter().any(|dependency| {
                dependency.sources.iter().any(|source| {
                    usize::try_from(source.parameter_index)
                        .is_ok_and(|index| event.place_local.as_usize() == index)
                })
            })
        })
}

fn has_projected_borrow(func: &AmirFunc, local: LocalId) -> bool {
    func.blocks
        .iter()
        .flat_map(|block| func.block_stmts(block.id))
        .any(|stmt| {
            matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                    ..
                } if place.local == local && !place.projections.is_empty()
            )
        })
}

fn o004_diag(name: &str, span: Span, block: BlockId, reason: &str, as_error: bool) -> Diagnostic {
    let msg = format!("generational fallback: '{name}' escapes stack-limited borrow window");
    let d = if as_error {
        Diagnostic::error(DiagCode::O004GenerationalFallback, msg, span).with_note(
            "this scope forbids generational fallback (`@NoFallback` or `--no-generational-fallback`)",
        )
    } else {
        Diagnostic::note(DiagCode::O004GenerationalFallback, msg, span).with_note(
            "not a silent heap promotion: this note records why the static borrow window was insufficient",
        )
    };
    d.with_label(span, "escape crosses the stack-only borrow boundary")
        .with_note(format!(
            "escape path: local `{name}` -> AMIR block {} -> storage that may outlive the borrow",
            block.as_usize()
        ))
        .with_note(reason.to_string())
        .with_hint(
            "stack-first alternatives: shorten the borrow, pass the owner, return owned data, or use an explicit heap type",
        )
}

/// Pure escape finder (no diagnostics).
#[must_use]
pub fn find_escapes(func: &AmirFunc, interner: &crate::types::TypeInterner) -> Vec<EscapeEvent> {
    let facts = analyze_borrow_facts(func);
    // Temp → place_local for stack-derived refs (from loans + propagation).
    let mut temp_to_place: Vec<Option<LocalId>> = vec![None; func.temps.len()];
    for loan in &facts.loans {
        for t in loan.holder_temps.iter() {
            let i = t.as_usize();
            if i < temp_to_place.len() {
                temp_to_place[i] = Some(loan.place_local);
            }
        }
    }

    let mut enum_construct_temps = rustc_hash::FxHashSet::default();
    // Also walk primary Borrow assigns and aggregate constructions
    // that incorporate stack references.
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } = stmt
                {
                    let i = lhs.as_usize();
                    if i < temp_to_place.len() && temp_to_place[i].is_none() {
                        temp_to_place[i] = Some(place.local);
                        changed = true;
                    }
                }
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::RelativeBorrow { local, .. },
                } = stmt
                {
                    let i = lhs.as_usize();
                    if i < temp_to_place.len() && temp_to_place[i].is_none() {
                        temp_to_place[i] = Some(*local);
                        changed = true;
                    }
                }
                if let AmirStmt::Assign { lhs, rhs } = stmt {
                    let i = lhs.as_usize();
                    match rhs {
                        AmirRvalue::Use(op) => {
                            if i < temp_to_place.len()
                                && temp_to_place[i].is_none()
                                && let Some(p) = operand_stack_place(op, &temp_to_place)
                            {
                                temp_to_place[i] = Some(p);
                                changed = true;
                            }
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
                                && enum_construct_temps.contains(t)
                                && enum_construct_temps.insert(*lhs)
                            {
                                changed = true;
                            }
                        }
                        AmirRvalue::StructLiteral { fields, .. } => {
                            if i < temp_to_place.len() && temp_to_place[i].is_none() {
                                for (_, op) in fields {
                                    if let Some(p) = operand_stack_place(op, &temp_to_place) {
                                        temp_to_place[i] = Some(p);
                                        changed = true;
                                        break;
                                    }
                                }
                            }
                        }
                        AmirRvalue::Tuple { items } | AmirRvalue::Array { items } => {
                            if i < temp_to_place.len() && temp_to_place[i].is_none() {
                                for op in items {
                                    if let Some(p) = operand_stack_place(op, &temp_to_place) {
                                        temp_to_place[i] = Some(p);
                                        changed = true;
                                        break;
                                    }
                                }
                            }
                        }
                        AmirRvalue::EnumConstruct { payload, .. } => {
                            if enum_construct_temps.insert(*lhs) {
                                changed = true;
                            }
                            if i < temp_to_place.len()
                                && temp_to_place[i].is_none()
                                && let Some(op) = payload
                                && let Some(p) = operand_stack_place(op, &temp_to_place)
                            {
                                temp_to_place[i] = Some(p);
                                changed = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let mut events = Vec::new();
    for block in &func.blocks {
        // Return of stack-derived ref.
        if matches!(block.terminator, AmirTerminator::Return) {
            // Return value is typically stored into local/temp _0 (return slot).
            // Look for last assigns to return temp (params[0] or TempId(0)).
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Use(op),
                } = stmt
                    && is_return_temp(*lhs, func)
                    && let Some(place) = operand_stack_place(op, &temp_to_place)
                {
                    events.push(EscapeEvent {
                        kind: EscapeKind::Return,
                        place_local: place,
                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                        block: block.id,
                        reason: "reference escapes via return of the current function",
                    });
                }
                // Direct: return temp = Borrow(local)
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } = stmt
                    && is_return_temp(*lhs, func)
                {
                    events.push(EscapeEvent {
                        kind: EscapeKind::Return,
                        place_local: place.local,
                        span: temp_span(*lhs, func).unwrap_or_else(|| place_span(place, func)),
                        block: block.id,
                        reason: "reference escapes via return of the current function",
                    });
                }
                // Return of aggregate containing stack ref directly assigned to return temp
                if let AmirStmt::Assign { lhs, rhs } = stmt
                    && is_return_temp(*lhs, func)
                {
                    match rhs {
                        AmirRvalue::StructLiteral { fields, .. } => {
                            for (_, op) in fields {
                                if let Some(place) = operand_stack_place(op, &temp_to_place) {
                                    events.push(EscapeEvent {
                                        kind: EscapeKind::Return,
                                        place_local: place,
                                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                                        block: block.id,
                                        reason: "reference escapes via return of the current function",
                                    });
                                    break;
                                }
                            }
                        }
                        AmirRvalue::Tuple { items } | AmirRvalue::Array { items } => {
                            for op in items {
                                if let Some(place) = operand_stack_place(op, &temp_to_place) {
                                    events.push(EscapeEvent {
                                        kind: EscapeKind::Return,
                                        place_local: place,
                                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                                        block: block.id,
                                        reason: "reference escapes via return of the current function",
                                    });
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Heap / aggregate store of stack-derived ref.
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Store { lhs, rhs } = stmt {
                let Some(place) = operand_stack_place(rhs, &temp_to_place) else {
                    continue;
                };
                // Storing into a projected place or memory local ⇒ may outlive pure SSA.
                let dest_is_memory = !lhs.projections.is_empty()
                    || func
                        .locals
                        .get(lhs.local.as_usize())
                        .is_some_and(|l| l.is_memory);
                let rhs_is_enum = match rhs {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => enum_construct_temps.contains(t),
                    _ => false,
                };
                let dest_is_ref_slot = lhs.projections.is_empty()
                    && (rhs_is_enum
                        || func.locals.get(lhs.local.as_usize()).is_some_and(|l| {
                            match interner.resolve(l.ty) {
                                ArType::Ref(_) | ArType::RefMut(_) => true,
                                ArType::Option(inner) | ArType::Nullable(inner) => {
                                    matches!(
                                        interner.resolve(inner),
                                        ArType::Ref(_) | ArType::RefMut(_)
                                    )
                                }
                                ArType::Result(ok, err) => {
                                    matches!(
                                        interner.resolve(ok),
                                        ArType::Ref(_) | ArType::RefMut(_)
                                    ) || matches!(
                                        interner.resolve(err),
                                        ArType::Ref(_) | ArType::RefMut(_)
                                    )
                                }
                                _ => false,
                            }
                        }));
                if dest_is_memory && !dest_is_ref_slot {
                    events.push(EscapeEvent {
                        kind: EscapeKind::HeapStore,
                        place_local: place,
                        span: local_span(place, func),
                        block: block.id,
                        reason: "reference is stored into memory that may outlive the stack frame (generational fallback candidate)",
                    });
                }
            }
        }
    }

    // Dedup by (kind, place, block).
    events.sort_by_key(|e| (e.block.as_usize(), e.place_local.as_usize(), e.kind as u8));
    events.dedup_by_key(|e| (e.block, e.place_local, e.kind));
    events
}

fn is_return_temp(t: TempId, func: &AmirFunc) -> bool {
    t.as_usize() == 0 || func.params.first().is_some_and(|p| *p == t)
}

fn operand_stack_place(op: &AmirOperand, temp_to_place: &[Option<LocalId>]) -> Option<LocalId> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
            temp_to_place.get(t.as_usize()).copied().flatten()
        }
        _ => None,
    }
}

fn local_name(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> String {
    func.locals
        .get(local.as_usize())
        .and_then(|l| l.symbol)
        .map_or_else(
            || format!("_{}", local.as_usize()),
            |sym| symbols.get(sym).name.to_string(),
        )
}

fn local_span(local: LocalId, func: &AmirFunc) -> Span {
    func.locals
        .get(local.as_usize())
        .map(|l| l.span)
        .unwrap_or_else(|| Span::new(0, 0, 0))
}

fn temp_span(t: TempId, func: &AmirFunc) -> Option<Span> {
    func.temps.get(t.as_usize()).map(|t| t.span)
}

fn place_span(place: &crate::amir::AmirPlace, func: &AmirFunc) -> Span {
    local_span(place.local, func)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::SymbolId;
    use crate::amir::{AmirBasicBlock, AmirLocal, AmirPlace, AmirStmtTable, AmirTemp};
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::types::{ArType, Primitive, TypeInterner};
    use smallvec::smallvec;

    fn place(l: usize) -> AmirPlace {
        AmirPlace {
            local: LocalId::from_usize(l),
            projections: smallvec![],
        }
    }

    fn empty_symbols() -> SymbolTable {
        SymbolTable::new(0)
    }

    #[test]
    fn return_ref_to_local_is_o010() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        // t0 = &s0  (return slot is t0? use assign to return temp 0)
        // Actually: t1 = &s0; _0 = use t1
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let block = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 2),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let blocks = vec![block];
        let cfg = compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: ref_int,
            receiver: None,
            params: vec![],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 10, 11),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 12, 13),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions::default(),
        );
        assert!(
            diags
                .iter()
                .any(|d| d.code == DiagCode::O010EscapeOfBorrowedValue),
            "expected O010, got {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.code == DiagCode::O004GenerationalFallback),
            "expected O004 note, got {diags:?}"
        );
        assert!(
            diags
                .iter()
                .filter(|d| d.code == DiagCode::O004GenerationalFallback)
                .all(|d| d.severity == crate::diagnostics::Severity::Note),
            "O004 default must be Note, not silent Error"
        );
    }

    #[test]
    fn no_fallback_promotes_o004_to_error() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let block = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 2),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let blocks = vec![block];
        let cfg = compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: ref_int,
            receiver: None,
            params: vec![],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 10, 11),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 12, 13),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions {
                no_fallback: true,
                ..EscapeCheckOptions::default()
            },
        );
        let o004: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::O004GenerationalFallback)
            .collect();
        assert!(!o004.is_empty());
        assert!(
            o004.iter()
                .all(|d| d.severity == crate::diagnostics::Severity::Error),
            "G2 must promote O004 to Error, got {o004:?}"
        );
    }

    #[test]
    fn return_ref_to_param_remains_rejected_without_interprocedural_provenance() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        // t1 = &s0 (where s0 is param 0); _0 = use t1
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let block = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 2),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let blocks = vec![block];
        let cfg = compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: ref_int,
            receiver: None,
            // 1 parameter (TempId(2)) corresponding to local 0
            params: vec![TempId::from_usize(2)],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 10, 11),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 12, 13),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 5, 6),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions::default(),
        );
        assert_eq!(
            diags
                .iter()
                .filter(|diag| diag.code == DiagCode::O010EscapeOfBorrowedValue)
                .count(),
            1,
            "borrowed parameter return must fail closed until callers inherit its loan: {diags:?}"
        );

        let declared = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions {
                no_fallback: false,
                return_borrow: Some(ReturnBorrowSummary::direct(
                    0,
                    arandu_middle::types::BorrowKind::Shared,
                )),
            },
        );
        assert!(
            declared.is_empty(),
            "a declared parameter dependency should make the return safe: {declared:?}"
        );
    }

    #[test]
    fn escape_via_struct_literal_detected_as_heap_store() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let struct_ty = interner.intern(ArType::named(SymbolId::new(0, 1), &[], &interner));
        let void = interner.intern(ArType::Void);

        let mut stmts = AmirStmtTable::new();
        // t1 = &local0
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        // t2 = StructLiteral { ref: t1 }
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: SymbolId::new(0, 1),
                fields: vec![(
                    smol_str::SmolStr::new("ptr"),
                    AmirOperand::Copy(TempId::from_usize(1)),
                )],
            },
        });
        // local1 (memory) = store t2
        stmts.push(AmirStmt::Store {
            lhs: place(1),
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });

        let block = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 3),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let blocks = vec![block];
        let cfg = compute_cfg_edges(&blocks);

        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: void,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: struct_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 2, 3),
                    use_span: None,
                },
            ],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: void,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: struct_ty,
                    is_copy: false,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions {
                no_fallback: true,
                ..EscapeCheckOptions::default()
            },
        );

        let o004: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::O004GenerationalFallback)
            .collect();
        assert!(
            !o004.is_empty(),
            "storing aggregate containing stack borrow into memory must trigger O004: {diags:?}"
        );
    }
}
