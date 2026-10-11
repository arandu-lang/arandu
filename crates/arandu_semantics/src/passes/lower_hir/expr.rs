use crate::diagnostics::Diagnostic;
use crate::hir::{
    HirCatchHandler, HirExpr, HirExprKind, HirFieldInit, HirLambdaBody, HirLambdaParam,
    HirStringPart,
};
use crate::passes::lowering::{require_def_symbol, require_type_symbol, require_value_symbol};
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{NodeKey, TypeCheckResult};
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_parser::CatchHandler;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

fn get_resolved_value_ref(
    type_check: &mut TypeCheckResult,
    expr: ExprId,
) -> Option<crate::symbol_table::SymbolId> {
    type_check.resolved.expr_symbol(expr)
}

fn lookup_namespace_field(
    pool: &AstPool,
    base: ExprId,
    field: &str,
    type_check: &mut TypeCheckResult,
) -> Option<crate::symbol_table::SymbolId> {
    let ExprKind::Path { path, .. } = pool.expr(base) else {
        return None;
    };
    if path.len() != 1 {
        return None;
    }
    type_check.symbols.lookup_module_member(&path[0], field)
}

fn builtin_ctor_variant(
    type_check: &TypeCheckResult,
    pool: &AstPool,
    callee: ExprId,
) -> Option<crate::hir::ResultCtorVariant> {
    let ExprKind::TypePath {
        type_name, member, ..
    } = pool.expr(callee)
    else {
        return None;
    };
    builtin_ctor_variant_for_type_path(type_check, type_name, member)
}

fn builtin_ctor_variant_for_type_path(
    type_check: &TypeCheckResult,
    type_name: &arandu_parser::TypeName,
    member: &str,
) -> Option<crate::hir::ResultCtorVariant> {
    let symbol = type_check
        .resolved
        .type_refs
        .get(&NodeKey::from(type_name.span))
        .copied()?;
    let item = type_check.symbols.get(symbol).lang_item?;
    use arandu_middle::symbol_table::LangItem;
    match (item, member) {
        (LangItem::Result, "Ok") => Some(crate::hir::ResultCtorVariant::Ok),
        (LangItem::Result, "Err") => Some(crate::hir::ResultCtorVariant::Err),
        (LangItem::Option, "Some") => Some(crate::hir::ResultCtorVariant::Some),
        (LangItem::Option, "None") => Some(crate::hir::ResultCtorVariant::None),
        (LangItem::Poll, "Ready") => Some(crate::hir::ResultCtorVariant::PollReady),
        (LangItem::Poll, "Pending") => Some(crate::hir::ResultCtorVariant::PollPending),
        _ => None,
    }
}

fn error_ty() -> TypeId {
    TypeInterner::preinterned_error_id()
}

fn expr_type_for_kind(
    type_check: &mut TypeCheckResult,
    hir_pool: &crate::hir::HirPool,
    kind: &HirExprKind,
    fallback: TypeId,
) -> TypeId {
    let interner = &type_check.type_info.type_interner;
    match kind {
        HirExprKind::Error => error_ty(),
        HirExprKind::Str(_) | HirExprKind::StringInterp { .. } | HirExprKind::ToStr { .. } => {
            TypeInterner::preinterned_primitive(Primitive::Str)
        }
        HirExprKind::Int(_) => {
            if fallback != error_ty() && fallback != interner.intern(ArType::IntLiteral) {
                fallback
            } else {
                interner.intern(ArType::IntLiteral)
            }
        }
        HirExprKind::Float(_) => {
            if matches!(
                interner.resolve(fallback),
                ArType::Primitive(Primitive::Float | Primitive::F32 | Primitive::F64)
            ) {
                fallback
            } else {
                interner.intern(ArType::FloatLiteral)
            }
        }
        HirExprKind::FloatBits(value) => {
            TypeInterner::preinterned_primitive(value.ty().primitive())
        }
        HirExprKind::FrozenBytes(_) => fallback,
        HirExprKind::Bool(_) => TypeInterner::preinterned_primitive(Primitive::Bool),
        HirExprKind::Char(_) => TypeInterner::preinterned_primitive(Primitive::Char),
        HirExprKind::Nil => {
            if fallback == error_ty() {
                let err_id = interner.intern(ArType::Error);
                interner.intern(ArType::Nullable(err_id))
            } else {
                fallback
            }
        }
        // Prefer typeck's recorded type (`fallback`) over `decl_type`: for
        // generic free funcs typeck specializes `join_g` → `Func(..., int)` on
        // the Path expr; `decl_type` stays the template `Func(..., T)`. Using
        // decl_type here made mono see identity `T` and skip specialization.
        HirExprKind::Path { symbol }
        | HirExprKind::TypePath {
            member_symbol: symbol,
            ..
        } => {
            if fallback != error_ty() {
                return fallback;
            }
            type_check
                .type_info
                .decl_type_id(*symbol)
                .filter(|&id| id != error_ty())
                .unwrap_or(fallback)
        }
        HirExprKind::Call { callee, .. } => {
            // Prefer typeck call type (instantiated return) over deriving from
            // a still-generic callee formal (`T`).
            if fallback != error_ty() {
                return fallback;
            }
            let callee_expr = hir_pool.expr(*callee);
            let interner = &type_check.type_info.type_interner;
            if callee_expr.ty != error_ty() {
                return match interner.resolve(callee_expr.ty) {
                    ArType::Func(_, ret) => ret,
                    _ => callee_expr.ty,
                };
            }
            match &callee_expr.kind {
                HirExprKind::Path { symbol }
                | HirExprKind::TypePath {
                    member_symbol: symbol,
                    ..
                } => type_check
                    .type_info
                    .decl_type_id(*symbol)
                    .and_then(|id| match interner.resolve(id) {
                        ArType::Func(_, ret) => Some(ret),
                        _ => None,
                    })
                    .unwrap_or(fallback),
                _ => fallback,
            }
        }
        HirExprKind::ResultCtor { .. } => fallback,
        _ => fallback,
    }
}

pub(crate) fn lower_expr_raw(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExpr, Diagnostic> {
    let span = pool.expr_span(expr);
    let fallback_ty = type_check
        .type_info
        .expr_type_id(expr)
        .unwrap_or_else(error_ty);

    if !hir_pool.ctfe_lowering
        && let Some((value, layout)) = type_check.type_info.ctfe_values.get(&span)
    {
        return super::materialize::materialize_ctfe_value(
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
                "frozen initializer disagrees with its checked type",
                span,
            )
        });
    }
    // Recursive binary/unary/group lowering uses a small coordinator frame;
    // Keep the other forms' diagnostic locals off those ancestor frames.
    let kind = match pool.expr(expr) {
        ExprKind::Group { expr: inner, .. } => {
            return lower_expr_raw(type_check, pool, hir_pool, *inner);
        }
        ExprKind::Binary { op, left, right } => {
            let left = lower_expr(type_check, pool, hir_pool, *left)?;
            let right = lower_expr(type_check, pool, hir_pool, *right)?;
            HirExprKind::Binary {
                op: (*op).into(),
                left,
                right,
            }
        }
        ExprKind::Unary { op, expr: inner } => {
            let inner = lower_expr(type_check, pool, hir_pool, *inner)?;
            HirExprKind::Unary {
                op: (*op).into(),
                expr: inner,
            }
        }
        _ => return lower_expr_dispatch(type_check, pool, hir_pool, expr, span, fallback_ty),
    };
    let ty = expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}

fn lower_expr_dispatch(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let kind = match pool.expr(expr) {
        ExprKind::Layout { query, ty } => match query {
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
        },
        ExprKind::Comptime { body } if hir_pool.ctfe_lowering => match body {
            arandu_parser::ast_pool::ComptimeBody::Expression(inner) => {
                return lower_expr_raw(type_check, pool, hir_pool, *inner);
            }
            arandu_parser::ast_pool::ComptimeBody::Block(block) => {
                let source = pool.block(*block);
                let mut lowered = super::stmt::lower_block_raw(type_check, pool, hir_pool, source)?;
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
        },
        ExprKind::Comptime { .. } => {
            let Some((value, layout)) = type_check.type_info.ctfe_values.get(&span) else {
                return Err(Diagnostic::error(crate::DiagCode::T042UnsupportedComptime,
                    "this comptime root has not been evaluated; nested staging in CTFE helpers is not supported yet", span)
                    .with_primary_label("compile-time evaluation required"));
            };
            return super::materialize::materialize_ctfe_value(
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
            });
        }
        ExprKind::Path { .. } => {
            let symbol = require_value_symbol(&type_check.resolved, expr, span)?;
            HirExprKind::Path { symbol }
        }
        ExprKind::VariantSugar { name, args, .. } => {
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
                    let value_id = lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
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
                        lowered_args.push(lower_expr(type_check, pool, hir_pool, arg_id)?);
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
        ExprKind::TypePath {
            type_name, member, ..
        } => {
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
                    .unwrap_or_else(error_ty);
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
            let variant = builtin_ctor_variant_for_type_path(type_check, type_name, member);
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
        ExprKind::Generic { callee, args, .. } => {
            let callee_id = lower_expr(type_check, pool, hir_pool, *callee)?;
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
        ExprKind::Field { base, field, .. } => {
            let base_id = *base;
            if let Some(symbol) = lookup_namespace_field(pool, base_id, field, type_check)
                .or_else(|| get_resolved_value_ref(type_check, expr))
            {
                HirExprKind::Path { symbol }
            } else {
                let base_vid = lower_expr(type_check, pool, hir_pool, base_id)?;
                HirExprKind::Field {
                    base: base_vid,
                    field: field.clone(),
                }
            }
        }
        ExprKind::SafeField { base, field, .. } => {
            let base_id = *base;
            if let Some(symbol) = lookup_namespace_field(pool, base_id, field, type_check)
                .or_else(|| get_resolved_value_ref(type_check, expr))
            {
                HirExprKind::Path { symbol }
            } else {
                let base_vid = lower_expr(type_check, pool, hir_pool, base_id)?;
                HirExprKind::SafeField {
                    base: base_vid,
                    field: field.clone(),
                }
            }
        }
        ExprKind::Index { base, index, .. } => {
            let base_id = lower_expr(type_check, pool, hir_pool, *base)?;
            let index_id = lower_expr(type_check, pool, hir_pool, *index)?;
            HirExprKind::Index {
                base: base_id,
                index: index_id,
            }
        }
        ExprKind::SafeIndex { base, index, .. } => {
            let base_id = lower_expr(type_check, pool, hir_pool, *base)?;
            let index_id = lower_expr(type_check, pool, hir_pool, *index)?;
            HirExprKind::SafeIndex {
                base: base_id,
                index: index_id,
            }
        }
        ExprKind::Try {
            expr: inner_expr, ..
        } => {
            let inner_id = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Try { expr: inner_id }
        }
        ExprKind::Call {
            callee,
            args,
            trailing_block,
            ..
        } => {
            let callee_id = *callee;
            let arg_ids = pool.expr_list(*args);
            if let Some(callee_sym) = get_resolved_value_ref(type_check, callee_id)
                && type_check.symbols.is_alloc_func(callee_sym)
                && let Some(&arg) = arg_ids.first()
            {
                let inner_id = lower_expr(type_check, pool, hir_pool, arg)?;
                let kind = HirExprKind::Alloc { expr: inner_id };
                let ty = expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            if trailing_block.is_none()
                && let Some(variant) = builtin_ctor_variant(type_check, pool, callee_id)
            {
                if arg_ids.len() == 1 {
                    let value_id = lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
                    let kind = HirExprKind::ResultCtor {
                        variant,
                        value: value_id,
                    };
                    let ty = expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
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
                    let ty = expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
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
                let value_id = lower_expr(type_check, pool, hir_pool, *base)?;
                let kind = HirExprKind::ToStr { value: value_id };
                let ty = expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
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
                        if lookup_namespace_field(pool, *base, field, type_check).is_some() {
                            None
                        } else {
                            Some(*base)
                        }
                    }
                    _ => None,
                })
            };
            let callee_vid = lower_expr(type_check, pool, hir_pool, callee_id)?;
            let mut hir_args = Vec::new();
            if let Some(base_id) = method_base {
                hir_args.push(lower_expr(type_check, pool, hir_pool, base_id)?);
            }
            for &arg_id in arg_ids {
                hir_args.push(lower_expr(type_check, pool, hir_pool, arg_id)?);
            }
            let hir_trailing = trailing_block
                .as_ref()
                .map(|b| super::stmt::lower_block(type_check, pool, hir_pool, pool.block(*b)))
                .transpose()?;
            let args_range = hir_pool.alloc_expr_list(&hir_args);
            HirExprKind::Call {
                callee: callee_vid,
                args: args_range,
                trailing_block: hir_trailing,
            }
        }
        ExprKind::StructLiteral { ty, fields, .. } => {
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
                    base_value_id = Some(lower_expr(type_check, pool, hir_pool, f.value)?);
                } else {
                    let value_id = lower_expr(type_check, pool, hir_pool, f.value)?;
                    explicit_field_names.push(f.name.clone());
                    hir_fields.push(HirFieldInit {
                        span: f.span,
                        name: f.name.clone(),
                        value: value_id,
                    });
                }
            }

            if let Some(base_vid) = base_value_id
                && let Some(defined_fields) = type_check
                    .type_info
                    .struct_fields
                    .get(&struct_symbol)
                    .cloned()
            {
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
            }

            let fields_range = hir_pool.alloc_field_init_list(&hir_fields);
            HirExprKind::StructLiteral {
                struct_symbol,
                fields: fields_range,
            }
        }
        ExprKind::Array { items, .. } => {
            let item_ids = pool.expr_list(*items);
            let mut hir_items = Vec::new();
            for &i in item_ids {
                hir_items.push(lower_expr(type_check, pool, hir_pool, i)?);
            }
            let items_range = hir_pool.alloc_expr_list(&hir_items);
            HirExprKind::Array { items: items_range }
        }
        ExprKind::ArrayRepeat { value, .. } => HirExprKind::ArrayRepeat {
            value: lower_expr(type_check, pool, hir_pool, *value)?,
        },
        ExprKind::Lambda { params, body, .. } => {
            let mut hir_params = Vec::new();
            let param_ids = pool.lambda_param_list(*params);
            for &pid in param_ids {
                let p = pool.lambda_param(pid);
                let symbol = require_def_symbol(&type_check.resolved, p.span)?;
                let p_ty = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or_else(error_ty);
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
                    let eid = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
                    HirLambdaBody::Expr(eid)
                }
                arandu_parser::LambdaBody::Block { block, .. } => HirLambdaBody::Block(
                    super::stmt::lower_block(type_check, pool, hir_pool, block)?,
                ),
            };
            HirExprKind::Lambda {
                params: params_range,
                body: hir_body,
            }
        }
        ExprKind::Alloc {
            expr: inner_expr, ..
        } => {
            let inner_id = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Alloc { expr: inner_id }
        }
        ExprKind::AsyncBlock { block, .. } => HirExprKind::AsyncBlock {
            block: super::stmt::lower_block(type_check, pool, hir_pool, pool.block(*block))?,
        },
        ExprKind::UnsafeBlock { block, .. } => HirExprKind::UnsafeBlock {
            block: super::stmt::lower_block(type_check, pool, hir_pool, pool.block(*block))?,
        },
        ExprKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => HirExprKind::If {
            condition: super::stmt::lower_condition(type_check, pool, hir_pool, condition)?,
            then_block: super::stmt::lower_block(
                type_check,
                pool,
                hir_pool,
                pool.block(*then_block),
            )?,
            else_block: super::stmt::lower_block(
                type_check,
                pool,
                hir_pool,
                pool.block(*else_block),
            )?,
        },
        ExprKind::Match { value, arms, .. } => {
            let arm_ids = pool.match_arm_list(*arms);
            let value_id = lower_expr(type_check, pool, hir_pool, *value)?;
            let arms_range = super::pattern::lower_match_arms(type_check, pool, hir_pool, arm_ids)?;
            HirExprKind::Match {
                value: value_id,
                arms: arms_range,
            }
        }
        ExprKind::Catch {
            expr: inner_expr,
            handler,
            ..
        } => {
            let hir_expr_id = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            let hir_handler = match pool.catch_handler(*handler) {
                CatchHandler::Expr { expr: h, .. } => {
                    let eid = lower_expr(type_check, pool, hir_pool, *h)?;
                    HirCatchHandler::Expr(eid)
                }
                CatchHandler::Block { span, error, block } => {
                    let error_symbol = type_check
                        .resolved
                        .definitions
                        .get(&NodeKey::from(*span))
                        .copied();
                    let b = super::stmt::lower_block(type_check, pool, hir_pool, block)?;
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
        ExprKind::NullCoalesce { left, right, .. } => {
            let left_id = lower_expr(type_check, pool, hir_pool, *left)?;
            let right_id = lower_expr(type_check, pool, hir_pool, *right)?;
            HirExprKind::NullCoalesce {
                left: left_id,
                right: right_id,
            }
        }
        ExprKind::Cast {
            expr: inner_expr,
            ty: cast_ty,
            ..
        } => {
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
            let inner_id = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Cast {
                expr: inner_id,
                target_ty,
            }
        }
        ExprKind::Group {
            expr: inner_expr, ..
        } => {
            return lower_expr_raw(type_check, pool, hir_pool, *inner_expr);
        }
        ExprKind::Unary {
            op,
            expr: inner_expr,
            ..
        } => {
            let inner_id = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
            HirExprKind::Unary {
                op: (*op).into(),
                expr: inner_id,
            }
        }
        ExprKind::Binary {
            op, left, right, ..
        } => {
            let left_id = lower_expr(type_check, pool, hir_pool, *left)?;
            let right_id = lower_expr(type_check, pool, hir_pool, *right)?;
            HirExprKind::Binary {
                op: (*op).into(),
                left: left_id,
                right: right_id,
            }
        }
        // SmolStr::clone is O(1) (inline representation or Arc storage)
        ExprKind::Int { value } => HirExprKind::Int(value.clone()),
        ExprKind::Float { value } => HirExprKind::Float(value.clone()),
        ExprKind::Bool { value } => HirExprKind::Bool(*value),
        ExprKind::Char { value } => HirExprKind::Char(value.clone()),
        ExprKind::Byte { value } => HirExprKind::Int(crate::SmolStr::new(value.to_string())),
        ExprKind::InterpolatedString { parts } => {
            let part_ids = pool.string_part_list(*parts);
            let mut hir_parts = Vec::with_capacity(part_ids.len());
            let mut has_expr = false;
            for &part_id in part_ids {
                match pool.string_part(part_id) {
                    arandu_parser::StringPart::Text { text, .. } => {
                        // AST already holds SmolStr — clone is cheap (inline/refcount).
                        hir_parts.push(HirStringPart::Text(text.clone()));
                    }
                    arandu_parser::StringPart::Expr {
                        expr: inner_expr, ..
                    } => {
                        has_expr = true;
                        let lowered = lower_expr(type_check, pool, hir_pool, *inner_expr)?;
                        hir_parts.push(HirStringPart::Expr(lowered));
                    }
                }
            }
            if has_expr {
                HirExprKind::StringInterp { parts: hir_parts }
            } else {
                // Pure text — collapse segments into a single Str literal.
                let combined = match hir_parts.as_slice() {
                    [HirStringPart::Text(t)] => t.clone(),
                    [HirStringPart::Expr(_)] => unreachable!("pure-text branch with Expr part"),
                    _ => {
                        let mut buf = String::new();
                        for p in hir_parts {
                            match p {
                                HirStringPart::Text(t) => buf.push_str(&t),
                                HirStringPart::Expr(_) => {
                                    unreachable!("pure-text branch with Expr part")
                                }
                            }
                        }
                        crate::SmolStr::new(buf)
                    }
                };
                HirExprKind::Str(combined)
            }
        }
        ExprKind::Nil => HirExprKind::Nil,
        ExprKind::Error => HirExprKind::Error,
    };

    let ty = expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}

/// Lower expression and allocate into a `HirPool`, returning a `HirExprId`.
pub(crate) fn lower_expr(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<crate::hir::HirExprId, Diagnostic> {
    let hir = lower_expr_raw(type_check, pool, hir_pool, expr)?;
    Ok(hir_pool.alloc_expr(hir))
}
