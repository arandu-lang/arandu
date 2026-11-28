use arandu_lexer::Span;
use arandu_parser::BinaryOp;
use arandu_parser::ast_pool::ExprId;

use super::super::{synth_expr, synth_expr_expected};
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{ArType, Primitive};
use arandu_middle::types::type_interner::TypeId;

/// Keep recursive operand typing separate from nonrecursive result validation.
/// The latter's diagnostic locals must not occupy every ancestor stack frame.
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, expr))]
pub(in crate::type_checker::synth::expr) fn synth_binary_expr(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    op: &BinaryOp,
    operands: (ExprId, ExprId),
    span: Span,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    let (left_id, right_id) = operands;

    let (left_ty_id, right_ty_id) = match op {
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Mod
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::BitAnd => {
            let numeric_expected = expected.filter(|&exp_id| {
                let exp_ty = checker.resolve(exp_id);
                exp_ty.is_numeric()
            });
            let mut l_id = synth_expr_expected(checker, left_id, numeric_expected);
            let l_ty = checker.resolve(l_id);
            let r_expected = if !l_ty.is_literal() && l_ty.is_numeric() {
                Some(l_id)
            } else {
                numeric_expected
            };
            let mut r_id = synth_expr_expected(checker, right_id, r_expected);
            let r_ty = checker.resolve(r_id);

            if l_ty.is_literal() && !r_ty.is_literal() && r_ty.is_numeric() {
                l_id = synth_expr_expected(checker, left_id, Some(r_id));
            } else if r_ty.is_literal() && !l_ty.is_literal() && l_ty.is_numeric() {
                r_id = synth_expr_expected(checker, right_id, Some(l_id));
            }

            let var_l = checker.literal_table.var_for_expr(left_id);
            let var_r = checker.literal_table.var_for_expr(right_id);
            match (var_l, var_r) {
                (Some(vl), Some(vr)) => {
                    checker.report_promoted_widening(vl, vr, span);
                    let merged = checker.literal_table.unify(vl, vr);
                    checker.literal_table.bind_expr(expr, merged);
                    if let Some(exp_id) = numeric_expected {
                        checker.constrain_literal_var(
                            merged,
                            exp_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                }
                (Some(vl), None) => {
                    if !r_ty.is_literal() && r_ty.is_numeric() {
                        checker.constrain_literal_var(
                            vl,
                            r_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    checker.literal_table.bind_expr(expr, vl);
                }
                (None, Some(vr)) => {
                    if !l_ty.is_literal() && l_ty.is_numeric() {
                        checker.constrain_literal_var(
                            vr,
                            l_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    checker.literal_table.bind_expr(expr, vr);
                }
                (None, None) => {}
            }

            (l_id, r_id)
        }
        BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
            let int_expected = expected.filter(|&exp_id| {
                let exp_ty = checker.resolve(exp_id);
                exp_ty.is_integer()
            });
            let l_id = synth_expr_expected(checker, left_id, int_expected);
            let l_ty = checker.resolve(l_id);
            let r_expected = if !l_ty.is_literal() && l_ty.is_integer() {
                Some(l_id)
            } else {
                None
            };
            let r_id = synth_expr_expected(checker, right_id, r_expected);
            (l_id, r_id)
        }
        BinaryOp::Equal
        | BinaryOp::NotEqual
        | BinaryOp::Lt
        | BinaryOp::Gt
        | BinaryOp::LtEqual
        | BinaryOp::GtEqual => {
            let left_is_nil = matches!(
                checker.pool.expr(left_id),
                arandu_parser::ast_pool::ExprKind::Nil
            );
            let right_is_nil = matches!(
                checker.pool.expr(right_id),
                arandu_parser::ast_pool::ExprKind::Nil
            );
            let (mut l_id, mut r_id) = if left_is_nil && !right_is_nil {
                let r_id = synth_expr(checker, right_id);
                let r_ty = checker.resolve(r_id);
                let l_expected =
                    if matches!(r_ty, ArType::Option(_) | ArType::Ptr(_) | ArType::GenRef) {
                        Some(r_id)
                    } else {
                        None
                    };
                let l_id = synth_expr_expected(checker, left_id, l_expected);
                (l_id, r_id)
            } else {
                let l_id = synth_expr(checker, left_id);
                let l_ty = checker.resolve(l_id);
                // A generic constructor such as `Option.None` takes its type
                // arguments from the other operand.
                let r_expected = if (!l_ty.is_literal() && l_ty.is_numeric())
                    || matches!(
                        l_ty,
                        ArType::Option(_) | ArType::Result(..) | ArType::Ptr(_) | ArType::GenRef
                    ) {
                    Some(l_id)
                } else {
                    None
                };
                let r_id = synth_expr_expected(checker, right_id, r_expected);
                (l_id, r_id)
            };
            let l_ty = checker.resolve(l_id);
            let r_ty = checker.resolve(r_id);

            if l_ty.is_literal() && !r_ty.is_literal() && r_ty.is_numeric() {
                l_id = synth_expr_expected(checker, left_id, Some(r_id));
            } else if r_ty.is_literal() && !l_ty.is_literal() && l_ty.is_numeric() {
                r_id = synth_expr_expected(checker, right_id, Some(l_id));
            }

            let var_l = checker.literal_table.var_for_expr(left_id);
            let var_r = checker.literal_table.var_for_expr(right_id);
            match (var_l, var_r) {
                (Some(vl), Some(vr)) => {
                    checker.report_promoted_widening(vl, vr, span);
                    checker.literal_table.unify(vl, vr);
                }
                (Some(vl), None) => {
                    if !r_ty.is_literal() && r_ty.is_numeric() {
                        checker.constrain_literal_var(
                            vl,
                            r_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                }
                (None, Some(vr)) => {
                    if !l_ty.is_literal() && l_ty.is_numeric() {
                        checker.constrain_literal_var(
                            vr,
                            l_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                }
                (None, None) => {}
            }

            view_compared_text(checker, (left_id, right_id), (l_id, r_id))
        }
        BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => {
            let range_elem_expected = expected.and_then(|exp_id| {
                super::super::range::range_parts(checker, &checker.resolve(exp_id))
                    .map(|(element, _)| element)
            });
            let mut l_id = synth_expr_expected(checker, left_id, range_elem_expected);
            let l_ty = checker.resolve(l_id);
            let r_expected = if !l_ty.is_literal() && l_ty.is_integer() {
                Some(l_id)
            } else {
                range_elem_expected
            };
            let mut r_id = synth_expr_expected(checker, right_id, r_expected);
            let r_ty = checker.resolve(r_id);

            if l_ty.is_literal() && !r_ty.is_literal() && r_ty.is_integer() {
                l_id = synth_expr_expected(checker, left_id, Some(r_id));
            } else if r_ty.is_literal() && !l_ty.is_literal() && l_ty.is_integer() {
                r_id = synth_expr_expected(checker, right_id, Some(l_id));
            }

            let var_l = checker.literal_table.var_for_expr(left_id);
            let var_r = checker.literal_table.var_for_expr(right_id);
            match (var_l, var_r) {
                (Some(vl), Some(vr)) => {
                    checker.report_promoted_widening(vl, vr, span);
                    checker.literal_table.unify(vl, vr);
                }
                (Some(vl), None) => {
                    if !r_ty.is_literal() && r_ty.is_integer() {
                        checker.constrain_literal_var(
                            vl,
                            r_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                }
                (None, Some(vr)) => {
                    if !l_ty.is_literal() && l_ty.is_integer() {
                        checker.constrain_literal_var(
                            vr,
                            l_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                }
                (None, None) => {}
            }

            (l_id, r_id)
        }
        BinaryOp::And | BinaryOp::Or => {
            let bool_id = checker.intern(ArType::Primitive(Primitive::Bool));
            let l_id = synth_expr_expected(checker, left_id, Some(bool_id));
            let r_id = synth_expr_expected(checker, right_id, Some(bool_id));
            (l_id, r_id)
        }
        _ => {
            let l_id = synth_expr(checker, left_id);
            let r_id = synth_expr(checker, right_id);
            (l_id, r_id)
        }
    };

    let left_ty_id = checker.settle_operand(left_ty_id, Some(right_ty_id));
    let right_ty_id = checker.settle_operand(right_ty_id, Some(left_ty_id));
    super::result::synth_binary_result(checker, op, operands, (left_ty_id, right_ty_id), span)
}

/// A `String` compared with text is compared as its `str` view, as Rust's
/// `String == &str`; the value outlives the comparison.
fn view_compared_text(
    checker: &mut TypeChecker<'_>,
    operands: (ExprId, ExprId),
    types: (TypeId, TypeId),
) -> (TypeId, TypeId) {
    use super::super::text::{adjust_text_view, is_text};
    let left = checker.resolve(types.0);
    let right = checker.resolve(types.1);
    if !is_text(checker, &left) || !is_text(checker, &right) {
        return types;
    }
    let str_id = checker.intern(ArType::Primitive(Primitive::Str));
    let view = |checker: &mut TypeChecker<'_>, expr: ExprId, ty: TypeId| {
        checker.text_view_sites.insert(expr);
        adjust_text_view(checker, expr, ty, str_id).unwrap_or(ty)
    };
    (
        view(checker, operands.0, types.0),
        view(checker, operands.1, types.1),
    )
}
