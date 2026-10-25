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

pub(super) fn lower_byte_string(
    type_check: &TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::ByteString { bytes } = pool.expr(expr) else {
        unreachable!();
    };
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(HirExprKind::FrozenBytes(
            arandu_middle::ctfe::ConstString::new(text).bytes(),
        ));
    }
    let span = pool.expr_span(expr);
    let u8_ty = type_check.type_info.type_interner.intern(
        crate::passes::type_checker::types::ArType::Primitive(
            crate::passes::type_checker::types::Primitive::U8,
        ),
    );
    let mut hir_items = Vec::with_capacity(bytes.len());
    for &b in bytes.iter() {
        hir_items.push(hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Int(crate::SmolStr::new(b.to_string())),
            ty: u8_ty,
            span,
        }));
    }
    let items_range = hir_pool.alloc_expr_list(&hir_items);
    Ok(HirExprKind::Array { items: items_range })
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
    })
}

pub(super) fn lower_nil() -> Result<HirExprKind, Diagnostic> {
    Ok(HirExprKind::Nil)
}

pub(super) fn lower_error() -> Result<HirExprKind, Diagnostic> {
    Ok(HirExprKind::Error)
}
