mod access;
mod aggregate;
mod call;
mod control;
mod literal;
mod path;
pub(crate) mod range;
pub(crate) mod temporary;
pub(crate) mod text;

pub(crate) use path::captured_value;

use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind};
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{NodeKey, TypeCheckResult};
use arandu_middle::types::{TypeId, TypeInterner};
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
    // A temporary lent to a callback is read through its adapter instead.
    if let Some(&(symbol, ty)) = hir_pool.hoisted_values.get(&span)
        && !type_check
            .type_info
            .closure_at(span)
            .is_some_and(|recipe| recipe.adapts.is_some())
    {
        return Ok(HirExpr {
            kind: HirExprKind::Path { symbol },
            ty,
            span,
        });
    }

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
    // A named function passed as a callback is its adapter's environment.
    if let Some(recipe) = type_check
        .type_info
        .closure_at(span)
        .filter(|recipe| recipe.adapts.is_some() && hir_pool.lowering_temporary != Some(span))
        .cloned()
    {
        if let Some(code) = &recipe.owning {
            let ty = code.closure_ty;
            let kind = aggregate::owning_closure(type_check, hir_pool, &recipe, span)?;
            return Ok(HirExpr { kind, ty, span });
        }
        let kind = super::closure::capture_struct_literal(hir_pool, &recipe);
        let ty = type_check
            .type_info
            .decl_type_id(recipe.struct_symbol)
            .unwrap_or(fallback_ty);
        return Ok(HirExpr { kind, ty, span });
    }
    // Recursive binary/unary/group lowering uses a small coordinator frame;
    // Keep the other forms' diagnostic locals off those ancestor frames.
    let kind = match pool.expr(expr) {
        ExprKind::Group { expr: inner, .. } => {
            return lower_expr_raw(type_check, pool, hir_pool, *inner);
        }
        ExprKind::Binary { op, left, right } => {
            if text::is_concatenation(type_check, pool, expr) {
                // `a + b` on text is `"${a}${b}"`; a chain `a + b + c` and
                // literal operands flatten into one interpolation, owned as a
                // `String` when the type checker made it one.
                let mut parts = Vec::new();
                text::concatenation_parts(type_check, pool, hir_pool, expr, &mut parts)?;
                let owned = text::own_type(type_check, pool, expr)
                    .is_some_and(|ty| text::is_string(type_check, ty));
                if owned {
                    text::owned_interpolation(type_check, hir_pool, parts, span).ok_or_else(
                        || {
                            Diagnostic::ice(
                                crate::DiagCode::ICEL001,
                                "`String.from` is missing",
                                span,
                            )
                        },
                    )?
                } else {
                    HirExprKind::StringInterp { parts }
                }
            } else {
                let left = lower_expr(type_check, pool, hir_pool, *left)?;
                let right = lower_expr(type_check, pool, hir_pool, *right)?;
                // `a..b` reaching here is a value; a loop or an index reads
                // a literal's bounds without lowering it as an expression.
                let range = text::own_type(type_check, pool, expr).and_then(|ty| {
                    range::literal_value(type_check, hir_pool, ty, left, right, span)
                });
                match range {
                    Some(value) => value,
                    None => HirExprKind::Binary {
                        op: super::ops::lower_binary_op(*op),
                        left,
                        right,
                    },
                }
            }
        }
        ExprKind::Unary { op, expr: inner } => {
            let inner = lower_expr(type_check, pool, hir_pool, *inner)?;
            HirExprKind::Unary {
                op: super::ops::lower_unary_op(*op),
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
        ExprKind::Layout { .. } => control::lower_layout(type_check, pool, expr, span)?,
        ExprKind::Comptime { .. } if hir_pool.ctfe_lowering => {
            return control::lower_comptime_in_ctfe(
                type_check,
                pool,
                hir_pool,
                expr,
                span,
                fallback_ty,
            );
        }
        ExprKind::Comptime { .. } => {
            return control::lower_comptime(type_check, hir_pool, span, fallback_ty);
        }
        ExprKind::Path { .. } => path::lower_path(type_check, pool, hir_pool, expr, span)?,
        ExprKind::VariantSugar { .. } => {
            path::lower_variant_sugar(type_check, pool, hir_pool, expr, span, fallback_ty)?
        }
        ExprKind::TypePath { .. } => {
            return path::lower_type_path(type_check, pool, hir_pool, expr, span, fallback_ty);
        }
        ExprKind::Generic { .. } => path::lower_generic(type_check, pool, hir_pool, expr)?,
        ExprKind::Field { .. } => access::lower_field(type_check, pool, hir_pool, expr)?,
        ExprKind::SafeField { .. } => access::lower_safe_field(type_check, pool, hir_pool, expr)?,
        ExprKind::Index { .. } => access::lower_index(type_check, pool, hir_pool, expr)?,
        ExprKind::SafeIndex { .. } => access::lower_safe_index(type_check, pool, hir_pool, expr)?,
        ExprKind::Try { .. } => access::lower_try(type_check, pool, hir_pool, expr)?,
        ExprKind::Call { .. } => {
            return call::lower_call(type_check, pool, hir_pool, expr, span, fallback_ty);
        }
        ExprKind::StructLiteral { .. } => {
            aggregate::lower_struct_literal(type_check, pool, hir_pool, expr, span, fallback_ty)?
        }
        ExprKind::Tuple { .. } => aggregate::lower_tuple(type_check, pool, hir_pool, expr)?,
        ExprKind::Array { .. } => aggregate::lower_array(type_check, pool, hir_pool, expr)?,
        ExprKind::ArrayRepeat { .. } => {
            aggregate::lower_array_repeat(type_check, pool, hir_pool, expr)?
        }
        ExprKind::Lambda { .. } => aggregate::lower_lambda(type_check, pool, hir_pool, expr, span)?,
        ExprKind::Alloc { .. } => aggregate::lower_alloc(type_check, pool, hir_pool, expr)?,
        ExprKind::AsyncBlock { .. } => {
            aggregate::lower_async_block(type_check, pool, hir_pool, expr)?
        }
        ExprKind::UnsafeBlock { .. } => {
            aggregate::lower_unsafe_block(type_check, pool, hir_pool, expr)?
        }
        ExprKind::If { .. } => control::lower_if(type_check, pool, hir_pool, expr)?,
        ExprKind::Match { .. } => control::lower_match(type_check, pool, hir_pool, expr)?,
        ExprKind::Catch { .. } => control::lower_catch(type_check, pool, hir_pool, expr)?,
        ExprKind::NullCoalesce { .. } => {
            control::lower_null_coalesce(type_check, pool, hir_pool, expr)?
        }
        ExprKind::Cast { .. } => control::lower_cast(type_check, pool, hir_pool, expr)?,
        ExprKind::Group { .. } => return literal::lower_group(type_check, pool, hir_pool, expr),
        ExprKind::Unary { .. } => literal::lower_unary(type_check, pool, hir_pool, expr)?,
        ExprKind::Binary { .. } => literal::lower_binary(type_check, pool, hir_pool, expr)?,
        ExprKind::Int { .. } => literal::lower_int(pool, expr)?,
        ExprKind::Float { .. } => literal::lower_float(pool, expr)?,
        ExprKind::Bool { .. } => literal::lower_bool(pool, expr)?,
        ExprKind::Char { .. } => literal::lower_char(pool, expr)?,
        ExprKind::Byte { .. } => literal::lower_byte(pool, expr)?,
        ExprKind::ByteString { .. } => literal::lower_byte_string(pool, expr)?,
        ExprKind::CString { .. } => literal::lower_c_string(pool, expr)?,
        ExprKind::InterpolatedString { .. } => {
            literal::lower_interpolated_string(type_check, pool, hir_pool, expr)?
        }
        ExprKind::Nil => literal::lower_nil()?,
        ExprKind::Error => literal::lower_error()?,
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
    // Expressions nest as deeply as a program writes them.
    arandu_base::ensure_sufficient_stack(|| lower_expr_here(type_check, pool, hir_pool, expr))
}

fn lower_expr_here(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<crate::hir::HirExprId, Diagnostic> {
    let mut hir = lower_expr_raw(type_check, pool, hir_pool, expr)?;
    // A hoisted value was adjusted when its local was initialized.
    let adjustment = (!hir_pool.hoisted_values.contains_key(&hir.span))
        .then(|| type_check.type_info.adjustments.get(&hir.span).cloned())
        .flatten();
    let Some(adjustment) = adjustment else {
        return Ok(hir_pool.alloc_expr(hir));
    };
    let span = hir.span;
    hir.ty = adjustment.from;
    let value = hir_pool.alloc_expr(hir);
    let (function, type_args) = match adjustment.kind {
        arandu_typeck::type_checker::info::AdjustmentKind::Call {
            function,
            type_args,
        } => (function, type_args),
        // `[N]T` as `[]T` is the range of all its elements, `value[0..N]`.
        arandu_typeck::type_checker::info::AdjustmentKind::Unsize => {
            return Ok(unsize_array(type_check, hir_pool, value, &adjustment, span));
        }
        // A `String` where a `str` is expected is read through its view.
        arandu_typeck::type_checker::info::AdjustmentKind::TextView => {
            let borrowed = matches!(
                type_check.type_info.type_interner.resolve(adjustment.to),
                arandu_middle::types::ArType::Ref(_)
            );
            let view = if borrowed {
                text::string_ref_view(type_check, hir_pool, value, span)
            } else {
                text::string_view(type_check, hir_pool, value, span)
            };
            return view.ok_or_else(|| {
                Diagnostic::ice(
                    crate::DiagCode::ICEL001,
                    "`String` has no `asStr` view",
                    span,
                )
            });
        }
    };
    // The value keeps its own type and becomes `function<type_args>(value)`.
    let instance = arandu_middle::types::ArType::func(
        &[adjustment.from],
        adjustment.to,
        &type_check.type_info.type_interner,
    );
    let instance = type_check.type_info_mut().type_interner.intern(instance);
    let path = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: function },
        ty: instance,
        span,
    });
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Generic {
            callee: path,
            args: type_args,
        },
        ty: instance,
        span,
    });
    let args = hir_pool.alloc_expr_list(&[value]);
    Ok(hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Call { callee, args },
        ty: adjustment.to,
        span,
    }))
}

/// `value[0..N]` for an array of `N` elements viewed as a slice.
fn unsize_array(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    value: crate::hir::HirExprId,
    adjustment: &arandu_typeck::type_checker::info::Adjustment,
    span: arandu_middle::Span,
) -> crate::hir::HirExprId {
    use arandu_middle::types::{ArType, Primitive};
    let interner = &type_check.type_info.type_interner;
    let count = match interner.resolve(adjustment.from) {
        ArType::Array(count, _) => count,
        _ => 0,
    };
    let usize_ty = interner.intern(ArType::Primitive(Primitive::USize));
    let range_ty = interner.intern(ArType::Range(usize_ty));
    let start = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Int("0".into()),
        ty: usize_ty,
        span,
    });
    let end = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Int(count.to_string().into()),
        ty: usize_ty,
        span,
    });
    let index = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Binary {
            op: arandu_middle::ops::BinaryOp::RangeExclusive,
            left: start,
            right: end,
        },
        ty: range_ty,
        span,
    });
    hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Index { base: value, index },
        ty: adjustment.to,
        span,
    })
}
