use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind};
use crate::passes::type_checker::types::Primitive;
use arandu_middle::types::TypeInterner;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let ExprKind::Call {
        callee,
        args,
        trailing_block,
        ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    let kind = {
        {
            let callee_id = *callee;
            let arg_ids = pool.expr_list(*args);
            if let Some(callee_sym) = super::get_resolved_value_ref(type_check, callee_id)
                && type_check.symbols.is_alloc_func(callee_sym)
                && let Some(&arg) = arg_ids.first()
            {
                let inner_id = super::lower_expr(type_check, pool, hir_pool, arg)?;
                let kind = HirExprKind::Alloc { expr: inner_id };
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            if trailing_block.is_none()
                && let Some(variant) = super::builtin_ctor_variant(type_check, pool, callee_id)
            {
                if arg_ids.len() == 1 {
                    let value_id = super::lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
                    let kind = HirExprKind::ResultCtor {
                        variant,
                        value: value_id,
                    };
                    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                    return Ok(HirExpr { kind, ty, span });
                }
                // Unit ctors: `Poll.Pending` / `Option.None` — dummy value ignored by AMIR.
                if arg_ids.is_empty()
                    && matches!(
                        variant,
                        crate::hir::ResultCtorVariant::PollPending
                            | crate::hir::ResultCtorVariant::None
                    )
                {
                    let dummy = hir_pool.alloc_expr(HirExpr {
                        kind: HirExprKind::Bool(false),
                        ty: TypeInterner::preinterned_primitive(Primitive::Bool),
                        span,
                    });
                    let kind = HirExprKind::ResultCtor {
                        variant,
                        value: dummy,
                    };
                    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                    return Ok(HirExpr { kind, ty, span });
                }
            }
            // ToStr v0.1: `receiver.to_str()` → HirExprKind::ToStr (not a real method call).
            if trailing_block.is_none()
                && arg_ids.is_empty()
                && let ExprKind::Field { base, field, .. } | ExprKind::SafeField { base, field, .. } =
                    pool.expr(callee_id)
                && field == "to_str"
            {
                let value_id = super::lower_expr(type_check, pool, hir_pool, *base)?;
                let kind = HirExprKind::ToStr { value: value_id };
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            // Method receiver is not in the AST arg list. Prepend it for
            // `obj.m(...)` and generic `obj.m<T>(...)` so AMIR/mono see
            // (self, …) matching the method Func type.
            let method_base = {
                let field_callee = match pool.expr(callee_id) {
                    ExprKind::Field { .. } | ExprKind::SafeField { .. } => Some(callee_id),
                    ExprKind::Generic { callee: inner, .. } => match pool.expr(*inner) {
                        ExprKind::Field { .. } | ExprKind::SafeField { .. } => Some(*inner),
                        _ => None,
                    },
                    _ => None,
                };
                field_callee.and_then(|fc| match pool.expr(fc) {
                    ExprKind::Field { base, field, .. }
                    | ExprKind::SafeField { base, field, .. } => {
                        if super::lookup_namespace_field(pool, *base, field, type_check).is_some() {
                            None
                        } else {
                            Some(*base)
                        }
                    }
                    _ => None,
                })
            };
            let callee_vid = super::lower_expr(type_check, pool, hir_pool, callee_id)?;
            let mut hir_args = Vec::new();
            if let Some(base_id) = method_base {
                hir_args.push(super::lower_expr(type_check, pool, hir_pool, base_id)?);
            }
            for &arg_id in arg_ids {
                hir_args.push(super::lower_expr(type_check, pool, hir_pool, arg_id)?);
            }
            let hir_trailing = trailing_block
                .as_ref()
                .map(|b| {
                    super::super::stmt::lower_block(type_check, pool, hir_pool, pool.block(*b))
                })
                .transpose()?;
            let args_range = hir_pool.alloc_expr_list(&hir_args);
            HirExprKind::Call {
                callee: callee_vid,
                args: args_range,
                trailing_block: hir_trailing,
            }
        }
    };
    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}
