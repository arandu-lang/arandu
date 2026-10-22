use arandu_lexer::Span;
use arandu_parser::Pattern;
use arandu_parser::ast_pool::ExprId;

use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::TypeId;
use super::super::expr::synth_expr;

pub(super) fn check_wildcard() {}

pub(super) fn check_bind(checker: &mut TypeChecker<'_>, span: &Span, value_ty: TypeId) {
    let key = crate::NodeKey::from(*span);
    if let Some(symbol_id) = checker.resolved.definitions.get(&key) {
        checker.ctx.bind(*symbol_id, value_ty);
        checker.record_decl_type(*symbol_id, value_ty);
    }
}

pub(super) fn check_literal(
    checker: &mut TypeChecker<'_>,
    pat: &Pattern,
    expr: &ExprId,
    value_ty: TypeId,
) {
    let expr_ty_id = synth_expr(checker, *expr);
    if !checker.unify_ids(value_ty, expr_ty_id) {
        checker.add_constraint(
            value_ty,
            expr_ty_id,
            ConstraintOrigin::Assignment {
                lhs_span: pat.span(),
                rhs_span: checker.pool.expr_span(*expr),
            },
        );
    }
}
