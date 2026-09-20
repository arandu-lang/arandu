use arandu_parser::ast_pool::AstPool;
use arandu_parser::{
    BindingItem, Block, Condition, DeferBody, ExprId, ForBinding, ForClause, Place, PlaceSuffix,
    SimpleStmt, Stmt,
};

use crate::{ScopeId, SymbolKind};

use super::Resolver;

impl<'a> Resolver<'a> {
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
                condition,
                then_block,
                else_block,
                ..
            } => {
                let then_scope = self.resolve_condition(scope, pool, condition);
                self.resolve_block_child(then_scope, pool, then_block);
                if let Some(block) = else_block {
                    self.resolve_block_child(scope, pool, block);
                }
            }
            Stmt::For { clause, body, .. } => self.resolve_for(scope, pool, clause, body),
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
