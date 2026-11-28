use arandu_lexer::Span;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};
use arandu_parser::{Block, MatchArmBody, Stmt};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, TypeId};
use super::stmt::check_stmt;

/// Returns true when execution cannot fall through the end of `block`.
#[must_use]
pub fn block_diverges(pool: &AstPool, block: &Block) -> bool {
    pool.stmt_list(block.statements)
        .iter()
        .any(|&stmt_id| stmt_diverges(pool, pool.stmt(stmt_id)))
}

fn stmt_diverges(pool: &AstPool, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => block_diverges(pool, then_block) && block_diverges(pool, else_block),
        Stmt::Match { expr, .. } | Stmt::Expr { expr, .. } => expr_diverges(pool, *expr),
        Stmt::Unsafe { block, .. } => block_diverges(pool, block),
        _ => false,
    }
}

/// Returns true when evaluating `expr` always exits via `return`, `break`, or `continue`.
#[must_use]
pub fn expr_diverges(pool: &AstPool, expr: ExprId) -> bool {
    match pool.expr(expr) {
        ExprKind::If {
            then_block,
            else_block,
            ..
        } => {
            block_diverges(pool, pool.block(*then_block))
                && block_diverges(pool, pool.block(*else_block))
        }
        ExprKind::Match { arms, .. } => {
            let arm_ids = pool.match_arm_list(*arms);
            !arm_ids.is_empty()
                && arm_ids
                    .iter()
                    .all(|&arm_id| match &pool.match_arm(arm_id).body {
                        MatchArmBody::Block { block, .. } => block_diverges(pool, block),
                        MatchArmBody::Expr { expr, .. } => expr_diverges(pool, *expr),
                    })
        }
        ExprKind::UnsafeBlock { block } => block_diverges(pool, pool.block(*block)),
        _ => false,
    }
}

fn check_tail_expr(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    span: Span,
    tail_expected: Option<TypeId>,
    allow_void_statement: bool,
) -> ArType {
    let last_ty_id = super::super::synth::synth_expr_expected(checker, expr, tail_expected);
    let last_ty = checker.resolve(last_ty_id);
    if allow_void_statement && matches!(last_ty, ArType::Void | ArType::Never) {
        return last_ty;
    }
    if let Some(expected_id) = tail_expected {
        let expected = checker.resolve(expected_id);
        if let Some(var_id) = checker.literal_table.var_for_expr(expr)
            && !expected.is_literal()
            && !expected.is_error()
        {
            checker.constrain_literal_var(
                var_id,
                expected_id,
                ConstraintOrigin::ReturnType {
                    return_span: span,
                    declared_span: checker.ctx.current_return_decl_span().unwrap_or(span),
                },
            );
        }
        if !checker.unify_return_type(&expected, &last_ty) {
            checker.add_constraint(
                expected,
                last_ty.clone(),
                ConstraintOrigin::ReturnType {
                    return_span: span,
                    declared_span: checker.ctx.current_return_decl_span().unwrap_or(span),
                },
            );
        }
    }
    last_ty
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, pool, block))]
pub fn check_block(checker: &mut TypeChecker<'_>, pool: &AstPool, block: &Block) -> ArType {
    check_block_tail(checker, pool, block, None)
}

/// Like [`check_block`], but when `tail_expected` is set and the last statement is an
/// expression, typecheck it as an **implicit return** (SYN.1) against that type.
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, pool, block))]
pub fn check_block_tail(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    block: &Block,
    tail_expected: Option<TypeId>,
) -> ArType {
    let mut last_ty = ArType::Void;
    let statements = pool.stmt_list(block.statements);
    let len = statements.len();
    let tail_is_void =
        tail_expected.is_some_and(|exp| matches!(checker.resolve(exp), ArType::Void));
    let tail_non_void =
        tail_expected.is_some_and(|exp| !matches!(checker.resolve(exp), ArType::Void));
    for (i, &stmt) in statements.iter().enumerate() {
        let stmt = pool.stmt(stmt);
        if i == len - 1 {
            match stmt {
                Stmt::Expr {
                    expr,
                    span,
                    has_semi,
                } => {
                    if *has_semi && !tail_non_void {
                        let expr_ty_id = super::super::synth::synth_expr(checker, *expr);
                        last_ty = if matches!(checker.resolve(expr_ty_id), ArType::Never) {
                            ArType::Never
                        } else {
                            ArType::Void
                        };
                    } else if tail_is_void {
                        let expr_expected = if matches!(
                            pool.expr(*expr),
                            ExprKind::If { .. } | ExprKind::Match { .. }
                        ) {
                            tail_expected
                        } else {
                            None
                        };
                        let last_ty_id =
                            super::super::synth::synth_expr_expected(checker, *expr, expr_expected);
                        let ty = checker.resolve(last_ty_id);
                        if checker.result_ok_err(&ty).is_some() {
                            checker.add_constraint(
                                ArType::Void,
                                ty.clone(),
                                ConstraintOrigin::ReturnType {
                                    return_span: *span,
                                    declared_span: checker
                                        .ctx
                                        .current_return_decl_span()
                                        .unwrap_or(*span),
                                },
                            );
                        }
                        last_ty = if matches!(ty, ArType::Never) {
                            ArType::Never
                        } else {
                            ArType::Void
                        };
                    } else {
                        last_ty = check_tail_expr(checker, *expr, *span, tail_expected, false);
                    }
                }
                Stmt::Match { expr, span } if tail_non_void => {
                    last_ty = check_tail_expr(checker, *expr, *span, tail_expected, true);
                }
                Stmt::Unsafe { block, .. } => {
                    checker.ctx.enter_unsafe();
                    let inner_ty = check_block_tail(
                        checker,
                        pool,
                        block,
                        if tail_non_void { tail_expected } else { None },
                    );
                    checker.ctx.exit_unsafe();
                    last_ty = if matches!(inner_ty, ArType::Never) || tail_non_void {
                        inner_ty
                    } else {
                        ArType::Void
                    };
                }
                _ => {
                    check_stmt(checker, pool, stmt);
                    last_ty = ArType::Void;
                }
            }
        } else {
            check_stmt(checker, pool, stmt);
        }
    }
    last_ty
}
