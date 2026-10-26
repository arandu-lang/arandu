use crate::diagnostics::Diagnostic;
use crate::hir::{HirCatchHandler, HirExpr, HirExprKind};
use crate::passes::type_checker::types::ArType;
use crate::{NodeKey, TypeCheckResult};
use arandu_parser::CatchHandler;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_layout(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    expr: ExprId,
    span: arandu_middle::Span,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Layout { query, ty } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        match query {
            arandu_parser::LayoutQuery::TargetOS => {
                HirExprKind::Str(type_check.type_info.target_identity.os.clone().into())
            }
            arandu_parser::LayoutQuery::TargetArch => {
                HirExprKind::Str(type_check.type_info.target_identity.arch.clone().into())
            }
            arandu_parser::LayoutQuery::TargetPointerWidth => {
                HirExprKind::Int(type_check.type_info.target_pointer_width.to_string().into())
            }
            arandu_parser::LayoutQuery::Size | arandu_parser::LayoutQuery::Align => {
                let Some(ty) = ty else {
                    return Err(Diagnostic::ice(
                        crate::DiagCode::ICET001,
                        "layout intrinsic has no type operand",
                        span,
                    ));
                };
                let interner =
                    &mut std::sync::Arc::make_mut(&mut type_check.type_info).type_interner;
                let operand = crate::passes::type_checker::types::lower_type_expr(
                    *ty,
                    pool,
                    &type_check.symbols,
                    crate::ScopeId(0),
                    &type_check.resolved,
                    interner,
                );
                HirExprKind::Layout {
                    query: *query,
                    operand_ty: interner.intern(operand),
                }
            }
        }
    })
}

pub(super) fn lower_comptime_in_ctfe(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let ExprKind::Comptime { body } = pool.expr(expr) else {
        unreachable!();
    };
    let kind = {
        match body {
            arandu_parser::ast_pool::ComptimeBody::Expression(inner) => {
                return super::lower_expr_raw(type_check, pool, hir_pool, *inner);
            }
            arandu_parser::ast_pool::ComptimeBody::Block(block) => {
                let source = pool.block(*block);
                let mut lowered =
                    super::super::stmt::lower_block_raw(type_check, pool, hir_pool, source)?;
                if pool.stmt_list(source.statements).last().is_some_and(|&id| {
                    matches!(
                        pool.stmt(id),
                        arandu_parser::Stmt::Expr { has_semi: true, .. }
                    )
                }) {
                    let nil = hir_pool.alloc_expr(HirExpr {
                        kind: HirExprKind::Nil,
                        ty: type_check.type_info.type_interner.intern(ArType::Void),
                        span,
                    });
                    let stmt = hir_pool.alloc_stmt(crate::hir::HirStmt {
                        kind: crate::hir::HirStmtKind::Expr(nil),
                        span,
                    });
                    let mut statements = hir_pool.stmt_list(lowered.statements).to_vec();
                    statements.push(stmt);
                    lowered.statements = hir_pool.alloc_stmt_list(&statements);
                }
                HirExprKind::ValueBlock {
                    block: hir_pool.alloc_block(lowered),
                }
            }
        }
    };
    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}

pub(super) fn lower_comptime(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let Some((value, layout)) = type_check.type_info.ctfe_values.get(&span) else {
        return Err(Diagnostic::error(crate::DiagCode::T042UnsupportedComptime,
            "this comptime root has not been evaluated; nested staging in CTFE helpers is not supported yet", span)
            .with_primary_label("compile-time evaluation required"));
    };
    super::super::materialize::materialize_ctfe_value(
        value,
        fallback_ty,
        &type_check.type_info,
        hir_pool,
        *layout,
        span,
    )
    .map_err(|_| {
        Diagnostic::ice(
            crate::DiagCode::ICET001,
            "evaluated scalar does not match its checked residual type",
            span,
        )
    })
}

pub(super) fn lower_if(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::If {
        condition,
        then_block,
        else_block,
        ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    Ok({
        HirExprKind::If {
            condition: super::super::stmt::lower_condition(type_check, pool, hir_pool, condition)?,
            then_block: super::super::stmt::lower_block(
                type_check,
                pool,
                hir_pool,
                pool.block(*then_block),
            )?,
            else_block: super::super::stmt::lower_block(
                type_check,
                pool,
                hir_pool,
                pool.block(*else_block),
            )?,
        }
    })
}

pub(super) fn lower_match(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Match { value, arms, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let arm_ids = pool.match_arm_list(*arms);
            let value_id = super::lower_expr(type_check, pool, hir_pool, *value)?;
            let arms_range =
                super::super::pattern::lower_match_arms(type_check, pool, hir_pool, arm_ids)?;
            HirExprKind::Match {
                value: value_id,
                arms: arms_range,
            }
        }
    })
}

pub(super) fn lower_catch(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Catch {
        expr: inner_expr,
        handler,
        ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    Ok({
        {
            let hir_expr_id = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            let hir_handler = match pool.catch_handler(*handler) {
                CatchHandler::Expr { expr: h, .. } => {
                    let eid = super::lower_expr(type_check, pool, hir_pool, *h)?;
                    HirCatchHandler::Expr(eid)
                }
                CatchHandler::Block { span, error, block } => {
                    let error_symbol = type_check
                        .resolved
                        .definitions
                        .get(&NodeKey::from(*span))
                        .copied();
                    let b = super::super::stmt::lower_block(type_check, pool, hir_pool, block)?;
                    HirCatchHandler::Block {
                        error_symbol,
                        error_name: Some(error.to_string()),
                        block: b,
                    }
                }
            };
            HirExprKind::Catch {
                expr: hir_expr_id,
                handler: hir_handler,
            }
        }
    })
}

pub(super) fn lower_null_coalesce(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::NullCoalesce { left, right, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let left_id = super::lower_expr(type_check, pool, hir_pool, *left)?;
            let right_id = super::lower_expr(type_check, pool, hir_pool, *right)?;
            HirExprKind::NullCoalesce {
                left: left_id,
                right: right_id,
            }
        }
    })
}

pub(super) fn lower_cast(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Cast {
        expr: inner_expr,
        ty: cast_ty,
        ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    Ok({
        {
            let interner = &mut std::sync::Arc::make_mut(&mut type_check.type_info).type_interner;
            let target_ar = crate::passes::type_checker::types::lower_type_expr(
                *cast_ty,
                pool,
                &type_check.symbols,
                crate::ScopeId(0),
                &type_check.resolved,
                interner,
            );
            let target_ty = interner.intern(target_ar);
            let inner_id = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Cast {
                expr: inner_id,
                target_ty,
            }
        }
    })
}
