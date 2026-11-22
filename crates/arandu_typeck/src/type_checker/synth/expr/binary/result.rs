use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use arandu_parser::{BinaryOp, UnaryOp};

use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{self, ArType, Primitive};
use arandu_middle::types::type_interner::TypeId;

fn is_equality_comparable(checker: &TypeChecker<'_>, ty: &ArType) -> bool {
    if matches!(
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
    ) {
        return true;
    }
    if let ArType::Named(sym, _) = ty
        && !checker.type_info.struct_fields.contains_key(sym)
        && checker
            .symbols
            .try_get(*sym)
            .is_some_and(|s| matches!(s.kind, arandu_middle::SymbolKind::Enum))
    {
        let mut has_variant = false;
        for (owner, shape) in checker.type_info.enum_variants.values() {
            if owner == sym {
                has_variant = true;
                if !matches!(shape, crate::type_checker::EnumPayloadShape::Unit) {
                    return false;
                }
            }
        }
        return has_variant;
    }
    false
}

pub(super) fn synth_binary_result(
    checker: &mut TypeChecker<'_>,
    op: &BinaryOp,
    operands: (ExprId, ExprId),
    operand_types: (TypeId, TypeId),
    span: Span,
) -> Option<TypeId> {
    let (left_id, right_id) = operands;
    let (left_ty_id, right_ty_id) = operand_types;
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
            // `a + b` on text builds a new owned `String`, as interpolation
            // `"${a}${b}"` does.
            if matches!(op, BinaryOp::Add)
                && super::super::text::is_text(checker, &left_ty)
                && super::super::text::is_text(checker, &right_ty)
            {
                if let Some(string) = super::super::text::string_type(checker) {
                    return Some(string);
                }
                return Some(checker.intern(ArType::Primitive(Primitive::Str)));
            }
            if matches!(op, BinaryOp::Add | BinaryOp::Sub) {
                if matches!(left_ty, ArType::Ptr(_)) && right_ty.is_integer() {
                    if !checker.ctx.is_in_unsafe() {
                        checker.diagnostics.push(
                            crate::Diagnostic::error(
                                crate::DiagCode::O012AllocRequiresUnsafe,
                                "pointer arithmetic requires an `unsafe` block",
                                span,
                            )
                            .with_label(span, "raw pointer arithmetic is unsafe"),
                        );
                    }
                    if let Some(var_id) = checker.literal_table.var_for_expr(right_id) {
                        let usize_id = checker.intern(ArType::Primitive(Primitive::USize));
                        checker.constrain_literal_var(
                            var_id,
                            usize_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    return Some(left_ty_id);
                }
                if matches!(op, BinaryOp::Sub)
                    && matches!(left_ty, ArType::Ptr(_))
                    && matches!(right_ty, ArType::Ptr(_))
                    && checker.unify_ids(left_ty_id, right_ty_id)
                {
                    if !checker.ctx.is_in_unsafe() {
                        checker.diagnostics.push(
                            crate::Diagnostic::error(
                                crate::DiagCode::O012AllocRequiresUnsafe,
                                "pointer arithmetic requires an `unsafe` block",
                                span,
                            )
                            .with_label(span, "raw pointer arithmetic is unsafe"),
                        );
                    }
                    return Some(checker.intern(ArType::Primitive(Primitive::ISize)));
                }
            }
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
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && right_ty.is_integer() {
                let mut divisor = right_id;
                while let ExprKind::Group { expr } = checker.pool.expr(divisor) {
                    divisor = *expr;
                }
                let is_zero = match checker.pool.expr(divisor) {
                    ExprKind::Int { value, .. } => {
                        arandu_middle::literal_pool::parse_int_literal(value) == Some(0)
                    }
                    ExprKind::Byte { value } => *value == 0,
                    _ => false,
                };
                if is_zero {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T040DivisionByZero,
                            "attempt to divide by zero",
                            span,
                        )
                        .with_label(checker.pool.expr_span(right_id), "division by zero here")
                        .with_hint("divisor must be non-zero"),
                    );
                    return Some(checker.intern(ArType::Error));
                }
            }
            if matches!(op, BinaryOp::Mod) && (!left_ty.is_integer() || !right_ty.is_integer()) {
                let left_str = left_ty.display(&checker.symbols, &checker.type_info.type_interner);
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
                    ExprKind::Byte { value } => Some(*value as i128),
                    ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr,
                    } => {
                        if let ExprKind::Int { value, .. } = checker.pool.expr(*expr) {
                            arandu_middle::literal_pool::parse_int_literal(value).map(|v| -v)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(shift_val) = parsed_shift {
                    let width = match left_ty {
                        ArType::Primitive(Primitive::I8 | Primitive::U8 | Primitive::Byte) => {
                            Some(8)
                        }
                        ArType::Primitive(Primitive::I16 | Primitive::U16) => Some(16),
                        ArType::Primitive(Primitive::I32 | Primitive::U32) => Some(32),
                        ArType::Primitive(Primitive::I64 | Primitive::U64) => Some(64),
                        ArType::Primitive(Primitive::Int | Primitive::Uint) => Some(32),
                        ArType::Primitive(Primitive::ISize | Primitive::USize) => {
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
                            .with_label(
                                checker.pool.expr_span(right_id),
                                "shift count exceeds bit width",
                            )
                            .with_hint(
                                "shift count must be non-negative and less than the bit width",
                            ),
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
                    ArType::Nullable(_) | ArType::Option(_) | ArType::Ptr(_) | ArType::GenRef
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
                || !is_equality_comparable(checker, &left_ty)
                || !is_equality_comparable(checker, &right_ty)
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
            let is_orderable = |checker: &mut TypeChecker<'_>, ty: &ArType| {
                ty.is_orderable()
                    || checker
                        .symbols
                        .lang_item_symbol(arandu_middle::symbol_table::LangItem::Ord)
                        .is_some_and(|ord_sym| {
                            crate::type_checker::types::interfaces::type_satisfies_interface(
                                checker,
                                ty,
                                ord_sym,
                                &[],
                                span,
                            )
                        })
            };
            if !checker.unify_ids(left_ty_id, right_ty_id)
                || !is_orderable(checker, &left_ty)
                || !is_orderable(checker, &right_ty)
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
            let inner_ty = types::resolve_literal_pair(&left_ty, &right_ty).default_literal();
            let inner_id = checker.intern(inner_ty);
            Some(super::super::range::range_type(
                checker,
                inner_id,
                matches!(op, BinaryOp::RangeInclusive),
            ))
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
