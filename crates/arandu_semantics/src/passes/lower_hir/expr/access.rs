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
                let symbol = super::path::maybe_wrap_exported_func_value(
                    type_check,
                    hir_pool,
                    symbol,
                    pool.expr_span(expr),
                );
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
                let symbol = super::path::maybe_wrap_exported_func_value(
                    type_check,
                    hir_pool,
                    symbol,
                    pool.expr_span(expr),
                );
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
            // `xs[r]` with a range value that is not a plain name: `r` is read
            // twice, so it is bound to a local in a scope around the index.
            if super::range::needs_local(type_check, pool, *index) {
                return scoped_range_index(type_check, pool, hir_pool, expr, *base, *index);
            }
            let base_id = super::lower_expr(type_check, pool, hir_pool, *base)?;
            let index_id = match super::range::bounds(type_check, pool, hir_pool, *index)? {
                Some(bounds) => bounds,
                None => super::lower_expr(type_check, pool, hir_pool, *index)?,
            };
            HirExprKind::Index {
                base: base_id,
                index: index_id,
            }
        }
    })
}

fn scoped_range_index(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    base: ExprId,
    index: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    use crate::hir::{HirBindingItem, HirBlock, HirExpr, HirStmt, HirStmtKind};
    let span = pool.expr_span(expr);
    let value_span = pool.expr_span(index);
    let base_id = super::lower_expr(type_check, pool, hir_pool, base)?;
    let value = super::lower_expr(type_check, pool, hir_pool, index)?;
    let ty = hir_pool.expr(value).ty;
    let local = super::temporary::synthetic_local(type_check, value_span, "range");
    let bindings = hir_pool.alloc_binding_list(&[HirBindingItem {
        symbol: local,
        ty,
        span: value_span,
    }]);
    let bind = hir_pool.alloc_stmt(HirStmt {
        kind: HirStmtKind::VarDecl { bindings, value },
        span: value_span,
    });
    hir_pool.hoisted_values.insert(value_span, (local, ty));
    let bounds = super::range::bounds(type_check, pool, hir_pool, index);
    hir_pool.hoisted_values.remove(&value_span);
    let index_id = bounds?.ok_or_else(|| {
        Diagnostic::ice(crate::DiagCode::ICEL001, "range value has no bounds", span)
    })?;
    let result_ty = type_check
        .type_info
        .expr_type_id(expr)
        .unwrap_or_else(arandu_middle::types::TypeInterner::preinterned_error_id);
    let indexed = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Index {
            base: base_id,
            index: index_id,
        },
        ty: result_ty,
        span,
    });
    let result = hir_pool.alloc_stmt(HirStmt {
        kind: HirStmtKind::Expr(indexed),
        span,
    });
    let statements = hir_pool.alloc_stmt_list(&[bind, result]);
    let block = hir_pool.alloc_block(HirBlock { statements, span });
    Ok(HirExprKind::Scope { block })
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
