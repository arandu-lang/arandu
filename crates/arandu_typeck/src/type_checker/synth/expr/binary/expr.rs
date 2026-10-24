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
            let mut l_id = synth_expr(checker, left_id);
            let l_ty = checker.resolve(l_id);
            // A generic constructor such as `Option.None` takes its type
            // arguments from the other operand.
            let r_expected = if (!l_ty.is_literal() && l_ty.is_numeric())
                || matches!(
                    l_ty,
                    ArType::Option(_) | ArType::Result(..) | ArType::Nullable(_)
                ) {
                Some(l_id)
            } else {
                None
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

            (l_id, r_id)
        }
        BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => {
            let range_elem_expected = expected.and_then(|exp_id| match checker.resolve(exp_id) {
                ArType::Range(inner) => Some(inner),
                _ => None,
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

    super::result::synth_binary_result(checker, op, operands, (left_ty_id, right_ty_id), span)
}
