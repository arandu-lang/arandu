use arandu_lexer::Span;
use rustc_hash::FxHashMap;

use crate::SymbolId;

use super::types::TypeId;

/// A return belongs to the nearest function or isolated evaluation root, not
/// to the function that happens to contain its source text.
#[derive(Debug)]
struct ReturnScope {
    expected: Option<TypeId>,
    span: Span,
    ctfe_values: Option<Vec<ReturnValue>>,
    ctfe_propagations: Vec<ReturnValue>,
    outer_loop_depth: Option<u32>,
}

#[derive(Debug)]
pub(super) struct ReturnValue {
    pub ty: TypeId,
    pub span: Span,
    pub expression: Option<arandu_parser::ast_pool::ExprId>,
}

// ── TyCtx — typing context ─────────────────────────────────────────

/// Typing context that accumulates bindings as we walk the AST.
///
/// Unlike the `SymbolTable` (which tracks names in scopes), `TyCtx` maps
/// each `SymbolId` to its `TypeId`. It also tracks the expected return
/// type of the current function and whether we're inside a loop.
///
/// ## Multi-file invariant
///
/// Bindings **must** key on the full [`SymbolId`] (`file_id` + `local_id`).
/// Indexing only by `local_id` collides imported functions with locals that
/// happen to share a dense index (e.g. `let p = f()` then `rt.spawn_i64(...)`
/// saw the local's type as the callee). That is a multi-module root bug, not
/// a surface symptom.
#[derive(Debug)]
pub struct TyCtx {
    /// Map from full `SymbolId` → inferred/declared `TypeId`.
    bindings: FxHashMap<SymbolId, TypeId>,

    /// Return targets, including isolated roots whose type is being inferred.
    return_stack: Vec<ReturnScope>,

    /// Depth of loop nesting (for validating break/continue).
    loop_depth: u32,

    /// Depth of unsafe block nesting.
    unsafe_depth: u32,
}

impl Default for TyCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl TyCtx {
    #[must_use]
    pub fn new() -> Self {
        Self {
            bindings: FxHashMap::default(),
            return_stack: Vec::new(),
            loop_depth: 0,
            unsafe_depth: 0,
        }
    }

    // ── Bindings ────────────────────────────────────────────────────

    /// Record that `symbol` has type `ty`.
    pub fn bind(&mut self, symbol: SymbolId, ty: TypeId) {
        self.bindings.insert(symbol, ty);
    }

    /// Look up the type for a symbol.
    #[must_use]
    pub fn lookup(&self, symbol: SymbolId) -> Option<TypeId> {
        self.bindings.get(&symbol).copied()
    }

    // ── Return type stack ───────────────────────────────────────────

    /// Push an expected return type when entering a function body.
    pub fn push_return(&mut self, ty: TypeId, decl_span: Span) {
        self.return_stack.push(ReturnScope {
            expected: Some(ty),
            span: decl_span,
            ctfe_values: None,
            ctfe_propagations: Vec::new(),
            outer_loop_depth: None,
        });
    }

    /// Pop the return type when leaving a function body.
    pub fn pop_return(&mut self) {
        self.pop_return_scope();
    }

    /// Get the return type expected by the current function.
    #[must_use]
    pub fn current_return(&self) -> Option<TypeId> {
        self.return_stack.last().and_then(|scope| scope.expected)
    }

    /// Span of the declared return type for the current function.
    #[must_use]
    pub fn current_return_decl_span(&self) -> Option<Span> {
        self.return_stack.last().map(|scope| scope.span)
    }

    pub(super) fn push_ctfe_return(&mut self, expected: Option<TypeId>, span: Span) {
        self.return_stack.push(ReturnScope {
            expected,
            span,
            ctfe_values: Some(Vec::new()),
            ctfe_propagations: Vec::new(),
            outer_loop_depth: Some(self.loop_depth),
        });
        // A control-flow exit cannot jump from CTFE into a runtime loop.
        self.loop_depth = 0;
    }

    pub(super) fn is_ctfe_return(&self) -> bool {
        self.return_stack
            .last()
            .is_some_and(|scope| scope.ctfe_values.is_some())
    }

    pub(super) fn record_ctfe_return(&mut self, value: ReturnValue) {
        if let Some(values) = self
            .return_stack
            .last_mut()
            .and_then(|scope| scope.ctfe_values.as_mut())
        {
            values.push(value);
        }
    }

    pub(super) fn record_ctfe_propagation(&mut self, value: ReturnValue) {
        if let Some(scope) = self.return_stack.last_mut()
            && scope.ctfe_values.is_some()
        {
            scope.ctfe_propagations.push(value);
        }
    }

    pub(super) fn pop_ctfe_return(&mut self) -> Option<(Vec<ReturnValue>, Vec<ReturnValue>)> {
        let scope = self.pop_return_scope()?;
        Some((scope.ctfe_values?, scope.ctfe_propagations))
    }

    fn pop_return_scope(&mut self) -> Option<ReturnScope> {
        let scope = self.return_stack.pop()?;
        if let Some(depth) = scope.outer_loop_depth {
            self.loop_depth = depth;
        }
        Some(scope)
    }

    // ── Loop tracking ───────────────────────────────────────────────

    /// Enter a loop scope.
    pub fn enter_loop(&mut self) {
        self.loop_depth += 1;
    }

    /// Leave a loop scope.
    pub fn exit_loop(&mut self) {
        self.loop_depth = self.loop_depth.saturating_sub(1);
    }

    /// Returns true if we're inside a loop.
    #[must_use]
    pub fn is_in_loop(&self) -> bool {
        self.loop_depth > 0
    }

    // ── Unsafe tracking ─────────────────────────────────────────────

    /// Enter an unsafe scope.
    pub fn enter_unsafe(&mut self) {
        self.unsafe_depth += 1;
    }

    /// Leave an unsafe scope.
    pub fn exit_unsafe(&mut self) {
        self.unsafe_depth = self.unsafe_depth.saturating_sub(1);
    }

    /// Returns true if we're inside an unsafe block.
    #[must_use]
    pub fn is_in_unsafe(&self) -> bool {
        self.unsafe_depth > 0
    }
}
