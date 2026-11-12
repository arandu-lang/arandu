//! Range values (`std.core.range`).
//!
//! `a..b` used directly by a `for` loop or a slice index stays the binary
//! range the AMIR lowering consumes without materializing it. Anywhere else
//! it is a value: a `Range<T>` / `RangeInclusive<T>` struct literal. A loop
//! or an index over such a value reads its `start` and `end` fields.

use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprId, HirExprKind, HirFieldInit};
use arandu_middle::Span;
use arandu_middle::ops::BinaryOp;
use arandu_middle::symbol_table::LangItem;
use arandu_middle::types::{ArType, TypeId};
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

/// The element type of a range struct and whether it includes its end.
pub(crate) fn range_struct(type_check: &TypeCheckResult, ty: TypeId) -> Option<(TypeId, bool)> {
    let interner = &type_check.type_info.type_interner;
    let ArType::Named(symbol, args) = interner.resolve(ty) else {
        return None;
    };
    let inclusive = match type_check.symbols.try_get(symbol)?.lang_item? {
        LangItem::Range => false,
        LangItem::RangeInclusive => true,
        _ => return None,
    };
    Some((*interner.type_args(args).first()?, inclusive))
}

/// `expr` without its parentheses.
fn ungrouped(pool: &AstPool, mut expr: ExprId) -> ExprId {
    while let ExprKind::Group { expr: inner } = pool.expr(expr) {
        expr = *inner;
    }
    expr
}

/// Whether `expr` is written as `a..b` or `a..=b`.
pub(crate) fn is_literal(pool: &AstPool, expr: ExprId) -> bool {
    matches!(
        pool.expr(ungrouped(pool, expr)),
        ExprKind::Binary {
            op: arandu_parser::BinaryOp::RangeExclusive | arandu_parser::BinaryOp::RangeInclusive,
            ..
        }
    )
}

/// `a..b` in a value position: the struct literal `Range { start, end }`.
pub(super) fn literal_value(
    type_check: &TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    ty: TypeId,
    left: HirExprId,
    right: HirExprId,
    span: Span,
) -> Option<HirExprKind> {
    range_struct(type_check, ty)?;
    let ArType::Named(struct_symbol, _) = type_check.type_info.type_interner.resolve(ty) else {
        return None;
    };
    let fields = hir_pool.alloc_field_init_list(&[
        HirFieldInit {
            span,
            name: "start".into(),
            value: left,
        },
        HirFieldInit {
            span,
            name: "end".into(),
            value: right,
        },
    ]);
    Some(HirExprKind::StructLiteral {
        struct_symbol,
        fields,
    })
}

/// The bounds a loop or an index consumes, as the binary range over
/// `expr`: its own operands when it is a literal, otherwise the `start` and
/// `end` of the range value. `None` when `expr` is not a range.
///
/// A value that is not a plain path must have been bound to a local first
/// (`hir_pool.hoisted_values`), since it is read twice.
pub(crate) fn bounds(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<Option<HirExprId>, Diagnostic> {
    let span = pool.expr_span(expr);
    let literal = ungrouped(pool, expr);
    if let ExprKind::Binary { op, left, right } = pool.expr(literal)
        && matches!(
            op,
            arandu_parser::BinaryOp::RangeExclusive | arandu_parser::BinaryOp::RangeInclusive
        )
    {
        let ty = super::text::own_type(type_check, pool, literal)
            .unwrap_or_else(arandu_middle::types::TypeInterner::preinterned_error_id);
        let left = super::lower_expr(type_check, pool, hir_pool, *left)?;
        let right = super::lower_expr(type_check, pool, hir_pool, *right)?;
        return Ok(Some(hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Binary {
                op: super::super::ops::lower_binary_op(*op),
                left,
                right,
            },
            ty,
            span,
        })));
    }
    let Some(ty) = super::text::own_type(type_check, pool, expr) else {
        return Ok(None);
    };
    let Some((element, inclusive)) = range_struct(type_check, ty) else {
        return Ok(None);
    };
    let bound = |type_check: &mut TypeCheckResult,
                 hir_pool: &mut crate::hir::HirPool,
                 field: &str|
     -> Result<HirExprId, Diagnostic> {
        let base = super::lower_expr(type_check, pool, hir_pool, expr)?;
        Ok(hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Field {
                base,
                field: field.into(),
            },
            ty: element,
            span,
        }))
    };
    let left = bound(type_check, hir_pool, "start")?;
    let right = bound(type_check, hir_pool, "end")?;
    Ok(Some(hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Binary {
            op: if inclusive {
                BinaryOp::RangeInclusive
            } else {
                BinaryOp::RangeExclusive
            },
            left,
            right,
        },
        ty,
        span,
    })))
}

/// Whether a loop or an index over `expr` reads a range value that must be
/// bound to a local first: a range struct that is neither a literal nor a
/// plain path.
pub(crate) fn needs_local(type_check: &TypeCheckResult, pool: &AstPool, expr: ExprId) -> bool {
    !is_literal(pool, expr)
        && !matches!(pool.expr(ungrouped(pool, expr)), ExprKind::Path { .. })
        && super::text::own_type(type_check, pool, expr)
            .is_some_and(|ty| range_struct(type_check, ty).is_some())
}
