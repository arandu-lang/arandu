//! Lowering of owned text: `String` values built by interpolation and `+`,
//! and their `str` views (see `arandu_typeck`'s `synth::expr::text`).

use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprId, HirExprKind, HirStringPart};
use arandu_middle::Span;
use arandu_middle::symbol_table::LangItem;
use arandu_middle::types::{ArType, Primitive, TypeId};
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

/// The `String` type and one of its associated functions.
fn string_member(
    type_check: &TypeCheckResult,
    member: &str,
) -> Option<(TypeId, arandu_middle::SymbolId)> {
    let string = type_check.symbols.lang_item_symbol(LangItem::String)?;
    let function = *type_check
        .symbols
        .associated_members
        .get(&(string, member.into()))?;
    let ty = type_check.type_info.type_interner.intern(ArType::named(
        string,
        &[],
        &type_check.type_info.type_interner,
    ));
    Some((ty, function))
}

/// Whether `ty` is `String`.
pub(crate) fn is_string(type_check: &TypeCheckResult, ty: TypeId) -> bool {
    let ArType::Named(symbol, _) = type_check.type_info.type_interner.resolve(ty) else {
        return false;
    };
    type_check.symbols.lang_item_symbol(LangItem::String) == Some(symbol)
        || type_check.symbols.try_get(symbol).and_then(|s| s.lang_item) == Some(LangItem::String)
}

fn call(
    hir_pool: &mut crate::hir::HirPool,
    function: arandu_middle::SymbolId,
    function_ty: TypeId,
    args: &[HirExprId],
    ty: TypeId,
    span: Span,
) -> HirExprId {
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: function },
        ty: function_ty,
        span,
    });
    let args = hir_pool.alloc_expr_list(args);
    hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Call { callee, args },
        ty,
        span,
    })
}

/// `*String.asStr(ref value)`: the `str` view of a `String`, or of a
/// reference to one.
pub(super) fn string_view(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    value: HirExprId,
    span: Span,
) -> Option<HirExprId> {
    let view = string_ref_view(type_check, hir_pool, value, span)?;
    let str_ty = type_check
        .type_info
        .type_interner
        .intern(ArType::Primitive(Primitive::Str));
    Some(hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Unary {
            op: arandu_middle::ops::UnaryOp::Deref,
            expr: view,
        },
        ty: str_ty,
        span,
    }))
}

/// `String.asStr(ref value)`: a `ref str` borrowing the buffer of `value`.
pub(super) fn string_ref_view(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    value: HirExprId,
    span: Span,
) -> Option<HirExprId> {
    let (string_ty, as_str) = string_member(type_check, "asStr")?;
    let interner = &type_check.type_info.type_interner;
    let str_ty = interner.intern(ArType::Primitive(Primitive::Str));
    let ref_str = interner.intern(ArType::Ref(str_ty));
    let ref_string = interner.intern(ArType::Ref(string_ty));
    let as_str_ty = interner.intern(ArType::func(&[ref_string], ref_str, interner));
    let value_ty = hir_pool.expr(value).ty;
    let borrowed = if matches!(
        interner.resolve(value_ty),
        ArType::Ref(_) | ArType::RefMut(_)
    ) {
        value
    } else {
        hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Unary {
                op: arandu_middle::ops::UnaryOp::Ref,
                expr: value,
            },
            ty: ref_string,
            span,
        })
    };
    Some(call(
        hir_pool,
        as_str,
        as_str_ty,
        &[borrowed],
        ref_str,
        span,
    ))
}

/// `String.from(interpolation)`: the owned result of interpolating `parts`,
/// where every `String` part is read through its view.
pub(super) fn owned_interpolation(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    parts: Vec<HirStringPart>,
    span: Span,
) -> Option<HirExprKind> {
    let mut viewed = Vec::with_capacity(parts.len());
    for part in parts {
        viewed.push(match part {
            HirStringPart::Expr(value)
                if is_string(type_check, hir_pool.expr(value).ty)
                    || matches!(
                        type_check.type_info.type_interner.resolve(hir_pool.expr(value).ty),
                        ArType::Ref(inner) if is_string(type_check, inner)
                    ) =>
            {
                HirStringPart::Expr(string_view(type_check, hir_pool, value, span)?)
            }
            other => other,
        });
    }
    string_from(
        type_check,
        hir_pool,
        HirExprKind::StringInterp { parts: viewed },
        span,
    )
}

/// A text literal where a `String` is expected: `String.from("...")`.
pub(super) fn owned_literal(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    text: crate::SmolStr,
    span: Span,
) -> Option<HirExprKind> {
    string_from(type_check, hir_pool, HirExprKind::Str(text), span)
}

/// `String.from(text)`, copying a `str` into an owned buffer.
fn string_from(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    text: HirExprKind,
    span: Span,
) -> Option<HirExprKind> {
    let (string_ty, from) = string_member(type_check, "from")?;
    let interner = &type_check.type_info.type_interner;
    let str_ty = interner.intern(ArType::Primitive(Primitive::Str));
    let from_ty = interner.intern(ArType::func(&[str_ty], string_ty, interner));
    let text = hir_pool.alloc_expr(HirExpr {
        kind: text,
        ty: str_ty,
        span,
    });
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: from },
        ty: from_ty,
        span,
    });
    let args = hir_pool.alloc_expr_list(&[text]);
    Some(HirExprKind::Call { callee, args })
}

/// `String.pushStr(mut ref target, value)` for `target += value`.
pub(crate) fn push_str(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    target: HirExprId,
    value: HirExprId,
    span: Span,
) -> Option<HirExprId> {
    let (string_ty, push) = string_member(type_check, "pushStr")?;
    let value = if is_string(type_check, hir_pool.expr(value).ty) {
        string_view(type_check, hir_pool, value, span)?
    } else {
        value
    };
    let interner = &type_check.type_info.type_interner;
    let str_ty = interner.intern(ArType::Primitive(Primitive::Str));
    let bool_ty = interner.intern(ArType::Primitive(Primitive::Bool));
    let mut_string = interner.intern(ArType::RefMut(string_ty));
    let push_ty = interner.intern(ArType::func(&[mut_string, str_ty], bool_ty, interner));
    let borrowed = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Unary {
            op: arandu_middle::ops::UnaryOp::RefMut,
            expr: target,
        },
        ty: mut_string,
        span,
    });
    Some(call(
        hir_pool,
        push,
        push_ty,
        &[borrowed, value],
        bool_ty,
        span,
    ))
}

/// The type of `expr` itself: the recorded type is the adjusted one when an
/// adjustment converts it where it is used.
pub(super) fn own_type(
    type_check: &TypeCheckResult,
    pool: &AstPool,
    expr: ExprId,
) -> Option<TypeId> {
    type_check
        .type_info
        .adjustments
        .get(&pool.expr_span(expr))
        .map(|adjustment| adjustment.from)
        .or_else(|| type_check.type_info.expr_type_id(expr))
}

/// `a + b` whose checked type is text: a concatenation, not arithmetic.
pub(super) fn is_concatenation(type_check: &TypeCheckResult, pool: &AstPool, expr: ExprId) -> bool {
    let ExprKind::Binary { op, .. } = pool.expr(expr) else {
        return false;
    };
    matches!(op, arandu_parser::BinaryOp::Add)
        && own_type(type_check, pool, expr).is_some_and(|ty| {
            is_string(type_check, ty)
                || matches!(
                    type_check.type_info.type_interner.resolve(ty),
                    ArType::Primitive(Primitive::Str)
                )
        })
}

/// The interpolation parts of a concatenation: nested concatenations,
/// interpolations and literals flatten; any other operand is one part.
pub(super) fn concatenation_parts(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    parts: &mut Vec<HirStringPart>,
) -> Result<(), Diagnostic> {
    match pool.expr(expr) {
        ExprKind::Binary { left, right, .. } if is_concatenation(type_check, pool, expr) => {
            let (left, right) = (*left, *right);
            concatenation_parts(type_check, pool, hir_pool, left, parts)?;
            concatenation_parts(type_check, pool, hir_pool, right, parts)
        }
        ExprKind::Group { expr: inner } => {
            concatenation_parts(type_check, pool, hir_pool, *inner, parts)
        }
        ExprKind::InterpolatedString { parts: pieces } => {
            for &piece in pool.string_part_list(*pieces) {
                match pool.string_part(piece) {
                    arandu_parser::StringPart::Text { text, .. } => {
                        parts.push(HirStringPart::Text(text.clone()));
                    }
                    arandu_parser::StringPart::Expr { expr: inner, .. } => {
                        let lowered = super::lower_expr(type_check, pool, hir_pool, *inner)?;
                        parts.push(HirStringPart::Expr(lowered));
                    }
                }
            }
            Ok(())
        }
        _ => {
            let lowered = super::lower_expr(type_check, pool, hir_pool, expr)?;
            parts.push(HirStringPart::Expr(lowered));
            Ok(())
        }
    }
}

/// The place `binding` or `binding.field...` read as an expression; `None`
/// for an indexed or dereferenced place.
pub(crate) fn place_expr(
    type_check: &TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    place: &crate::hir::HirPlace,
) -> Option<HirExprId> {
    let root_ty = if place.suffixes.is_empty() {
        place.ty
    } else {
        type_check.type_info.decl_type_id(place.root_symbol)?
    };
    let mut expr = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path {
            symbol: place.root_symbol,
        },
        ty: root_ty,
        span: place.span,
    });
    for suffix in &place.suffixes {
        let crate::hir::HirPlaceSuffix::Field { name, ty, span, .. } = suffix else {
            return None;
        };
        expr = hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Field {
                base: expr,
                field: name.clone(),
            },
            ty: *ty,
            span: *span,
        });
    }
    Some(expr)
}
