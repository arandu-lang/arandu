//! M2 — Borrow conflict diagnostics (O002 / O003 / O006).
//!
//! Pure checks over F2.1/F2.2 facts: no second analysis engine.

#![allow(clippy::too_many_arguments)]
//!
//! | Code | Rule |
//! |------|------|
//! | **O002** | Move/consume of a local while a loan of that local is active |
//! | **O003** | Conflicting loans (any `&mut` overlap) or mutation under an active loan |
//! | **O006** | `Destroy` of a place while a loan is still active |
//!
//! Shared+shared overlap is allowed. Messages use source vocabulary (name + spans),
//! not internal lattice jargon.

mod diag;
mod liveness;
mod places;
mod reborrow;
mod stmt;

use crate::SymbolTable;
use crate::amir::{AmirFunc, BlockId};
use crate::borrow_facts::analyze_borrow_facts;
use crate::diagnostics::Diagnostic;

#[cfg(test)]
use crate::Span;
#[cfg(test)]
use crate::amir::{AmirOperand, AmirProjection, AmirRvalue, AmirStmt, LocalId, TempId};
#[cfg(test)]
use crate::diagnostics::DiagCode;
#[cfg(test)]
use places::places_may_overlap;

/// Run M2 borrow checks; return diagnostics (no block tags).
pub fn check_borrows(func: &AmirFunc, symbols: &SymbolTable) -> Vec<Diagnostic> {
    check_borrows_by_block(func, symbols)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_borrows`], tagged with the AMIR block of the violation.
#[must_use]
pub fn check_borrows_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() || func.locals.is_empty() {
        return Vec::new();
    }
    stmt::check_borrows_from_facts(func, symbols, analyze_borrow_facts(func), None)
}

/// Type-aware checks do not prolong a loan through an independent scalar read.
pub fn check_borrows_with_types(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
) -> Vec<Diagnostic> {
    check_borrows_by_block_with_types(func, symbols, interner)
        .into_iter()
        .map(|(_, diagnostic)| diagnostic)
        .collect()
}

#[must_use]
pub fn check_borrows_by_block_with_types(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() || func.locals.is_empty() {
        return Vec::new();
    }
    stmt::check_borrows_from_facts(
        func,
        symbols,
        crate::borrow_facts::analyze_borrow_facts_with_types(func, interner),
        Some(interner),
    )
}
#[cfg(test)]
#[path = "../borrow_check_tests.rs"]
mod tests;
