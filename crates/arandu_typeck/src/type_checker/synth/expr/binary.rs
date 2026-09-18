use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use arandu_parser::{BinaryOp, UnaryOp};

use super::{synth_expr, synth_expr_expected};
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{self, ArType, Primitive};
use arandu_middle::types::type_interner::TypeId;

pub(super) fn cast_types_compatible(
    found: &ArType,
    target: &ArType,
    interner: &arandu_middle::types::TypeInterner,
    symbols: &arandu_middle::SymbolTable,
) -> bool {
    if found.is_error() || target.is_error() {
        return true;
    }
    if types::unify(found, target, interner) {
        return true;
    }
    if found.is_numeric() && target.is_numeric() {
        return true;
    }
    // `char` is a Unicode scalar value with a canonical u32 representation.
    // This direction is lossless; the inverse requires scalar validation and
    // therefore remains outside the general cast operator.
    if matches!(found, ArType::Primitive(Primitive::Char))
        && matches!(target, ArType::Primitive(Primitive::U32))
    {
        return true;
    }
    if matches!(found, ArType::Ptr(_)) && matches!(target, ArType::Ptr(_)) {
        return true;
    }
    // WorkThunk / function-to-pointer cast: allow `func as ptr[T]`.
    if matches!(found, ArType::Func(_, _)) && matches!(target, ArType::Ptr(_)) {
        return true;
    }
    // A3/SL_R ABI: `Coroutine[T]` is a state-blob pointer at runtime (Cranelift
    // `clif_type` → pointer). Allow `job as ptr[u8]` so `std.runtime` typed
    // spawn/block_on can hand the blob to host `ar_rt_*` / `ar_co_*`.
    if let (ArType::Coroutine(_), ArType::Ptr(inner)) = (found, target) {
        let inner_ty = interner.resolve(*inner);
        return matches!(
            inner_ty,
            ArType::Primitive(Primitive::Byte) | ArType::Primitive(Primitive::U8)
        );
    }
    // Host i64 payload bits → type param `T` (generic join/block_on).
    // Only TypeParam targets — never arbitrary named types.
    if found.is_integer()
        && let ArType::Named(id, args) = target
        && args.is_empty()
        && symbols.get(*id).kind == arandu_middle::SymbolKind::TypeParam
    {
        return true;
    }
    false
}

pub(super) fn is_equality_comparable(ty: &ArType) -> bool {
    matches!(
        ty,
        ArType::Primitive(_)
            | ArType::Ptr(_)
            | ArType::Ref(_)
            | ArType::RefMut(_)
            | ArType::Nullable(_)
            | ArType::Option(_)
            | ArType::Result(_, _)
            | ArType::Err
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::GenRef
    )
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, expr))]
pub(super) fn synth_binary_unary_expr(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    kind: &ExprKind,
    span: Span,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    match kind {
        // `ExprKind::NullCoalesce` is routed to `synth_call_expr`
        // (call/mod.rs), which also accepts `Option` and reports T005 for
        // non-nullable operands; it never reaches this function, so a
        // duplicate arm here would be unreachable and divergent.
        ExprKind::Cast {
            expr: inner_expr,
            ty,
        } => {
            let inner_id = *inner_expr;
            let ty_id = *ty;
            let found_ty_id = synth_expr(checker, inner_id);
            let target_ty = checker.lower_type_expr(ty_id, checker.type_scope());
            let target_ty_id = checker.intern(target_ty);
            let found_ty = checker.resolve(found_ty_id);
            let target_ty = checker.resolve(target_ty_id);
            if !cast_types_compatible(
                &found_ty,
                &target_ty,
                &checker.type_info.type_interner,
                &checker.symbols,
            ) {
                checker.add_constraint(
                    target_ty_id,
                    found_ty_id,
                    ConstraintOrigin::CastExpr {
                        expr_span: checker.pool.expr_span(inner_id),
                        target_span: checker.pool.type_expr_span(ty_id),
                    },
                );
            }
            Some(target_ty_id)
        }
        ExprKind::Unary {
            op,
            expr: inner_expr,
        } => {
            let inner_id = *inner_expr;
            match op {
                UnaryOp::Neg => {
                    // `-(128)` keeps the same literal semantics as `-128`:
                    // peel parenthesized groups so the sign participates in
                    // the expected-type fast path and, when promoted, in the
                    // literal group below (sign-aware T038 range checks).
                    let mut lit_id = inner_id;
                    while let ExprKind::Group { expr } = checker.pool.expr(lit_id) {
                        lit_id = *expr;
                    }
                    let lit_is_int = matches!(checker.pool.expr(lit_id), ExprKind::Int { .. });
                    if let Some(exp_id) = expected
                        && let ArType::Primitive(p) = checker.resolve(exp_id)
                        && lit_is_int
                        && let ExprKind::Int { value, .. } = checker.pool.expr(lit_id)
                        && let Some(parsed) = arandu_middle::literal_pool::parse_int_literal(value)
                    {
                        if p.is_float() {
                            checker.record_expr_type(inner_id, exp_id);
                            return Some(exp_id);
                        }
                        if p.is_signed() {
                            let neg_val = -parsed;
                            let fits = match p {
                                Primitive::I8 => {
                                    (i8::MIN as i128..=i8::MAX as i128).contains(&neg_val)
                                }
                                Primitive::I16 => {
                                    (i16::MIN as i128..=i16::MAX as i128).contains(&neg_val)
                                }
                                Primitive::I32 => {
                                    (i32::MIN as i128..=i32::MAX as i128).contains(&neg_val)
                                }
                                Primitive::I64 => {
                                    (i64::MIN as i128..=i64::MAX as i128).contains(&neg_val)
                                }
                                Primitive::Int => (checker.target_info.int_min()
                                    ..=checker.target_info.int_max())
                                    .contains(&neg_val),
                                _ => false,
                            };
                            if !fits {
                                checker.diagnostics.push(
                                    crate::Diagnostic::error(
                                        crate::DiagCode::T038IntegerLiteralOutOfRange,
                                        format!(
                                            "integer literal `-{value}` does not fit in `{}`",
                                            p.as_str()
                                        ),
                                        span,
                                    )
                                    .with_label(
                                        span,
                                        format!("value is outside the range of `{}`", p.as_str()),
                                    )
                                    .with_hint(
                                        "use a wider integer type or change the literal value",
                                    ),
                                );
                            }
                            checker.record_expr_type(inner_id, exp_id);
                            return Some(exp_id);
                        }
                    }

                    let expr_ty_id = synth_expr_expected(checker, inner_id, expected);
                    // Bind the negation to the operand's literal group (peeling
                    // groups like `-(128)`) so late constraints see it, and
                    // mark the occurrence as negated so the retroactive T038
                    // range check validates the signed value.
                    if let Some(var_id) = checker.literal_table.var_for_expr(lit_id) {
                        if lit_is_int {
                            checker.literal_table.negate_occurrence(var_id, lit_id);
                        }
                        checker.literal_table.bind_expr(expr, var_id);
                    }
                    let expr_ty = checker.resolve(expr_ty_id);
                    if expr_ty.is_error() {
                        return Some(checker.intern(ArType::Error));
                    }
                    if expr_ty.is_signed() {
                        Some(expr_ty_id)
                    } else {
                        checker.add_constraint(
                            ArType::Primitive(Primitive::Int),
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op: *op,
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                UnaryOp::Not => {
                    let bool_id = checker.intern(ArType::Primitive(Primitive::Bool));
                    let expr_ty_id = synth_expr_expected(checker, inner_id, Some(bool_id));
                    let expr_ty = checker.resolve(expr_ty_id);
                    if expr_ty.is_error() {
                        return Some(checker.intern(ArType::Error));
                    }
                    if types::unify(
                        &expr_ty,
                        &ArType::Primitive(Primitive::Bool),
                        &checker.type_info.type_interner,
                    ) {
                        Some(checker.intern(ArType::Primitive(Primitive::Bool)))
                    } else {
                        checker.add_constraint(
                            ArType::Primitive(Primitive::Bool),
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op: *op,
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                UnaryOp::BitNot => {
                    let int_expected =
                        expected.filter(|&exp_id| checker.resolve(exp_id).is_integer());
                    let expr_ty_id = synth_expr_expected(checker, inner_id, int_expected);
                    let expr_ty = checker.resolve(expr_ty_id);
                    if expr_ty.is_error() {
                        return Some(checker.intern(ArType::Error));
                    }
                    if expr_ty.is_integer() {
                        Some(expr_ty_id)
                    } else {
                        checker.add_constraint(
                            ArType::Primitive(Primitive::Int),
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op: *op,
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                UnaryOp::Await => {
                    checker.current_observed_effects = checker
                        .current_observed_effects
                        .union(arandu_middle::EffectFlags::SUSPEND);
                    let expr_ty_id = synth_expr(checker, inner_id);
                    let expr_ty = checker.resolve(expr_ty_id);
                    if expr_ty.is_error() {
                        Some(checker.intern(ArType::Error))
                    } else if let ArType::Coroutine(inner) = expr_ty {
                        Some(inner)
                    } else {
                        checker.add_constraint(
                            ArType::Error,
                            expr_ty_id,
                            ConstraintOrigin::AwaitInvalid { span },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                // F2.0: safe shared/exclusive address-of (not raw ptr).
                UnaryOp::Ref => {
                    let expr_ty_id = synth_expr(checker, inner_id);
                    let expr_ty = checker.resolve(expr_ty_id);
                    let inner_id = if expr_ty.is_error() {
                        checker.intern(ArType::Error)
                    } else {
                        expr_ty_id
                    };
                    Some(checker.intern(ArType::Ref(inner_id)))
                }
                UnaryOp::RefMut => {
                    let expr_ty_id = synth_expr(checker, inner_id);
                    let expr_ty = checker.resolve(expr_ty_id);
                    let inner_id = if expr_ty.is_error() {
                        checker.intern(ArType::Error)
                    } else {
                        expr_ty_id
                    };
                    Some(checker.intern(ArType::RefMut(inner_id)))
                }
                UnaryOp::Deref => {
                    let expr_ty_id = synth_expr(checker, inner_id);
                    let expr_ty = checker.resolve(expr_ty_id);
                    match expr_ty {
                        ArType::Ref(inner) | ArType::RefMut(inner) => Some(inner),
                        ArType::Ptr(inner) => {
                            if !checker.ctx.is_in_unsafe() {
                                checker.diagnostics.push(
                                    crate::Diagnostic::error(
                                        crate::DiagCode::O012AllocRequiresUnsafe,
                                        "dereferencing a raw pointer requires an `unsafe` block",
                                        span,
                                    )
                                    .with_label(span, "raw `ptr[T]` deref is unsafe")
                                    .with_note(
                                        "use `&T` / `&mut T` for safe borrows (F2.0)".to_string(),
                                    ),
                                );
                            }
                            Some(inner)
                        }
                        _ => {
                            checker.add_constraint(
                                ArType::Error,
                                expr_ty_id,
                                ConstraintOrigin::UnaryOp {
                                    op: *op,
                                    op_span: span,
                                    operand_span: checker.pool.expr_span(inner_id),
                                },
                            );
                            Some(checker.intern(ArType::Error))
                        }
                    }
                }
            }
        }
        ExprKind::Binary { op, left, right } => {
            let left_id = *left;
            let right_id = *right;

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
                    let r_expected = if !l_ty.is_literal() && l_ty.is_numeric() {
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
                    let range_elem_expected =
                        expected.and_then(|exp_id| match checker.resolve(exp_id) {
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

            let interner = &checker.type_info.type_interner;
            let left_ty = interner.resolve(left_ty_id);
            let right_ty = interner.resolve(right_ty_id);

            if left_ty.is_error() || right_ty.is_error() {
                return Some(checker.intern(ArType::Error));
            }

            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || (!left_ty.is_numeric() && !right_ty.is_numeric())
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    if matches!(op, BinaryOp::Div | BinaryOp::Mod) {
                        let is_zero = match checker.pool.expr(right_id) {
                            ExprKind::Int { value, .. } => {
                                arandu_middle::literal_pool::parse_int_literal(value) == Some(0)
                            }
                            ExprKind::Float { value, .. } => {
                                value.parse::<f64>().map(|f| f == 0.0).unwrap_or(false)
                            }
                            _ => false,
                        };
                        if is_zero {
                            checker.diagnostics.push(
                                crate::Diagnostic::error(
                                    crate::DiagCode::T040DivisionByZero,
                                    "attempt to divide by zero",
                                    span,
                                )
                                .with_label(
                                    checker.pool.expr_span(right_id),
                                    "division by zero here",
                                )
                                .with_hint("divisor must be non-zero"),
                            );
                            return Some(checker.intern(ArType::Error));
                        }
                    }
                    if matches!(op, BinaryOp::Mod)
                        && (!left_ty.is_integer() || !right_ty.is_integer())
                    {
                        let left_str =
                            left_ty.display(&checker.symbols, &checker.type_info.type_interner);
                        let right_str =
                            right_ty.display(&checker.symbols, &checker.type_info.type_interner);
                        checker.diagnostics.push(
                            crate::Diagnostic::error(
                                crate::DiagCode::T005OperatorNotApplicable,
                                format!("operator '%' is only defined for integer types, found '{left_str}' and '{right_str}'"),
                                span,
                            )
                            .with_label(checker.pool.expr_span(left_id), format!("type '{left_str}'"))
                            .with_label(checker.pool.expr_span(right_id), format!("type '{right_str}'"))
                            .with_hint("modulo operator `%` is only defined for integer types"),
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    Some(checker.intern(types::resolve_literal_pair(&left_ty, &right_ty)))
                }
                BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::BitAnd
                | BinaryOp::ShiftLeft
                | BinaryOp::ShiftRight => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || !left_ty.is_integer()
                        || !right_ty.is_integer()
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    if matches!(op, BinaryOp::ShiftLeft | BinaryOp::ShiftRight) {
                        let parsed_shift: Option<i128> = match checker.pool.expr(right_id) {
                            ExprKind::Int { value, .. } => {
                                arandu_middle::literal_pool::parse_int_literal(value)
                            }
                            ExprKind::Unary {
                                op: UnaryOp::Neg,
                                expr,
                            } => {
                                if let ExprKind::Int { value, .. } = checker.pool.expr(*expr) {
                                    arandu_middle::literal_pool::parse_int_literal(value)
                                        .map(|v| -v)
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        if let Some(shift_val) = parsed_shift {
                            let width = match left_ty {
                                ArType::Primitive(
                                    Primitive::I8 | Primitive::U8 | Primitive::Byte,
                                ) => Some(8),
                                ArType::Primitive(Primitive::I16 | Primitive::U16) => Some(16),
                                ArType::Primitive(Primitive::I32 | Primitive::U32) => Some(32),
                                ArType::Primitive(Primitive::I64 | Primitive::U64) => Some(64),
                                ArType::Primitive(Primitive::Int | Primitive::Uint) => {
                                    Some(checker.target_info.pointer_width as i128)
                                }
                                _ => None,
                            };
                            if shift_val < 0 || width.is_some_and(|w| shift_val >= w) {
                                checker.diagnostics.push(
                                    crate::Diagnostic::error(
                                        crate::DiagCode::T038IntegerLiteralOutOfRange,
                                        format!("shift count `{shift_val}` is out of range for type"),
                                        span,
                                    )
                                    .with_label(checker.pool.expr_span(right_id), "shift count exceeds bit width")
                                    .with_hint("shift count must be non-negative and less than the bit width"),
                                );
                                return Some(checker.intern(ArType::Error));
                            }
                        }
                    }
                    Some(checker.intern(types::resolve_literal_pair(&left_ty, &right_ty)))
                }
                BinaryOp::Equal | BinaryOp::NotEqual => {
                    // Root cause fix (RC-ERR-NIL): `x != nil` / `x == nil` where `x` is
                    // `T?` / `Option<T>` / Result-destructure err channel. Bare `nil`
                    // otherwise defaults to `void?` and fails unify with `Err`.
                    let left_is_nil = matches!(checker.pool.expr(left_id), ExprKind::Nil);
                    let right_is_nil = matches!(checker.pool.expr(right_id), ExprKind::Nil);
                    if left_is_nil || right_is_nil {
                        let (value_ty_id, nil_expr) = if right_is_nil {
                            (left_ty_id, right_id)
                        } else {
                            (right_ty_id, left_id)
                        };
                        let value_ty = checker.resolve(value_ty_id);
                        let ok = matches!(
                            value_ty,
                            ArType::Nullable(_)
                                | ArType::Option(_)
                                | ArType::Ptr(_)
                                | ArType::GenRef
                        ) || value_ty.is_error();
                        if ok {
                            // Re-record nil with the value's type so HIR is consistent.
                            checker.record_expr_type(nil_expr, value_ty_id);
                            return Some(checker.intern(ArType::Primitive(Primitive::Bool)));
                        }
                    }

                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || !is_equality_comparable(&left_ty)
                        || !is_equality_comparable(&right_ty)
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    Some(checker.intern(ArType::Primitive(Primitive::Bool)))
                }
                BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEqual | BinaryOp::GtEqual => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || !left_ty.is_orderable()
                        || !right_ty.is_orderable()
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    Some(checker.intern(ArType::Primitive(Primitive::Bool)))
                }
                BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || (!left_ty.is_integer() && !right_ty.is_integer())
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    let inner_ty =
                        types::resolve_literal_pair(&left_ty, &right_ty).default_literal();
                    let inner_id = checker.intern(inner_ty);
                    Some(checker.intern(ArType::Range(inner_id)))
                }
                BinaryOp::And | BinaryOp::Or => {
                    let bool_id = checker.intern(ArType::Primitive(Primitive::Bool));
                    if !checker.unify_ids(left_ty_id, bool_id) {
                        checker.add_constraint(
                            bool_id,
                            left_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    if !checker.unify_ids(right_ty_id, bool_id) {
                        checker.add_constraint(
                            bool_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    Some(bool_id)
                }
                _ => Some(checker.intern(ArType::Error)),
            }
        }
        _ => None,
    }
}
