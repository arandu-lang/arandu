use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind, HirStringPart};
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_group(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExpr, Diagnostic> {
    let ExprKind::Group {
        expr: inner_expr, ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    super::lower_expr_raw(type_check, pool, hir_pool, *inner_expr)
}

pub(super) fn lower_unary(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Unary {
        op,
        expr: inner_expr,
        ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    if matches!(
        op,
        arandu_parser::UnaryOp::Ref | arandu_parser::UnaryOp::RefMut
    ) {
        let mut target = *inner_expr;
        while let ExprKind::Group { expr: inner, .. } = pool.expr(target) {
            target = *inner;
        }
        if let ExprKind::Index { base, index } = pool.expr(target)
            && let Some(kind) = super::access::try_lower_trait_index_ref(
                type_check,
                pool,
                hir_pool,
                *base,
                *index,
                matches!(op, arandu_parser::UnaryOp::RefMut),
                pool.expr_span(expr),
            )?
        {
            return Ok(kind);
        }
    }
    Ok({
        let inner_id = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
        HirExprKind::Unary {
            op: super::super::ops::lower_unary_op(*op),
            expr: inner_id,
        }
    })
}

pub(super) fn lower_binary(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Binary {
        op, left, right, ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    Ok({
        {
            let left_id = super::lower_expr(type_check, pool, hir_pool, *left)?;
            let right_id = super::lower_expr(type_check, pool, hir_pool, *right)?;
            HirExprKind::Binary {
                op: super::super::ops::lower_binary_op(*op),
                left: left_id,
                right: right_id,
            }
        }
    })
}

pub(super) fn lower_int(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Int { value } = pool.expr(expr) else {
        unreachable!();
    };
    // SmolStr::clone is O(1) (inline representation or Arc storage)
    Ok(HirExprKind::Int(value.clone()))
}

pub(super) fn lower_float(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Float { value } = pool.expr(expr) else {
        unreachable!();
    };
    Ok(HirExprKind::Float(value.clone()))
}

pub(super) fn lower_bool(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Bool { value } = pool.expr(expr) else {
        unreachable!();
    };
    Ok(HirExprKind::Bool(*value))
}

pub(super) fn lower_char(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Char { value } = pool.expr(expr) else {
        unreachable!();
    };
    Ok(HirExprKind::Char(value.clone()))
}

pub(super) fn lower_byte(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Byte { value } = pool.expr(expr) else {
        unreachable!();
    };
    Ok(HirExprKind::Int(crate::SmolStr::new(value.to_string())))
}

pub(super) fn lower_byte_string(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::ByteString { bytes } = pool.expr(expr) else {
        unreachable!();
    };
    // `b"..."` is a `[]u8` view over frozen bytes. UTF-8 bodies reuse the string
    // allocation; anything else keeps its exact raw bytes. Materializing an
    // array initializer here would produce a `[u8; N]` value where typeck has
    // already decided on `[]u8`.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(HirExprKind::FrozenBytes(
            arandu_middle::ctfe::ConstString::new(text).bytes(),
        ));
    }
    Ok(HirExprKind::FrozenBytes(
        arandu_middle::ctfe::ConstBytes::from_raw(bytes.clone()),
    ))
}

pub(super) fn lower_c_string(pool: &AstPool, expr: ExprId) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::CString { bytes } = pool.expr(expr) else {
        unreachable!();
    };
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(HirExprKind::FrozenBytes(
            arandu_middle::ctfe::ConstString::new(text).bytes(),
        ));
    }
    Ok(HirExprKind::FrozenBytes(
        arandu_middle::ctfe::ConstBytes::from_raw(bytes.clone()),
    ))
}

pub(super) fn lower_interpolated_string(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::InterpolatedString { parts } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
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
                        let lowered = super::lower_expr(type_check, pool, hir_pool, *inner_expr)?;
                        let wrapped = super::text::wrap_user_to_str(type_check, hir_pool, lowered);
                        let wrapped = super::text::wrap_user_display(type_check, hir_pool, wrapped);
                        hir_parts.push(HirStringPart::Expr(wrapped));
                    }
                }
            }
            let owned = super::text::own_type(type_check, pool, expr)
                .is_some_and(|ty| super::text::is_string(type_check, ty));
            if has_expr && owned {
                // Text built at run time is an owned `String`.
                let span = pool.expr_span(expr);
                super::text::owned_interpolation(type_check, hir_pool, hir_parts, span).ok_or_else(
                    || Diagnostic::ice(crate::DiagCode::ICEL001, "`String.from` is missing", span),
                )?
            } else if has_expr {
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
                if owned {
                    let span = pool.expr_span(expr);
                    super::text::owned_literal(type_check, hir_pool, combined, span).ok_or_else(
                        || {
                            Diagnostic::ice(
                                crate::DiagCode::ICEL001,
                                "`String.from` is missing",
                                span,
                            )
                        },
                    )?
                } else {
                    HirExprKind::Str(combined)
                }
            }
        }
    })
}

pub(super) fn lower_nil() -> Result<HirExprKind, Diagnostic> {
    Ok(HirExprKind::Nil)
}

pub(super) fn lower_error() -> Result<HirExprKind, Diagnostic> {
    Ok(HirExprKind::Error)
}
