//! Resolution outputs: the symbol table and the side tables keyed by AST nodes.

use std::sync::Arc;

use arandu_parser::ast_pool::ExprId;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Diagnostic, DocCommentMap, NodeKey, Severity, SymbolId, SymbolTable};
use arandu_middle::Span;

/// Resolution output shared across queries and type-check fan-out.
///
/// PERF: `symbols`/`resolved` are O(1)-clone [`Arc`] handles so that Salsa
/// memoization, IDE snapshots and per-item type-check share one table instead
/// of deep-cloning on every query call. Mutation goes through
/// `Arc::make_mut` / `Arc::unwrap_or_clone` (copy-on-write, once per shared
/// handle). `diagnostics` stays an owned `Vec` — it is typically empty and
/// pushed to in many sites.
#[derive(Debug, Clone)]
pub struct ResolutionResult {
    pub symbols: Arc<SymbolTable>,
    pub resolved: Arc<ResolvedNames>,
    pub docs: DocCommentMap,
    pub diagnostics: Vec<Diagnostic>,
    pub is_cycle_fallback: bool,
}

impl ResolutionResult {
    #[must_use]
    pub fn cycle_fallback() -> Self {
        Self {
            symbols: Arc::new(SymbolTable::new(0)),
            resolved: Arc::new(ResolvedNames::default()),
            docs: DocCommentMap::default(),
            diagnostics: Vec::new(),
            is_cycle_fallback: true,
        }
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedNames {
    /// Templates with staging are resolved and checked only after their
    /// structural arguments are known. Source IDs remain their identity.
    pub deferred_comptime_functions: FxHashSet<SymbolId>,
    /// Static loop bodies are checked and lowered in separate occurrence domains.
    pub deferred_loop_bodies: FxHashSet<NodeKey>,
    /// Revision-local static branch decisions. Missing means failed/not staged,
    /// never permission to resolve or type either branch.
    pub comptime_branches: FxHashMap<NodeKey, bool>,
    /// Finite half-open domains frozen before body lowering. Absence is never
    /// permission for a static loop to fall back to runtime iteration.
    pub comptime_loops:
        FxHashMap<NodeKey, (arandu_middle::ctfe::ConstInt, arandu_middle::ctfe::ConstInt)>,
    /// Revision-local values frozen by pre-body staging. `None` records a
    /// failed obligation whose diagnostic is already reported by the producer.
    /// Pure consumers must not evaluate expressions or fall back to runtime.
    pub comptime_arguments: FxHashMap<NodeKey, Option<u64>>,
    pub typed_comptime_arguments: FxHashMap<NodeKey, arandu_middle::ctfe::ConstValue>,
    pub definitions: FxHashMap<NodeKey, SymbolId>,
    pub expr_symbols: Vec<Option<SymbolId>>,
    pub value_refs: FxHashMap<NodeKey, SymbolId>,
    pub type_refs: FxHashMap<NodeKey, SymbolId>,
    pub mutable_symbols: FxHashSet<SymbolId>,
}

impl ResolvedNames {
    pub fn define(&mut self, span: Span, symbol: SymbolId) {
        self.definitions.insert(span.into(), symbol);
    }

    pub fn expr_ref(&mut self, expr: ExprId, symbol: SymbolId) {
        let idx = expr.as_usize();
        if self.expr_symbols.len() <= idx {
            self.expr_symbols.resize(idx + 1, None);
        }
        self.expr_symbols[idx] = Some(symbol);
    }

    #[must_use]
    pub fn expr_symbol(&self, expr: ExprId) -> Option<SymbolId> {
        self.expr_symbols
            .get(expr.as_usize())
            .and_then(|symbol| *symbol)
    }

    pub fn value_ref(&mut self, span: Span, symbol: SymbolId) {
        self.value_refs.insert(span.into(), symbol);
    }

    pub fn type_ref(&mut self, span: Span, symbol: SymbolId) {
        self.type_refs.insert(span.into(), symbol);
    }
}
