use crate::NO_GENERATIONAL_FALLBACK;
use crate::Span;
use crate::amir::{BlockId, LocalId};
use arandu_middle::types::ReturnBorrowSummary;
use std::sync::atomic::Ordering;

/// How a reference escapes the current function's static window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeKind {
    /// Returning a reference derived from a stack local.
    Return,
    /// Storing a stack-derived ref through an indirection into storage that
    /// may outlive the frame — candidate for generational fallback.
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
