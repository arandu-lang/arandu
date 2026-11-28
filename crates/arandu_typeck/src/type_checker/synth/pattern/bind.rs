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
    if let Some(&symbol_id) = checker.resolved.definitions.get(&key) {
        // In a later alternative of `p1 | p2` the name is the variable the
        // first alternative bound, so it has one type in all of them.
        let shared = checker
            .symbols
            .try_get(symbol_id)
            .is_some_and(|symbol| symbol.span != *span);
        if shared && let Some(first) = checker.type_info.decl_type_id(symbol_id) {
            if !checker.unify_ids(first, value_ty) {
                let interner = &checker.type_info.type_interner;
                let name = checker.symbols.get(symbol_id).name.clone();
                let first_span = checker.symbols.get(symbol_id).span;
                let diagnostic = crate::Diagnostic::error(
                    crate::DiagCode::T002IncompatibleAssignment,
                    format!(
                        "'{name}' has type '{}' in this alternative and '{}' in the first",
                        checker
                            .resolve(value_ty)
                            .display(&checker.symbols, interner),
                        checker.resolve(first).display(&checker.symbols, interner),
                    ),
                    *span,
                )
                .with_label(first_span, "first bound here")
                .with_hint("a name bound by `p1 | p2` has the same type in every alternative");
                checker.diagnostics.push(diagnostic);
            }
            return;
        }
        checker.ctx.bind(symbol_id, value_ty);
        checker.record_decl_type(symbol_id, value_ty);
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
