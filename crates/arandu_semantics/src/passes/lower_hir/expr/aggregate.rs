use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind, HirFieldInit, HirLambdaBody, HirLambdaParam};
use crate::passes::lowering::require_def_symbol;
use crate::passes::type_checker::types::ArType;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_struct_literal(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::StructLiteral { ty, fields, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let resolved_variant_sym = match pool.type_expr(*ty) {
                arandu_parser::TypeExpr::Named { name, .. } => type_check
                    .resolved
                    .type_refs
                    .get(&crate::NodeKey::from(name.span))
                    .copied()
                    .filter(|sym| type_check.type_info.enum_variants.contains_key(sym)),
                _ => None,
            };
            let struct_symbol = if let Some(variant_sym) = resolved_variant_sym {
                variant_sym
            } else {
                match type_check.type_info.type_interner.resolve(fallback_ty) {
                    ArType::Named(id, _) => id,
                    _ => {
                        return Err(Diagnostic::error(
                            crate::diagnostics::DiagCode::L001LoweringUnresolvedSymbol,
                            "cannot lower struct literal: type is not a named struct",
                            span,
                        ));
                    }
                }
            };
            let field_ids = pool.field_init_list(*fields);
            let mut hir_fields = Vec::new();
            let mut base_value_id = None;
            let mut explicit_field_names = Vec::new();

            for &fid in field_ids {
                let f = pool.field_init(fid);
                if f.name == ".." {
                    base_value_id = Some(super::lower_expr(type_check, pool, hir_pool, f.value)?);
                } else {
                    let value_id = super::lower_expr(type_check, pool, hir_pool, f.value)?;
                    explicit_field_names.push(f.name.clone());
                    hir_fields.push(HirFieldInit {
                        span: f.span,
                        name: f.name.clone(),
                        value: value_id,
                    });
                }
            }

            if let Some(defined_fields) = type_check
                .type_info
                .struct_fields
                .get(&struct_symbol)
                .cloned()
            {
                if let Some(base_vid) = base_value_id {
                    for f in defined_fields.iter() {
                        let smol_name = f.name.clone();
                        if !explicit_field_names.contains(&smol_name) {
                            let field_expr = hir_pool.alloc_expr(HirExpr {
                                kind: HirExprKind::Field {
                                    base: base_vid,
                                    field: smol_name.clone(),
                                },
                                ty: f.ty,
                                span,
                            });
                            hir_fields.push(HirFieldInit {
                                span,
                                name: smol_name,
                                value: field_expr,
                            });
                        }
                    }
                } else {
                    for f in defined_fields.iter() {
                        let smol_name = f.name.clone();
                        if !explicit_field_names.contains(&smol_name)
                            && let Some(&default_expr) = type_check
                                .type_info
                                .struct_field_defaults
                                .get(&(struct_symbol, smol_name.clone()))
                        {
                            let value_id =
                                super::lower_expr(type_check, pool, hir_pool, default_expr)?;
                            hir_fields.push(HirFieldInit {
                                span,
                                name: smol_name,
                                value: value_id,
                            });
                        }
                    }
                }
            }

            let fields_range = hir_pool.alloc_field_init_list(&hir_fields);
            HirExprKind::StructLiteral {
                struct_symbol,
                fields: fields_range,
            }
        }
    })
}

pub(super) fn lower_array(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Array { items, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let item_ids = pool.expr_list(*items);
            let mut hir_items = Vec::new();
            for &i in item_ids {
                hir_items.push(super::lower_expr(type_check, pool, hir_pool, i)?);
            }
            let items_range = hir_pool.alloc_expr_list(&hir_items);
            HirExprKind::Array { items: items_range }
        }
    })
}

pub(super) fn lower_array_repeat(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::ArrayRepeat { value, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        HirExprKind::ArrayRepeat {
            value: super::lower_expr(type_check, pool, hir_pool, *value)?,
        }
    })
}

pub(super) fn lower_lambda(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Lambda { params, body, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            if let Some(recipe) = type_check.type_info.thread_capture_at(span).cloned() {
                super::super::thread_capture::capture_struct_literal(hir_pool, &recipe)
            } else {
                let mut hir_params = Vec::new();
                let param_ids = pool.lambda_param_list(*params);
                for &pid in param_ids {
                    let p = pool.lambda_param(pid);
                    let symbol = require_def_symbol(&type_check.resolved, p.span)?;
                    let p_ty = type_check
                        .type_info
                        .decl_type_id(symbol)
                        .unwrap_or_else(super::error_ty);
                    hir_params.push(HirLambdaParam {
                        span: p.span,
                        symbol,
                        ty: p_ty,
                    });
                }
                let params_range = hir_pool.alloc_lambda_param_list(&hir_params);
                let hir_body = match body {
                    arandu_parser::LambdaBody::Expr {
                        expr: inner_expr, ..
                    } => {
                        let eid = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
                        HirLambdaBody::Expr(eid)
                    }
                    arandu_parser::LambdaBody::Block { block, .. } => HirLambdaBody::Block(
                        super::super::stmt::lower_block(type_check, pool, hir_pool, block)?,
                    ),
                };
                HirExprKind::Lambda {
                    params: params_range,
                    body: hir_body,
                }
            }
        }
    })
}

pub(super) fn lower_alloc(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Alloc {
        expr: inner_expr, ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    Ok({
        {
            let inner_id = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Alloc { expr: inner_id }
        }
    })
}

pub(super) fn lower_async_block(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::AsyncBlock { block, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        HirExprKind::AsyncBlock {
            block: super::super::stmt::lower_block(type_check, pool, hir_pool, pool.block(*block))?,
        }
    })
}

pub(super) fn lower_unsafe_block(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::UnsafeBlock { block, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        HirExprKind::UnsafeBlock {
            block: super::super::stmt::lower_block(type_check, pool, hir_pool, pool.block(*block))?,
        }
    })
}
