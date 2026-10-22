use crate::BitSet;
use crate::amir::{AmirFunc, AmirPlace, BlockId, LocalId, TempId};
use crate::borrow_facts::{FuncBorrowFacts, ProgramPoint};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::{Span, SymbolTable};

fn local_name(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> String {
    func.locals
        .get(local.as_usize())
        .and_then(|l| l.symbol)
        .map_or_else(
            || format!("_{}", local.as_usize()),
            |sym| symbols.get(sym).name.to_string(),
        )
}

pub(super) fn local_span(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> Span {
    let Some(l) = func.locals.get(local.as_usize()) else {
        return Span::new(0, 0, 0);
    };
    if let Some(u) = l.use_span
        && u.start != u.end
    {
        return u;
    }
    if l.span.start != l.span.end {
        return l.span;
    }
    if let Some(sym) = l.symbol {
        let s = symbols.get(sym).span;
        if s.start != s.end {
            return s;
        }
    }
    Span::new(0, 0, 0)
}

fn first_loan_span(
    local: LocalId,
    borrowed_place: Option<&AmirPlace>,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    func: &AmirFunc,
    symbols: &SymbolTable,
    current_block: BlockId,
) -> Span {
    for loan in &facts.loans {
        if loan.place_local != local {
            continue;
        }
        if let Some(place) = borrowed_place
            && !super::places::places_may_overlap(
                place.local,
                &place.projections,
                loan.place_local,
                &loan.place_projections,
            )
        {
            continue;
        }
        let Some(loan_index) = facts
            .loans
            .iter()
            .position(|candidate| std::ptr::eq(candidate, loan))
        else {
            continue;
        };
        if !super::places::loan_holders_live(
            loan_index,
            loan,
            live,
            facts,
            ProgramPoint {
                block: current_block,
                stmt_index: facts
                    .local_holders_at
                    .get(current_block.as_usize())
                    .map_or(0, |points| points.len().saturating_sub(1)),
            },
        ) {
            continue;
        }
        // Prefer a holder temp span.
        for t in loan.holder_temps.iter() {
            if live.contains(t)
                && let Some(temp) = func.temps.get(t.as_usize())
                && temp.span.start != temp.span.end
            {
                return temp.span;
            }
        }
    }
    local_span(local, func, symbols)
}

pub(super) fn move_while_borrowed_diag(
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    current_block: BlockId,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_span(local, func, symbols);
    let origin = first_loan_span(local, None, facts, live, func, symbols, current_block);
    Diagnostic::error(
        DiagCode::O002MoveWhileBorrowed,
        format!("cannot move '{name}' while borrowed"),
        span,
    )
    .with_label(origin, "borrow is still active from here")
    .with_note("end all uses of the reference before moving the owner")
}

pub(super) fn destroy_diag(
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    current_block: BlockId,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_span(local, func, symbols);
    let origin = first_loan_span(local, None, facts, live, func, symbols, current_block);
    Diagnostic::error(
        DiagCode::O006DestroyWhileBorrowed,
        format!("cannot destroy '{name}' while borrowed"),
        span,
    )
    .with_label(origin, "borrow is still active from here")
    .with_note("ending the owner while a reference is live would create a dangling pointer")
}

pub(super) fn conflict_diag(
    place: &AmirPlace,
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    _prefix: &str,
    note: &str,
    current_block: BlockId,
) -> Diagnostic {
    let local = place.local;
    let name = local_name(local, func, symbols);
    let span = local_span(local, func, symbols);
    let origin = first_loan_span(
        local,
        Some(place),
        facts,
        live,
        func,
        symbols,
        current_block,
    );
    Diagnostic::error(
        DiagCode::O003MutableBorrowConflict,
        format!("mutable borrow conflict on '{name}'"),
        span,
    )
    .with_label(origin, "previous borrow is still active")
    .with_note(note)
}
