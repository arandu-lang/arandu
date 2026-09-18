use arandu_parser::ast_pool::AstPool;
use arandu_parser::{Block, Stmt};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, TypeId};
use super::stmt::check_stmt;

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
    for (i, &stmt) in statements.iter().enumerate() {
        let stmt = pool.stmt(stmt);
        if i == len - 1 {
            if let Stmt::Expr {
                expr,
                span,
                has_semi,
            } = stmt
            {
                let tail_non_void =
                    tail_expected.is_some_and(|exp| !matches!(checker.resolve(exp), ArType::Void));
                if *has_semi && !tail_non_void {
                    let _ = super::super::synth::synth_expr(checker, *expr);
                    last_ty = ArType::Void;
                } else {
                    let last_ty_id =
                        super::super::synth::synth_expr_expected(checker, *expr, tail_expected);
                    last_ty = checker.resolve(last_ty_id);
                    if let Some(expected_id) = tail_expected {
                        let expected = checker.resolve(expected_id);
                        if let Some(var_id) = checker.literal_table.var_for_expr(*expr)
                            && !expected.is_literal()
                            && !expected.is_error()
                        {
                            checker.constrain_literal_var(
                                var_id,
                                expected_id,
                                ConstraintOrigin::ReturnType {
                                    return_span: *span,
                                    declared_span: checker
                                        .ctx
                                        .current_return_decl_span()
                                        .unwrap_or(*span),
                                },
                            );
                        }
                        if !checker.unify_return_type(&expected, &last_ty) {
                            checker.add_constraint(
                                expected,
                                last_ty.clone(),
                                ConstraintOrigin::ReturnType {
                                    return_span: *span,
                                    declared_span: checker
                                        .ctx
                                        .current_return_decl_span()
                                        .unwrap_or(*span),
                                },
                            );
                        }
                    }
                }
            } else {
                check_stmt(checker, pool, stmt);
                last_ty = ArType::Void;
            }
        } else {
            check_stmt(checker, pool, stmt);
        }
    }
    last_ty
}
