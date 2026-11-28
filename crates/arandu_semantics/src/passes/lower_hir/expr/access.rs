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

pub(super) fn is_builtin_index_base(
    ty: &arandu_middle::types::ArType,
    symbols: &crate::SymbolTable,
) -> bool {
    use arandu_middle::types::ArType;
    matches!(
        ty,
        ArType::Array(_, _)
            | ArType::ConstArray(_, _)
            | ArType::Slice(_)
            | ArType::MutSlice(_)
            | ArType::Ptr(_)
            | ArType::Error
    ) || arandu_middle::types::is_vec_type(ty, symbols)
}

pub(super) fn peel_ref_ty_id(
    type_check: &TypeCheckResult,
    mut ty_id: arandu_middle::types::TypeId,
) -> arandu_middle::types::TypeId {
    use arandu_middle::types::ArType;
    let interner = &type_check.type_info.type_interner;
    for _ in 0..8 {
        match interner.resolve(ty_id) {
            ArType::Ref(inner) | ArType::RefMut(inner) => ty_id = inner,
            _ => break,
        }
    }
    ty_id
}

pub(in crate::passes::lower_hir) fn resolve_trait_index_sig(
    type_check: &mut TypeCheckResult,
    base_ty_id: arandu_middle::types::TypeId,
    mutable: bool,
) -> Option<(
    arandu_middle::types::TypeId,
    arandu_middle::types::TypeId,
    arandu_middle::types::TypeId,
)> {
    use arandu_middle::symbol_table::LangItem;
    use arandu_middle::types::ArType;

    let peeled_id = peel_ref_ty_id(type_check, base_ty_id);
    let actual_base_ty = type_check.type_info.type_interner.resolve(peeled_id);
    if is_builtin_index_base(&actual_base_ty, &type_check.symbols) {
        return None;
    }

    if let ArType::Named(param_sym, args) = &actual_base_ty
        && args.is_empty()
        && type_check
            .symbols
            .try_get(*param_sym)
            .is_some_and(|s| s.kind == arandu_middle::SymbolKind::TypeParam)
    {
        let target_lang = if mutable {
            LangItem::IndexMut
        } else {
            LangItem::Index
        };
        let (idx_ty, out_ty) = type_check
            .type_info
            .param_constraints
            .get(param_sym)
            .and_then(|bounds| {
                bounds.iter().find_map(|b| {
                    let is_target = type_check
                        .symbols
                        .try_get(b.iface_sym)
                        .is_some_and(|iface| iface.lang_item == Some(target_lang));
                    (is_target && b.type_args.len() == 2).then(|| (b.type_args[0], b.type_args[1]))
                })
            })?;
        let interner = &type_check.type_info.type_interner;
        let recv_ty = interner.intern(if mutable {
            ArType::RefMut(peeled_id)
        } else {
            ArType::Ref(peeled_id)
        });
        let ret_ty = interner.intern(if mutable {
            ArType::RefMut(out_ty)
        } else {
            ArType::Ref(out_ty)
        });
        let fn_ty = interner.intern(ArType::func(&[recv_ty, idx_ty], ret_ty, interner));
        return Some((fn_ty, ret_ty, out_ty));
    }

    let ArType::Named(type_sym, type_args) = actual_base_ty else {
        return None;
    };
    let method_name = if mutable { "indexMut" } else { "index" };
    let method_sym = type_check
        .symbols
        .lookup_associated_member(type_sym, method_name)?;
    let raw_ty = type_check.type_info.decl_type_id(method_sym)?;
    let arg_vec = type_check.type_info.type_interner.type_args(type_args);
    let inst_ty = if !arg_vec.is_empty() {
        let interner = &type_check.type_info.type_interner;
        let concrete_args: Vec<ArType> = arg_vec.iter().map(|&a| interner.resolve(a)).collect();
        let gp_opt = type_check
            .type_info
            .generic_params
            .get(&method_sym)
            .or_else(|| type_check.type_info.generic_params.get(&type_sym));
        if let Some(gp) = gp_opt {
            let n = gp.len().min(concrete_args.len());
            let subst = arandu_middle::types::build_subst(&gp[..n], &concrete_args[..n]);
            let resolved_raw = interner.resolve(raw_ty);
            interner.intern(arandu_middle::types::substitute_type(
                &resolved_raw,
                &subst,
                interner,
            ))
        } else {
            raw_ty
        }
    } else {
        raw_ty
    };
    let interner = &type_check.type_info.type_interner;
    let ArType::Func(_, ret_ty) = interner.resolve(inst_ty) else {
        return None;
    };
    let out_ty = match interner.resolve(ret_ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => inner,
        _ => return None,
    };
    Some((inst_ty, ret_ty, out_ty))
}

pub(in crate::passes::lower_hir) struct TraitIndexCallArgs {
    pub base_for_callee: crate::hir::HirExprId,
    pub base_for_arg: crate::hir::HirExprId,
    pub index_hir: crate::hir::HirExprId,
    pub fn_ty: arandu_middle::types::TypeId,
    pub ret_ref_ty: arandu_middle::types::TypeId,
    pub mutable: bool,
    pub span: arandu_middle::Span,
}

pub(in crate::passes::lower_hir) fn make_trait_index_call_from_hir(
    hir_pool: &mut crate::hir::HirPool,
    call: TraitIndexCallArgs,
) -> crate::hir::HirExpr {
    use crate::hir::{HirExpr, HirExprKind};
    let method_name = if call.mutable { "indexMut" } else { "index" };
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Field {
            base: call.base_for_callee,
            field: method_name.into(),
        },
        ty: call.fn_ty,
        span: call.span,
    });
    let args = hir_pool.alloc_expr_list(&[call.base_for_arg, call.index_hir]);
    HirExpr {
        kind: HirExprKind::Call { callee, args },
        ty: call.ret_ref_ty,
        span: call.span,
    }
}

pub(super) fn try_lower_trait_index_ref(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    base: ExprId,
    index: ExprId,
    mutable: bool,
    span: arandu_middle::Span,
) -> Result<Option<crate::hir::HirExprKind>, Diagnostic> {
    if super::range::bounds(type_check, pool, hir_pool, index)?.is_some() {
        return Ok(None);
    }
    let Some(base_ty_id) = type_check.type_info.expr_type_id(base) else {
        return Ok(None);
    };
    let Some((fn_ty, ret_ref_ty, _out_ty)) =
        resolve_trait_index_sig(type_check, base_ty_id, mutable)
    else {
        return Ok(None);
    };
    let base_callee = super::lower_expr(type_check, pool, hir_pool, base)?;
    let base_arg = super::lower_expr(type_check, pool, hir_pool, base)?;
    let index_hir = super::lower_expr(type_check, pool, hir_pool, index)?;
    let call_expr = make_trait_index_call_from_hir(
        hir_pool,
        TraitIndexCallArgs {
            base_for_callee: base_callee,
            base_for_arg: base_arg,
            index_hir,
            fn_ty,
            ret_ref_ty,
            mutable,
            span,
        },
    );
    Ok(Some(call_expr.kind))
}

pub(super) fn lower_index_with_mut(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    mutable: bool,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Index { base, index, .. } = pool.expr(expr) else {
        unreachable!();
    };
    let span = pool.expr_span(expr);
    if !super::range::needs_local(type_check, pool, *index)
        && super::range::bounds(type_check, pool, hir_pool, *index)?.is_none()
        && let Some(base_ty_id) = type_check.type_info.expr_type_id(*base)
        && let Some((fn_ty, ret_ref_ty, _out_ty)) =
            resolve_trait_index_sig(type_check, base_ty_id, mutable)
    {
        let base_callee = super::lower_expr(type_check, pool, hir_pool, *base)?;
        let base_arg = super::lower_expr(type_check, pool, hir_pool, *base)?;
        let index_hir = super::lower_expr(type_check, pool, hir_pool, *index)?;
        let call_expr = make_trait_index_call_from_hir(
            hir_pool,
            TraitIndexCallArgs {
                base_for_callee: base_callee,
                base_for_arg: base_arg,
                index_hir,
                fn_ty,
                ret_ref_ty,
                mutable,
                span,
            },
        );
        let call_id = hir_pool.alloc_expr(call_expr);
        return Ok(HirExprKind::Unary {
            op: arandu_middle::ops::UnaryOp::Deref,
            expr: call_id,
        });
    }
    lower_index(type_check, pool, hir_pool, expr)
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
    let span = pool.expr_span(expr);
    Ok({
        {
            // `xs[r]` with a range value that is not a plain name: `r` is read
            // twice, so it is bound to a local in a scope around the index.
            if super::range::needs_local(type_check, pool, *index) {
                return scoped_range_index(type_check, pool, hir_pool, expr, *base, *index);
            }
            if super::range::bounds(type_check, pool, hir_pool, *index)?.is_none()
                && let Some(base_ty_id) = type_check.type_info.expr_type_id(*base)
                && let Some((fn_ty, ret_ref_ty, _out_ty)) =
                    resolve_trait_index_sig(type_check, base_ty_id, false)
            {
                let base_callee = super::lower_expr(type_check, pool, hir_pool, *base)?;
                let base_arg = super::lower_expr(type_check, pool, hir_pool, *base)?;
                let index_hir = super::lower_expr(type_check, pool, hir_pool, *index)?;
                let call_expr = make_trait_index_call_from_hir(
                    hir_pool,
                    TraitIndexCallArgs {
                        base_for_callee: base_callee,
                        base_for_arg: base_arg,
                        index_hir,
                        fn_ty,
                        ret_ref_ty,
                        mutable: false,
                        span,
                    },
                );
                let call_id = hir_pool.alloc_expr(call_expr);
                return Ok(HirExprKind::Unary {
                    op: arandu_middle::ops::UnaryOp::Deref,
                    expr: call_id,
                });
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
