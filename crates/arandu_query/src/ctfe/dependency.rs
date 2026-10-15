//! Explicit staging ancestry. No thread-local state or database-side counter.
//! The ancestry is part of a request key; cycle identity is checked separately
//! from that contextual key so increasing depth cannot disguise a cycle.

use arandu_middle::{DiagCode, Diagnostic, Span, SymbolId};
use std::sync::Arc;

/// Conservative bound on native query nesting, independent of VM call frames.
pub const MAX_QUERY_DEPENDENCY_DEPTH: usize = 16;
const MAX_CAUSAL_LABELS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum DependencyKey {
    Global(SymbolId),
    Header(arandu_middle::types::FunctionInstance),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Entry {
    key: DependencyKey,
    span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DependencyContext {
    path: Arc<[Entry]>,
}

impl DependencyContext {
    pub(crate) fn is_empty(&self) -> bool {
        self.path.is_empty()
    }

    pub(crate) fn enter(&self, key: DependencyKey, span: Span) -> Result<Self, Diagnostic> {
        let cycle = self.path.iter().any(|entry| entry.key == key);
        if cycle || self.path.len() >= MAX_QUERY_DEPENDENCY_DEPTH {
            let mut diagnostic = if cycle {
                Diagnostic::error(
                    DiagCode::T044ComptimeEvaluationFailed,
                    "cyclic compile-time declaration dependency",
                    span,
                )
                .with_primary_label("this declaration is already in the dependency path")
            } else {
                Diagnostic::error(
                    DiagCode::T045ComptimeLimitExceeded,
                    "compile-time query dependency depth exceeded",
                    span,
                )
                .with_primary_label(format!(
                    "at most {MAX_QUERY_DEPENDENCY_DEPTH} nested declaration dependencies"
                ))
            };
            for entry in self.path.iter().rev().take(MAX_CAUSAL_LABELS).rev() {
                diagnostic =
                    diagnostic.with_label(entry.span, "required by this compile-time declaration");
            }
            if self.path.len() > MAX_CAUSAL_LABELS {
                diagnostic = diagnostic.with_note("earlier dependency locations are omitted");
            }
            return Err(diagnostic);
        }
        let mut path = self.path.to_vec();
        path.push(Entry { key, span });
        Ok(Self { path: path.into() })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn growing_context_keeps_cycle_identity_and_bounds_acyclic_paths() {
        let span = Span::new(1, 2, 3);
        let mut context = DependencyContext::default();
        for index in 0..MAX_QUERY_DEPENDENCY_DEPTH {
            context = context
                .enter(
                    DependencyKey::Global(SymbolId::new(
                        1,
                        u32::try_from(index).expect("small index"),
                    )),
                    span,
                )
                .expect("within depth bound");
        }
        assert_eq!(
            context
                .enter(DependencyKey::Global(SymbolId::new(1, 0)), span)
                .expect_err("cycle takes priority")
                .code,
            DiagCode::T044ComptimeEvaluationFailed
        );
        assert_eq!(
            context
                .enter(DependencyKey::Global(SymbolId::new(1, 1000)), span)
                .expect_err("depth ceiling")
                .code,
            DiagCode::T045ComptimeLimitExceeded
        );
        assert!(DependencyContext::default()
            .enter(DependencyKey::Global(SymbolId::new(1, 1000)), span)
            .is_ok());
    }
}
