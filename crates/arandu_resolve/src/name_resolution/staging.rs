//! Lexical navigation for isolated staging. This walk only establishes bindings:
//! it never evaluates or resolves an enclosing runtime expression.

use arandu_middle::Span;
use arandu_parser::{
    CatchHandler, Condition, ExprId, ExprKind, LambdaBody, MatchArmBody, StringPart,
    ast_pool::AstPool,
};

use crate::{ScopeId, SymbolKind};

use super::Resolver;

impl Resolver<'_> {
    /// Navigate a selected clause without evaluating runtime siblings. Earlier
    /// successful patterns shadow globals in later clauses, just as in resolve.
    pub(crate) fn staging_condition(
        &mut self,
        scope: ScopeId,
        pool: &AstPool,
        condition: &Condition,
        target: Span,
        depth: usize,
    ) -> Option<ScopeId> {
        if depth > 64 {
            return None;
        }
        let contains = |span: Span| {
            span.file_id == target.file_id && span.start <= target.start && target.end <= span.end
        };
        match condition {
            Condition::Expr { expr, .. } | Condition::Is { expr, .. } => {
                contains(pool.expr_span(*expr))
                    .then(|| self.staging_expression(scope, pool, *expr, target, depth + 1))
                    .flatten()
            }
            Condition::And { conditions, .. } => {
                let mut scope = scope;
                for condition in conditions {
                    let span = match condition {
                        Condition::Expr { span, .. }
                        | Condition::Is { span, .. }
                        | Condition::And { span, .. } => *span,
                    };
                    if contains(span) {
                        return self.staging_condition(scope, pool, condition, target, depth + 1);
                    }
                    let next = self.symbols.new_scope(scope);
                    self.declare_condition_bindings(next, condition);
                    scope = next;
                }
                None
            }
        }
    }

    pub(crate) fn staging_expression(
        &mut self,
        scope: ScopeId,
        pool: &AstPool,
        expression: ExprId,
        target: Span,
        depth: usize,
    ) -> Option<ScopeId> {
        let contains = |span: Span| {
            span.file_id == target.file_id && span.start <= target.start && target.end <= span.end
        };
        if depth > 64 || !contains(pool.expr_span(expression)) {
            return None;
        }
        if pool.expr_span(expression) == target {
            return Some(scope);
        }
        // Only children containing the selected occurrence are followed. This
        // does not resolve sibling branches, initializers or lambda captures.
        let mut children = Vec::new();
        match pool.expr(expression) {
            ExprKind::Lambda { params, body } => {
                let scope = self.symbols.new_scope(scope);
                for &parameter in pool.lambda_param_list(*params) {
                    let parameter = pool.lambda_param(parameter);
                    self.define(scope, &parameter.name, SymbolKind::Param, parameter.span);
                }
                return match body {
                    LambdaBody::Expr { expr, .. } => {
                        self.staging_expression(scope, pool, *expr, target, depth + 1)
                    }
                    LambdaBody::Block { block, .. } => {
                        self.staging_context(scope, pool, block, target, depth + 1)
                    }
                };
            }
            ExprKind::Match { value, arms } => {
                if contains(pool.expr_span(*value)) {
                    return self.staging_expression(scope, pool, *value, target, depth + 1);
                }
                for &arm in pool.match_arm_list(*arms) {
                    let arm = pool.match_arm(arm);
                    if !contains(arm.span) {
                        continue;
                    }
                    let scope = self.symbols.new_scope(scope);
                    self.resolve_pattern(scope, arm.pattern);
                    if let Some(guard) = arm.guard
                        && contains(pool.expr_span(guard))
                    {
                        return self.staging_expression(scope, pool, guard, target, depth + 1);
                    }
                    return match &arm.body {
                        MatchArmBody::Expr { expr, .. } => {
                            self.staging_expression(scope, pool, *expr, target, depth + 1)
                        }
                        MatchArmBody::Block { block, .. } => {
                            self.staging_context(scope, pool, block, target, depth + 1)
                        }
                    };
                }
                return None;
            }
            ExprKind::AsyncBlock { block } | ExprKind::UnsafeBlock { block } => {
                let scope = self.symbols.new_scope(scope);
                return self.staging_context(scope, pool, pool.block(*block), target, depth + 1);
            }
            // Nested roots need their own selected environment, not a borrow of
            // the runtime owner's frame. Keep this unsupported case closed.
            ExprKind::Comptime { body } => {
                return match body {
                    arandu_parser::ast_pool::ComptimeBody::Block(block) => {
                        let scope = self.symbols.new_scope(scope);
                        self.staging_context(scope, pool, pool.block(*block), target, depth + 1)
                    }
                    arandu_parser::ast_pool::ComptimeBody::Expression(expression) => {
                        self.staging_expression(scope, pool, *expression, target, depth + 1)
                    }
                };
            }
            ExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                if let Some(selected) =
                    self.staging_condition(scope, pool, condition, target, depth + 1)
                {
                    return Some(selected);
                }
                for (block, is_then) in [(*then_block, true), (*else_block, false)] {
                    let block = pool.block(block);
                    if contains(block.span) {
                        let scope = self.symbols.new_scope(scope);
                        if is_then {
                            self.declare_condition_bindings(scope, condition);
                        }
                        return self.staging_context(scope, pool, block, target, depth + 1);
                    }
                }
                return None;
            }
            ExprKind::Catch { expr, handler } => {
                if contains(pool.expr_span(*expr)) {
                    children.push(*expr);
                } else {
                    return match pool.catch_handler(*handler) {
                        CatchHandler::Expr { expr, .. } => {
                            self.staging_expression(scope, pool, *expr, target, depth + 1)
                        }
                        CatchHandler::Block { span, error, block } => {
                            let scope = self.symbols.new_scope(scope);
                            self.define(scope, error, SymbolKind::Local, *span);
                            self.staging_context(scope, pool, block, target, depth + 1)
                        }
                    };
                }
            }
            ExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                if let Some(block) = trailing_block
                    && contains(pool.block(*block).span)
                {
                    let scope = self.symbols.new_scope(scope);
                    return self.staging_context(
                        scope,
                        pool,
                        pool.block(*block),
                        target,
                        depth + 1,
                    );
                }
                children.push(*callee);
                children.extend_from_slice(pool.expr_list(*args));
            }
            ExprKind::VariantSugar { args, .. } | ExprKind::Array { items: args } => {
                children.extend_from_slice(pool.expr_list(*args));
            }
            ExprKind::ArrayRepeat { value, count } => {
                children.push(*value);
                if let arandu_parser::TypeExpr::ConstExpression { expression, .. } =
                    pool.type_expr(*count)
                {
                    children.push(*expression);
                }
            }
            ExprKind::StructLiteral { fields, .. } => {
                children.extend(
                    pool.field_init_list(*fields)
                        .iter()
                        .map(|&field| pool.field_init(field).value),
                );
            }
            ExprKind::InterpolatedString { parts } => {
                children.extend(pool.string_part_list(*parts).iter().filter_map(|&part| {
                    match pool.string_part(part) {
                        StringPart::Expr { expr, .. } => Some(*expr),
                        StringPart::Text { .. } => None,
                    }
                }));
            }
            ExprKind::Generic { callee, .. } => children.push(*callee),
            ExprKind::Field { base, .. } | ExprKind::SafeField { base, .. } => children.push(*base),
            ExprKind::Index { base, index } | ExprKind::SafeIndex { base, index } => {
                children.extend([*base, *index])
            }
            ExprKind::Try { expr }
            | ExprKind::Alloc { expr }
            | ExprKind::Cast { expr, .. }
            | ExprKind::Group { expr }
            | ExprKind::Unary { expr, .. } => children.push(*expr),
            ExprKind::NullCoalesce { left, right } | ExprKind::Binary { left, right, .. } => {
                children.extend([*left, *right])
            }
            ExprKind::Layout { .. }
            | ExprKind::Path { .. }
            | ExprKind::TypePath { .. }
            | ExprKind::Error
            | ExprKind::Int { .. }
            | ExprKind::Float { .. }
            | ExprKind::Bool { .. }
            | ExprKind::Char { .. }
            | ExprKind::Byte { .. }
            | ExprKind::Nil => {}
        }
        for child in children {
            if contains(pool.expr_span(child)) {
                return self.staging_expression(scope, pool, child, target, depth + 1);
            }
        }
        // A computed type argument has no expression-child edge in the runtime
        // expression graph. Its containing expression still supplies the scope.
        Some(scope)
    }
}
