use rustc_hash::{FxHashMap, FxHashSet};

use arandu_lexer::Span;
use arandu_parser::ast_pool::ExprId;

use crate::SymbolId;

pub type DocCommentMap = FxHashMap<NodeKey, Vec<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeKey {
    pub start: u32,
    pub end: u32,
}

impl From<Span> for NodeKey {
    fn from(span: Span) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
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
    pub comptime_loops: FxHashMap<NodeKey, (crate::ctfe::ConstInt, crate::ctfe::ConstInt)>,
    /// Revision-local values frozen by pre-body staging. `None` records a
    /// failed obligation whose diagnostic is already reported by the producer.
    /// Pure consumers must not evaluate expressions or fall back to runtime.
    pub comptime_arguments: FxHashMap<NodeKey, Option<u64>>,
    pub typed_comptime_arguments: FxHashMap<NodeKey, crate::ctfe::ConstValue>,
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
