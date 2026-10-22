use arandu_diagnostics::Diagnostic;
use arandu_middle::hir::{
    HirExpr, HirExprId, HirExprKind, HirLambdaBody, HirLambdaParam, HirPlace, HirPlaceSuffix,
    HirProgram, HirStringPart,
};
use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_middle::types::{GenericSubst, substitute_type_id};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

pub(in crate::passes::monomorphize::expand) fn clone_place(
    hir: &mut HirProgram,
    place: &HirPlace,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirPlace, Diagnostic> {
    let root = *symbol_map
        .get(&place.root_symbol)
        .unwrap_or(&place.root_symbol);
    let mut suffixes = place.suffixes.clone();
    suffixes.clear();
    for s in &place.suffixes {
        suffixes.push(match s {
            HirPlaceSuffix::Field {
                span,
                name,
                field_symbol,
                ty,
            } => HirPlaceSuffix::Field {
                span: *span,
                name: name.clone(),
                field_symbol: *field_symbol,
                ty: substitute_type_id(*ty, subst, &tc.type_info.type_interner),
            },
            HirPlaceSuffix::Index { span, expr, ty } => HirPlaceSuffix::Index {
                span: *span,
                expr: clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
                ty: substitute_type_id(*ty, subst, &tc.type_info.type_interner),
            },
        });
    }
    Ok(HirPlace {
        root_symbol: root,
        suffixes,
        ty: substitute_type_id(place.ty, subst, &tc.type_info.type_interner),
        span: place.span,
    })
}

pub(in crate::passes::monomorphize::expand) fn clone_expr(
    hir: &mut HirProgram,
    expr_id: HirExprId,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirExprId, Diagnostic> {
    let expr = hir.pool.expr(expr_id).clone();
    let new_ty = substitute_type_id(expr.ty, subst, &tc.type_info.type_interner);
    let kind = match expr.kind {
        HirExprKind::Path { symbol } if tc.symbols.get(symbol).kind == SymbolKind::ConstParam => {
            match subst
                .iter()
                .find_map(|(param, value)| (*param == symbol).then_some(value))
            {
                Some(arandu_middle::types::ArType::FrozenConst(value)) => {
                    let mut value = value.as_ref().clone();
                    if let arandu_middle::ctfe::ConstValue::Integer(integer) = value
                        && let arandu_middle::types::ArType::Primitive(primitive) =
                            tc.type_info.type_interner.resolve(new_ty)
                    {
                        value = arandu_middle::ctfe::ConstValue::Integer(
                            arandu_middle::ctfe::IntegerType::new(
                                primitive,
                                tc.type_info.target_layout,
                            )
                            .ok()
                            .and_then(|ty| integer.cast(ty).ok())
                            .ok_or_else(|| {
                                Diagnostic::ice(
                                    arandu_middle::DiagCode::ICET001,
                                    "constant parameter exceeds its declared integer type",
                                    expr.span,
                                )
                            })?,
                        );
                    }
                    crate::passes::lower_hir::materialize_ctfe_value(
                        &value,
                        new_ty,
                        &tc.type_info,
                        &mut hir.pool,
                        tc.type_info.target_layout,
                        expr.span,
                    )
                    .map_err(|_| {
                        Diagnostic::ice(
                            arandu_middle::DiagCode::ICET001,
                            "constant parameter disagrees with its declared type",
                            expr.span,
                        )
                    })?
                    .kind
                }
                Some(arandu_middle::types::ArType::Const(value)) => {
                    HirExprKind::Int(value.to_string().into())
                }
                _ => HirExprKind::Path { symbol },
            }
        }
        ref kind => clone_expr_kind(hir, kind, subst, symbol_map, tc, name_prefix)?,
    };
    Ok(hir.pool.alloc_expr(HirExpr {
        kind,
        ty: new_ty,
        span: expr.span,
    }))
}

pub(in crate::passes::monomorphize::expand) fn clone_expr_kind(
    hir: &mut HirProgram,
    kind: &HirExprKind,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirExprKind, Diagnostic> {
    use HirExprKind::*;
    Ok(match kind {
        FloatBits(value) => FloatBits(*value),
        FrozenBytes(value) => FrozenBytes(value.clone()),
        Path { symbol } => Path {
            symbol: *symbol_map.get(symbol).unwrap_or(symbol),
        },
        TypePath {
            type_symbol,
            member_symbol,
        } => TypePath {
            type_symbol: *type_symbol,
            member_symbol: *member_symbol,
        },
        Generic { callee, args } => Generic {
            callee: clone_expr(hir, *callee, subst, symbol_map, tc, name_prefix)?,
            args: args
                .iter()
                .map(|&a| substitute_type_id(a, subst, &tc.type_info.type_interner))
                .collect(),
        },
        Field { base, field } => Field {
            base: clone_expr(hir, *base, subst, symbol_map, tc, name_prefix)?,
            field: field.clone(),
        },
        SafeField { base, field } => SafeField {
            base: clone_expr(hir, *base, subst, symbol_map, tc, name_prefix)?,
            field: field.clone(),
        },
        Index { base, index } => Index {
            base: clone_expr(hir, *base, subst, symbol_map, tc, name_prefix)?,
            index: clone_expr(hir, *index, subst, symbol_map, tc, name_prefix)?,
        },
        SafeIndex { base, index } => SafeIndex {
            base: clone_expr(hir, *base, subst, symbol_map, tc, name_prefix)?,
            index: clone_expr(hir, *index, subst, symbol_map, tc, name_prefix)?,
        },
        Call {
            callee,
            args,
            trailing_block,
        } => {
            let old_args: Vec<_> = hir.pool.expr_list(*args).to_vec();
            let mut new_args = Vec::with_capacity(old_args.len());
            for &a in &old_args {
                new_args.push(clone_expr(hir, a, subst, symbol_map, tc, name_prefix)?);
            }
            Call {
                callee: clone_expr(hir, *callee, subst, symbol_map, tc, name_prefix)?,
                args: hir.pool.alloc_expr_list(&new_args),
                trailing_block: trailing_block
                    .map(|b| super::clone_block(hir, b, subst, symbol_map, tc, name_prefix))
                    .transpose()?,
            }
        }
        StructLiteral {
            struct_symbol,
            fields,
        } => {
            let old_f: Vec<_> = hir.pool.field_inits_list(*fields).to_vec();
            let mut new_f = Vec::with_capacity(old_f.len());
            for f in &old_f {
                new_f.push(arandu_middle::hir::HirFieldInit {
                    span: f.span,
                    name: f.name.clone(),
                    value: clone_expr(hir, f.value, subst, symbol_map, tc, name_prefix)?,
                });
            }
            StructLiteral {
                struct_symbol: *struct_symbol,
                fields: hir.pool.alloc_field_init_list(&new_f),
            }
        }
        Array { items } => {
            let old: Vec<_> = hir.pool.expr_list(*items).to_vec();
            let mut new_items = Vec::with_capacity(old.len());
            for &e in &old {
                new_items.push(clone_expr(hir, e, subst, symbol_map, tc, name_prefix)?);
            }
            Array {
                items: hir.pool.alloc_expr_list(&new_items),
            }
        }
        ArrayRepeat { value } => ArrayRepeat {
            value: clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?,
        },
        Tuple { items } => {
            let old = hir.pool.expr_list(*items).to_vec();
            let mut new_items = Vec::with_capacity(old.len());
            for expression in old {
                new_items.push(clone_expr(
                    hir,
                    expression,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            Tuple {
                items: hir.pool.alloc_expr_list(&new_items),
            }
        }
        Lambda { params, body } => {
            let old_p: Vec<_> = hir.pool.lambda_params_list(*params).to_vec();
            let mut new_p = Vec::with_capacity(old_p.len());
            for p in &old_p {
                let new_ty = substitute_type_id(p.ty, subst, &tc.type_info.type_interner);
                let new_sym = super::fresh_symbol(
                    tc,
                    &format!("{name_prefix}$lam_{}", tc.symbols.get(p.symbol).name),
                    SymbolKind::Param,
                    p.span,
                );
                symbol_map.insert(p.symbol, new_sym);
                tc.type_info_mut().record_decl_type(new_sym, new_ty);
                new_p.push(HirLambdaParam {
                    span: p.span,
                    symbol: new_sym,
                    ty: new_ty,
                });
            }
            let body = match body {
                HirLambdaBody::Expr(e) => {
                    HirLambdaBody::Expr(clone_expr(hir, *e, subst, symbol_map, tc, name_prefix)?)
                }
                HirLambdaBody::Block(b) => HirLambdaBody::Block(super::clone_block(
                    hir,
                    *b,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?),
            };
            Lambda {
                params: hir.pool.alloc_lambda_param_list(&new_p),
                body,
            }
        }
        Alloc { expr } => Alloc {
            expr: clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
        },
        AsyncBlock { block } => AsyncBlock {
            block: super::clone_block(hir, *block, subst, symbol_map, tc, name_prefix)?,
        },
        ValueBlock { block } => ValueBlock {
            block: super::clone_block(hir, *block, subst, symbol_map, tc, name_prefix)?,
        },
        UnsafeBlock { block } => UnsafeBlock {
            block: super::clone_block(hir, *block, subst, symbol_map, tc, name_prefix)?,
        },
        If {
            condition,
            then_block,
            else_block,
        } => If {
            condition: super::clone_condition(hir, condition, subst, symbol_map, tc, name_prefix)?,
            then_block: super::clone_block(hir, *then_block, subst, symbol_map, tc, name_prefix)?,
            else_block: super::clone_block(hir, *else_block, subst, symbol_map, tc, name_prefix)?,
        },
        Match { value, arms } => {
            let value = clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?;
            let old_arms: Vec<_> = hir.pool.match_arms_list(*arms).to_vec();
            let mut new_arms = Vec::with_capacity(old_arms.len());
            for arm in &old_arms {
                new_arms.push(super::clone_match_arm(
                    hir,
                    arm,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            Match {
                value,
                arms: hir.pool.alloc_match_arm_list(&new_arms),
            }
        }
        Catch { expr, handler } => Catch {
            expr: clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
            handler: super::clone_catch_handler(hir, handler, subst, symbol_map, tc, name_prefix)?,
        },
        NullCoalesce { left, right } => NullCoalesce {
            left: clone_expr(hir, *left, subst, symbol_map, tc, name_prefix)?,
            right: clone_expr(hir, *right, subst, symbol_map, tc, name_prefix)?,
        },
        Layout { query, operand_ty } => Layout {
            query: *query,
            operand_ty: substitute_type_id(*operand_ty, subst, &tc.type_info.type_interner),
        },
        Cast { expr, target_ty } => Cast {
            expr: clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
            target_ty: substitute_type_id(*target_ty, subst, &tc.type_info.type_interner),
        },
        Unary { op, expr } => Unary {
            op: *op,
            expr: clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
        },
        Binary { op, left, right } => Binary {
            op: *op,
            left: clone_expr(hir, *left, subst, symbol_map, tc, name_prefix)?,
            right: clone_expr(hir, *right, subst, symbol_map, tc, name_prefix)?,
        },
        Int(v) => Int(v.clone()),
        Float(v) => Float(v.clone()),
        Bool(v) => Bool(*v),
        Char(v) => Char(v.clone()),
        Str(v) => Str(v.clone()),
        StringInterp { parts } => {
            let mut new_parts = Vec::with_capacity(parts.len());
            for p in parts {
                new_parts.push(match p {
                    HirStringPart::Text(t) => HirStringPart::Text(t.clone()),
                    HirStringPart::Expr(e) => HirStringPart::Expr(clone_expr(
                        hir,
                        *e,
                        subst,
                        symbol_map,
                        tc,
                        name_prefix,
                    )?),
                });
            }
            StringInterp { parts: new_parts }
        }
        ToStr { value } => ToStr {
            value: clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?,
        },
        Nil => Nil,
        Error => Error,
        ResultCtor { variant, value } => ResultCtor {
            variant: *variant,
            value: clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?,
        },
        Try { expr } => Try {
            expr: clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
        },
    })
}
