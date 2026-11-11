use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::HirExprKind;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_field(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Field { base, field, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let base_id = *base;
            // A struct field named on a callee (`job.run(x)`) is read from
            // its base, never a path of its own.
            let value_ref = super::get_resolved_value_ref(type_check, expr).filter(|&symbol| {
                type_check
                    .symbols
                    .try_get(symbol)
                    .is_none_or(|symbol| symbol.kind != arandu_middle::SymbolKind::Field)
            });
            if let Some(symbol) =
                super::lookup_namespace_field(pool, base_id, field, type_check).or(value_ref)
            {
                HirExprKind::Path { symbol }
            } else {
                let base_vid = super::lower_expr(type_check, pool, hir_pool, base_id)?;
                HirExprKind::Field {
                    base: base_vid,
                    field: field.clone(),
                }
            }
        }
    })
}

pub(super) fn lower_safe_field(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::SafeField { base, field, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let base_id = *base;
            if let Some(symbol) = super::lookup_namespace_field(pool, base_id, field, type_check)
                .or_else(|| super::get_resolved_value_ref(type_check, expr))
            {
                HirExprKind::Path { symbol }
            } else {
                let base_vid = super::lower_expr(type_check, pool, hir_pool, base_id)?;
                HirExprKind::SafeField {
                    base: base_vid,
                    field: field.clone(),
                }
            }
        }
    })
}

pub(super) fn lower_index(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Index { base, index, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let base_id = super::lower_expr(type_check, pool, hir_pool, *base)?;
            let index_id = super::lower_expr(type_check, pool, hir_pool, *index)?;
            HirExprKind::Index {
                base: base_id,
                index: index_id,
            }
        }
    })
}

pub(super) fn lower_safe_index(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::SafeIndex { base, index, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let base_id = super::lower_expr(type_check, pool, hir_pool, *base)?;
            let index_id = super::lower_expr(type_check, pool, hir_pool, *index)?;
            HirExprKind::SafeIndex {
                base: base_id,
                index: index_id,
            }
        }
    })
}

pub(super) fn lower_try(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Try {
        expr: inner_expr, ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    Ok({
        {
            let inner_id = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Try { expr: inner_id }
        }
    })
}
