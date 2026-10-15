use arandu_parser::ast_pool::AstPool;
use arandu_parser::{
    BindingItem, Block, Condition, DeferBody, ExprId, ForBinding, ForClause, Place, PlaceSuffix,
    SimpleStmt, Stmt,
};

use crate::{ScopeId, SymbolKind};

use super::Resolver;

impl<'a> Resolver<'a> {
    pub(crate) fn declare_condition_bindings(&mut self, scope: ScopeId, condition: &Condition) {
        match condition {
            Condition::Expr { .. } => {}
            Condition::Is { pattern, .. } => self.resolve_pattern(scope, *pattern),
            Condition::And { conditions, .. } => {
                for condition in conditions {
                    self.declare_condition_bindings(scope, condition);
                }
            }
        }
    }
    /// Establish lexical shadowing without resolving/typing runtime initializers.
    /// These bindings can only be reported as captures by pre-body staging.
    pub(crate) fn staging_context(
        &mut self,
        scope: ScopeId,
        pool: &AstPool,
        block: &Block,
        target: arandu_middle::Span,
        depth: usize,
    ) -> Option<ScopeId> {
        if depth > 64 {
            return None;
        }
        if block.span == target {
            return Some(scope);
        }
        let contains =
            |span: arandu_middle::Span| span.start <= target.start && target.end <= span.end;
        for &id in pool.stmt_list(block.statements) {
            let stmt = pool.stmt(id);
            if stmt.span() == target {
                return Some(scope);
            }
            if stmt.span().start > target.start {
                break;
            }
            if contains(stmt.span()) {
                let child = self.symbols.new_scope(scope);
                let nested = match stmt {
                    Stmt::If {
                        condition,
                        then_block,
                        else_block,
                        ..
                    } => {
                        if let Some(selected) =
                            self.staging_condition(scope, pool, condition, target, depth + 1)
                        {
                            return Some(selected);
                        }
                        if contains(then_block.span) {
                            self.declare_condition_bindings(child, condition);
                            return self.staging_context(
                                child,
                                pool,
                                then_block,
                                target,
                                depth + 1,
                            );
                        }
                        else_block.as_ref().filter(|b| contains(b.span))
                    }
                    Stmt::While {
                        body, condition, ..
                    } => {
                        if let Some(selected) =
                            self.staging_condition(scope, pool, condition, target, depth + 1)
                        {
                            return Some(selected);
                        }
                        self.declare_condition_bindings(child, condition);
                        Some(body)
                    }
                    Stmt::For { body, clause, .. } => {
                        match clause {
                            ForClause::In {
                                bindings, iterable, ..
                            } => {
                                if contains(pool.expr_span(*iterable)) {
                                    return self.staging_expression(
                                        scope,
                                        pool,
                                        *iterable,
                                        target,
                                        depth + 1,
                                    );
                                }
                                for binding in bindings {
                                    self.define_for_binding(child, binding);
                                }
                            }
                            ForClause::CStyle {
                                init,
                                condition,
                                step,
                                ..
                            } => {
                                if let Some(init) = init {
                                    let (span, expression) = match init {
                                        SimpleStmt::VarDecl { span, value, .. }
                                        | SimpleStmt::Set { span, value, .. } => (*span, *value),
                                        SimpleStmt::Expr { span, expr } => (*span, *expr),
                                    };
                                    if contains(span) {
                                        return if contains(pool.expr_span(expression)) {
                                            self.staging_expression(
                                                scope,
                                                pool,
                                                expression,
                                                target,
                                                depth + 1,
                                            )
                                        } else {
                                            Some(scope)
                                        };
                                    }
                                }
                                if let Some(SimpleStmt::VarDecl { bindings, .. }) = init {
                                    for binding in bindings {
                                        self.define(
                                            child,
                                            &binding.name,
                                            SymbolKind::Local,
                                            binding.span,
                                        );
                                    }
                                }
                                if let Some(expression) = condition
                                    && contains(pool.expr_span(*expression))
                                {
                                    return self.staging_expression(
                                        child,
                                        pool,
                                        *expression,
                                        target,
                                        depth + 1,
                                    );
                                }
                                if let Some(step) = step {
                                    let expression = match step {
                                        SimpleStmt::VarDecl { value, .. }
                                        | SimpleStmt::Set { value, .. } => *value,
                                        SimpleStmt::Expr { expr, .. } => *expr,
                                    };
                                    if contains(pool.expr_span(expression)) {
                                        return self.staging_expression(
                                            child,
                                            pool,
                                            expression,
                                            target,
                                            depth + 1,
                                        );
                                    }
                                }
                            }
                        }
                        Some(body)
                    }
                    Stmt::Unsafe { block, .. }
                    | Stmt::Defer {
                        body: DeferBody::Block { block, .. },
                        ..
                    }
                    | Stmt::ErrDefer {
                        body: DeferBody::Block { block, .. },
                        ..
                    } => Some(block),
                    Stmt::VarDecl { value, .. } if !contains(pool.expr_span(*value)) => {
                        // Type arguments in a local annotation are checked in
                        // the incoming scope, before the local is declared.
                        // They have no child edge through its initializer.
                        return Some(scope);
                    }
                    Stmt::VarDecl { value, .. } | Stmt::Set { value, .. } => {
                        return self.staging_expression(child, pool, *value, target, depth + 1);
                    }
                    Stmt::Return { values, .. } => {
                        for &expression in values {
                            if contains(pool.expr_span(expression)) {
                                return self.staging_expression(
                                    child,
                                    pool,
                                    expression,
                                    target,
                                    depth + 1,
                                );
                            }
                        }
                        return None;
                    }
                    Stmt::Expr { expr, .. }
                    | Stmt::Match { expr, .. }
                    | Stmt::Free { expr, .. } => {
                        return self.staging_expression(child, pool, *expr, target, depth + 1);
                    }
                    Stmt::Break { .. }
                    | Stmt::Continue { .. }
                    | Stmt::Defer { .. }
                    | Stmt::ErrDefer { .. }
                    | Stmt::Error(_) => None,
                };
                return nested
                    .and_then(|b| self.staging_context(child, pool, b, target, depth + 1));
            }
            if let Stmt::VarDecl { bindings, .. } = stmt {
                for binding in bindings {
                    self.define(scope, &binding.name, SymbolKind::Local, binding.span);
                }
            }
        }
        None
    }
    pub(crate) fn resolve_block_child(&mut self, parent: ScopeId, pool: &AstPool, block: &Block) {
        let scope = self.symbols.new_scope(parent);
        self.resolve_block_in_scope(scope, pool, block);
    }

    pub(crate) fn resolve_block_in_scope(&mut self, scope: ScopeId, pool: &AstPool, block: &Block) {
        for &stmt in pool.stmt_list(block.statements) {
            self.resolve_stmt(scope, pool, pool.stmt(stmt));
        }
    }

    pub(crate) fn resolve_stmt(&mut self, scope: ScopeId, pool: &AstPool, stmt: &Stmt) {
        match stmt {
            Stmt::VarDecl {
                bindings, value, ..
            } => self.resolve_var_decl(scope, bindings, *value),
            Stmt::Set { places, value, .. } => {
                for place in places {
                    self.resolve_place(scope, place);
                }
                self.resolve_expr(scope, *value);
            }
            Stmt::Return { values, .. } => {
                for value in values {
                    self.resolve_expr(scope, *value);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Free { expr, .. } => self.resolve_expr(scope, *expr),
            Stmt::Expr { expr, .. } => self.resolve_expr(scope, *expr),
            Stmt::If {
                span,
                is_comptime,
                condition,
                then_block,
                else_block,
                ..
            } => {
                if *is_comptime {
                    if let Some(&selected) = self.resolved.comptime_branches.get(&(*span).into()) {
                        // Record compile-time import use, but never expose the
                        // discarded body to name resolution.
                        self.resolve_condition(scope, pool, condition);
                        if selected {
                            self.resolve_block_child(scope, pool, then_block);
                        } else if let Some(block) = else_block {
                            self.resolve_block_child(scope, pool, block);
                        }
                    } else if !self
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == crate::Severity::Error)
                    {
                        self.diagnostics.push(crate::Diagnostic::error(crate::DiagCode::T042UnsupportedComptime, "comptime if requires compile-time branch staging in this compilation context", *span));
                    }
                    return;
                }
                let then_scope = self.resolve_condition(scope, pool, condition);
                self.resolve_block_child(then_scope, pool, then_block);
                if let Some(block) = else_block {
                    self.resolve_block_child(scope, pool, block);
                }
            }
            Stmt::For {
                span,
                is_comptime,
                clause,
                body,
            } => {
                if *is_comptime && !self.resolved.comptime_loops.contains_key(&(*span).into()) {
                    return;
                }
                if self.resolved.deferred_loop_bodies.contains(&(*span).into()) {
                    let empty = arandu_parser::Block {
                        span: body.span,
                        statements: arandu_parser::ast_pool::IndexRange::empty(),
                    };
                    self.resolve_for(scope, pool, clause, &empty);
                } else {
                    self.resolve_for(scope, pool, clause, body);
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                let body_scope = self.resolve_condition(scope, pool, condition);
                self.resolve_block_child(body_scope, pool, body);
            }
            Stmt::Match { expr, .. } => self.resolve_expr(scope, *expr),
            Stmt::Defer { body, .. } | Stmt::ErrDefer { body, .. } => {
                self.resolve_defer_body(scope, pool, body);
            }
            Stmt::Unsafe { block, .. } => self.resolve_block_child(scope, pool, block),
            Stmt::Error(span) => {
                let _ = span;
            }
        }
    }

    pub(crate) fn resolve_var_decl(
        &mut self,
        scope: ScopeId,
        bindings: &[BindingItem],
        value: ExprId,
    ) {
        self.resolve_expr(scope, value);
        for binding in bindings {
            if let Some(ty) = &binding.ty {
                self.resolve_type_expr(scope, *ty);
            }
        }
        for binding in bindings {
            self.define(scope, &binding.name, SymbolKind::Local, binding.span);
            if let Some(symbol_id) = self
                .resolved
                .definitions
                .get(&binding.span.into())
                .copied()
                .filter(|_| binding.mutable)
            {
                self.resolved.mutable_symbols.insert(symbol_id);
            }
        }
    }

    pub(crate) fn resolve_simple_stmt(&mut self, scope: ScopeId, stmt: &SimpleStmt) {
        match stmt {
            SimpleStmt::VarDecl {
                bindings, value, ..
            } => self.resolve_var_decl(scope, bindings, *value),
            SimpleStmt::Set { places, value, .. } => {
                for place in places {
                    self.resolve_place(scope, place);
                }
                self.resolve_expr(scope, *value);
            }
            SimpleStmt::Expr { expr, .. } => self.resolve_expr(scope, *expr),
        }
    }

    pub(crate) fn resolve_for(
        &mut self,
        parent: ScopeId,
        pool: &AstPool,
        clause: &ForClause,
        body: &Block,
    ) {
        let scope = self.symbols.new_scope(parent);
        match clause {
            ForClause::In {
                bindings, iterable, ..
            } => {
                self.resolve_expr(parent, *iterable);
                for binding in bindings {
                    self.define_for_binding(scope, binding);
                }
            }
            ForClause::CStyle {
                init,
                condition,
                step,
                ..
            } => {
                if let Some(init) = init {
                    self.resolve_simple_stmt(scope, init);
                }
                if let Some(condition) = condition {
                    self.resolve_expr(scope, *condition);
                }
                if let Some(step) = step {
                    self.resolve_simple_stmt(scope, step);
                }
            }
        }
        self.resolve_block_in_scope(scope, pool, body);
    }

    pub(crate) fn define_for_binding(&mut self, scope: ScopeId, binding: &ForBinding) {
        self.define(scope, &binding.name, SymbolKind::Local, binding.span);
        if let Some(symbol_id) = self
            .resolved
            .definitions
            .get(&binding.span.into())
            .copied()
            .filter(|_| binding.mutable)
        {
            self.resolved.mutable_symbols.insert(symbol_id);
        }
    }

    pub(crate) fn resolve_defer_body(&mut self, scope: ScopeId, pool: &AstPool, body: &DeferBody) {
        match body {
            DeferBody::Expr { expr, .. } => self.resolve_expr(scope, *expr),
            DeferBody::Block { block, .. } => self.resolve_block_child(scope, pool, block),
        }
    }

    pub(crate) fn resolve_condition(
        &mut self,
        scope: ScopeId,
        _pool: &AstPool,
        condition: &Condition,
    ) -> ScopeId {
        match condition {
            Condition::Expr { expr, .. } => {
                self.resolve_expr(scope, *expr);
                scope
            }
            Condition::Is { expr, pattern, .. } => {
                self.resolve_expr(scope, *expr);
                let pattern_scope = self.symbols.new_scope(scope);
                self.resolve_pattern(pattern_scope, *pattern);
                pattern_scope
            }
            Condition::And { conditions, .. } => {
                conditions.iter().fold(scope, |clause_scope, c| {
                    self.resolve_condition(clause_scope, self.pool, c)
                })
            }
        }
    }

    pub(crate) fn resolve_place(&mut self, scope: ScopeId, place: &Place) {
        self.resolve_assignment_target(scope, &place.root, place.span);
        for suffix in &place.suffixes {
            if let PlaceSuffix::Index { expr, .. } = suffix {
                self.resolve_expr(scope, *expr);
            }
        }
    }
}
