use arandu_lexer::Span;
use arandu_parser::UnaryOp;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::super::{synth_expr, synth_expr_expected};
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{self, ArType, Primitive};
use arandu_middle::types::type_interner::TypeId;

fn cast_types_compatible(
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
    // Every unsigned byte maps to a valid Unicode scalar U+0000..U+00FF.
    // This is a numeric codepoint conversion, not UTF-8 decoding. Wider or
    // signed integers still require checked scalar validation.
    if matches!(found, ArType::Primitive(Primitive::U8 | Primitive::Byte))
        && matches!(target, ArType::Primitive(Primitive::Char))
    {
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

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, expr))]
pub(in crate::type_checker::synth::expr) fn synth_binary_unary_expr(
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
            let target_ty = checker.lower_type_expr(ty_id, checker.type_scope());
            let target_ty_id = checker.intern(target_ty);
            let expected_inner = match checker.pool.expr(inner_id) {
                ExprKind::Int { .. } | ExprKind::Float { .. } => Some(target_ty_id),
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: sub_id,
                    ..
                } => match checker.pool.expr(*sub_id) {
                    ExprKind::Int { .. } | ExprKind::Float { .. } => Some(target_ty_id),
                    _ => None,
                },
                _ => None,
            };
            let found_ty_id = super::super::synth_expr_expected(checker, inner_id, expected_inner);
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
                                Primitive::ISize => (checker.target_info.isize_min()
                                    ..=checker.target_info.isize_max())
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
            super::expr::synth_binary_expr(checker, expr, op, (*left, *right), span, expected)
        }
        _ => None,
    }
}
