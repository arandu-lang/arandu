use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind};
use crate::passes::type_checker::types::Primitive;
use arandu_middle::types::{ArType, TypeInterner};
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
            if trailing_block.is_none()
                && let Some(kind) =
                    lower_owning_closure_call(type_check, pool, hir_pool, callee_id, arg_ids)?
            {
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            if trailing_block.is_none()
                && let Some(kind) =
                    lower_callback_call(type_check, pool, hir_pool, callee_id, arg_ids)?
            {
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
            let mut callee_vid = super::lower_expr(type_check, pool, hir_pool, callee_id)?;
            // A called generic path stays a path: the call instantiates it.
            if matches!(pool.expr(callee_id), ExprKind::Path { .. })
                && let HirExprKind::Generic { callee: path, .. } = hir_pool.expr(callee_vid).kind
            {
                callee_vid = path;
            }
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

/// `f(args)` on an implicit callback parameter `f: F` is `F.call(f, args)`.
/// Monomorphization replaces `F` with the closure struct, so the call is direct.
fn lower_callback_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    callee_id: ExprId,
    arg_ids: &[ExprId],
) -> Result<Option<HirExprKind>, Diagnostic> {
    let Some(callee_ty) = type_check.type_info.expr_type_id(callee_id) else {
        return Ok(None);
    };
    let interner = &type_check.type_info.type_interner;
    let callee = match interner.resolve(callee_ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => interner.resolve(inner),
        other => other,
    };
    let ArType::Named(implicit, _) = callee else {
        return Ok(None);
    };
    let Some(&bound) = type_check.type_info.callback_bounds.get(&implicit) else {
        return Ok(None);
    };
    let ArType::Func(params, ret) = interner.resolve(bound) else {
        return Err(Diagnostic::ice(
            crate::DiagCode::ICEL001,
            "callback bound is not a function type",
            pool.expr_span(callee_id),
        ));
    };
    let mut method_params = vec![callee_ty];
    method_params.extend(interner.type_args(params));
    let method = ArType::func(&method_params, ret, interner);
    let method_ty = type_check.type_info_mut().type_interner.intern(method);
    let base = super::lower_expr(type_check, pool, hir_pool, callee_id)?;
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Field {
            base,
            field: "call".into(),
        },
        ty: method_ty,
        span: pool.expr_span(callee_id),
    });
    let mut args = vec![super::lower_expr(type_check, pool, hir_pool, callee_id)?];
    for &arg in arg_ids {
        args.push(super::lower_expr(type_check, pool, hir_pool, arg)?);
    }
    Ok(Some(HirExprKind::Call {
        callee,
        args: hir_pool.alloc_expr_list(&args),
        trailing_block: None,
    }))
}

/// `c(args)` on an owning closure `c` is `c.code(c.env, args)`: its code takes
/// the environment first (RFC 0032, L3). The type checker only admits a place
/// as `c`, so reading it twice evaluates nothing twice.
fn lower_owning_closure_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    callee_id: ExprId,
    arg_ids: &[ExprId],
) -> Result<Option<HirExprKind>, Diagnostic> {
    use arandu_middle::symbol_table::LangItem;
    let Some(callee_ty) = type_check.type_info.expr_type_id(callee_id) else {
        return Ok(None);
    };
    let interner = &type_check.type_info.type_interner;
    let closure = match interner.resolve(callee_ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => interner.resolve(inner),
        other => other,
    };
    let ArType::Named(symbol, type_args) = closure else {
        return Ok(None);
    };
    if !type_check
        .symbols
        .try_get(symbol)
        .and_then(|symbol| symbol.lang_item)
        .is_some_and(|item| matches!(item, LangItem::Closure | LangItem::SendClosure))
    {
        return Ok(None);
    }
    let &[code_ty] = interner.type_args(type_args).as_slice() else {
        return Err(Diagnostic::ice(
            crate::DiagCode::ICEL001,
            "owning closure type has no code signature",
            pool.expr_span(callee_id),
        ));
    };
    let byte = interner.intern(ArType::Primitive(Primitive::U8));
    let env_ty = interner.intern(ArType::Ptr(byte));
    let span = pool.expr_span(callee_id);
    let mut field = |hir_pool: &mut crate::hir::HirPool, name: &str, ty| {
        super::lower_expr(type_check, pool, hir_pool, callee_id).map(|base| {
            hir_pool.alloc_expr(HirExpr {
                kind: HirExprKind::Field {
                    base,
                    field: name.into(),
                },
                ty,
                span,
            })
        })
    };
    let callee = field(hir_pool, "code", code_ty)?;
    let mut args = vec![field(hir_pool, "env", env_ty)?];
    for &arg in arg_ids {
        args.push(super::lower_expr(type_check, pool, hir_pool, arg)?);
    }
    Ok(Some(HirExprKind::Call {
        callee,
        args: hir_pool.alloc_expr_list(&args),
        trailing_block: None,
    }))
}
