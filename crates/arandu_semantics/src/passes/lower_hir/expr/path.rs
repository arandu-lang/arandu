use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind};
use crate::passes::lowering::{require_type_symbol, require_value_symbol};
use crate::passes::type_checker::types::{ArType, Primitive};
use arandu_middle::types::TypeInterner;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_path(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    expr: ExprId,
    span: arandu_middle::Span,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Path { .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let symbol = require_value_symbol(&type_check.resolved, expr, span)?;
            HirExprKind::Path { symbol }
        }
    })
}

pub(super) fn lower_variant_sugar(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::VariantSugar { name, args, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            // T2.2: lower like Result/Option/Poll ctors from the recorded expr type.
            let arg_ids = pool.expr_list(*args);
            let variant = match (type_check.type_info.expr_type(expr), name.as_str()) {
                (Some(ArType::Result(_, _)), "Ok") => Some(crate::hir::ResultCtorVariant::Ok),
                (Some(ArType::Result(_, _)), "Err") => Some(crate::hir::ResultCtorVariant::Err),
                (Some(ArType::Option(_)), "Some") => Some(crate::hir::ResultCtorVariant::Some),
                (Some(ArType::Option(_)), "None") => Some(crate::hir::ResultCtorVariant::None),
                (Some(ArType::Poll(_)), "Ready") => Some(crate::hir::ResultCtorVariant::PollReady),
                (Some(ArType::Poll(_)), "Pending") => {
                    Some(crate::hir::ResultCtorVariant::PollPending)
                }
                _ => None,
            };
            if let Some(variant) = variant {
                if arg_ids.len() == 1 {
                    let value_id = super::lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
                    HirExprKind::ResultCtor {
                        variant,
                        value: value_id,
                    }
                } else if arg_ids.is_empty()
                    && matches!(
                        variant,
                        crate::hir::ResultCtorVariant::None
                            | crate::hir::ResultCtorVariant::PollPending
                    )
                {
                    let dummy = hir_pool.alloc_expr(HirExpr {
                        kind: HirExprKind::Bool(false),
                        ty: TypeInterner::preinterned_primitive(Primitive::Bool),
                        span,
                    });
                    HirExprKind::ResultCtor {
                        variant,
                        value: dummy,
                    }
                } else {
                    return Err(Diagnostic::error(
                        crate::DiagCode::ICET001,
                        format!(
                            "variant sugar `.{}` has unexpected arity {}",
                            name,
                            arg_ids.len()
                        ),
                        span,
                    ));
                }
            } else if let Some(member_symbol) = type_check
                .resolved
                .value_refs
                .get(&crate::NodeKey::from(span))
                .copied()
                .or_else(|| type_check.resolved.expr_symbol(expr))
            {
                let type_symbol = match type_check.type_info.expr_type(expr) {
                    Some(ArType::Named(id, _)) => id,
                    _ => {
                        return Err(Diagnostic::error(
                            crate::DiagCode::ICET001,
                            format!("variant sugar `.{}` missing resolved enum type", name),
                            span,
                        ));
                    }
                };
                let path_kind = HirExprKind::TypePath {
                    type_symbol,
                    member_symbol,
                };
                if arg_ids.is_empty() {
                    path_kind
                } else {
                    let mut lowered_args = Vec::with_capacity(arg_ids.len());
                    for &arg_id in arg_ids {
                        lowered_args.push(super::lower_expr(type_check, pool, hir_pool, arg_id)?);
                    }
                    let callee_ty = type_check
                        .type_info
                        .decl_type_id(member_symbol)
                        .unwrap_or(fallback_ty);
                    let callee = hir_pool.alloc_expr(HirExpr {
                        kind: path_kind,
                        ty: callee_ty,
                        span,
                    });
                    let args_range = hir_pool.alloc_expr_list(&lowered_args);
                    HirExprKind::Call {
                        callee,
                        args: args_range,
                        trailing_block: None,
                    }
                }
            } else {
                return Err(Diagnostic::error(
                    crate::DiagCode::ICET001,
                    format!("unresolved variant sugar `.{}`", name),
                    span,
                ));
            }
        }
    })
}

pub(super) fn lower_type_path(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let ExprKind::TypePath {
        type_name, member, ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    let kind = {
        {
            if let Some(symbol) = type_check.resolved.expr_symbol(expr)
                && type_check.symbols.try_get(symbol).is_some_and(|entry| {
                    matches!(
                        entry.kind,
                        arandu_middle::SymbolKind::ConstParam | arandu_middle::SymbolKind::Const
                    )
                })
            {
                let base_ty = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or_else(super::error_ty);
                let base = hir_pool.alloc_expr(HirExpr {
                    kind: HirExprKind::Path { symbol },
                    ty: base_ty,
                    span: type_name.span,
                });
                return Ok(HirExpr {
                    kind: HirExprKind::Field {
                        base,
                        field: member.clone(),
                    },
                    ty: fallback_ty,
                    span,
                });
            }
            // Builtin unit ctors as bare TypePath (not Call): `Option.None`, `Poll.Pending`.
            let variant = super::builtin_ctor_variant_for_type_path(type_check, type_name, member);
            if let Some(variant) = variant.filter(|variant| {
                matches!(
                    variant,
                    crate::hir::ResultCtorVariant::None
                        | crate::hir::ResultCtorVariant::PollPending
                )
            }) {
                let dummy = hir_pool.alloc_expr(HirExpr {
                    kind: HirExprKind::Bool(false),
                    ty: TypeInterner::preinterned_primitive(Primitive::Bool),
                    span,
                });
                HirExprKind::ResultCtor {
                    variant,
                    value: dummy,
                }
            } else {
                let type_symbol = require_type_symbol(&type_check.resolved, type_name.span)?;
                let member_symbol = require_value_symbol(&type_check.resolved, expr, span)?;
                HirExprKind::TypePath {
                    type_symbol,
                    member_symbol,
                }
            }
        }
    };
    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}

pub(super) fn lower_generic(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Generic { callee, args, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let callee_id = super::lower_expr(type_check, pool, hir_pool, *callee)?;
            let mut hir_args = Vec::new();
            let arg_ids = pool.type_expr_list(*args);
            let interner = &mut std::sync::Arc::make_mut(&mut type_check.type_info).type_interner;
            for &arg_id in arg_ids {
                let ar = crate::passes::type_checker::types::lower_type_expr(
                    arg_id,
                    pool,
                    &type_check.symbols,
                    crate::ScopeId(0),
                    &type_check.resolved,
                    interner,
                );
                hir_args.push(interner.intern(ar));
            }
            HirExprKind::Generic {
                callee: callee_id,
                args: hir_args,
            }
        }
    })
}
