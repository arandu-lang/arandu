#![allow(clippy::expect_used)]

use super::*;
use arandu_middle::layout::DataLayout;
use arandu_middle::types::Primitive;

fn ty(primitive: Primitive, width: u64) -> IntegerType {
    IntegerType::new(primitive, DataLayout::ptr_width(width)).expect("admitted integer")
}

fn integer(ty: IntegerType, value: i128) -> ConstValue {
    ConstValue::Integer(ConstInt::new(ty, value).expect("test value in range"))
}

#[test]
fn unsigned_byte_arithmetic_matches_an_independent_checked_oracle() {
    let ty = ty(Primitive::U8, 8);
    for a in u8::MIN..=u8::MAX {
        for b in u8::MIN..=u8::MAX {
            let left = integer(ty, i128::from(a));
            let right = integer(ty, i128::from(b));
            for (op, expected) in [
                (BinaryOp::Add, a.checked_add(b)),
                (BinaryOp::Sub, a.checked_sub(b)),
                (BinaryOp::Mul, a.checked_mul(b)),
            ] {
                let expected = expected
                    .map(|value| integer(ty, i128::from(value)))
                    .ok_or(ScalarEvalError::Overflow(ty));
                assert_eq!(
                    eval_binary(op, left.clone(), right.clone()),
                    expected,
                    "{a} {op:?} {b}"
                );
            }
            for (op, expected) in [
                (BinaryOp::Div, a.checked_div(b)),
                (BinaryOp::Mod, a.checked_rem(b)),
            ] {
                let expected = expected
                    .map(|value| integer(ty, i128::from(value)))
                    .ok_or(ScalarEvalError::DivisionByZero);
                assert_eq!(
                    eval_binary(op, left.clone(), right.clone()),
                    expected,
                    "{a} {op:?} {b}"
                );
            }
        }
    }
}

#[test]
fn signed_byte_arithmetic_matches_an_independent_checked_oracle() {
    let ty = ty(Primitive::I8, 8);
    for a in i8::MIN..=i8::MAX {
        for b in i8::MIN..=i8::MAX {
            let left = integer(ty, i128::from(a));
            let right = integer(ty, i128::from(b));
            for (op, expected) in [
                (BinaryOp::Add, a.checked_add(b)),
                (BinaryOp::Sub, a.checked_sub(b)),
                (BinaryOp::Mul, a.checked_mul(b)),
            ] {
                let expected = expected
                    .map(|value| integer(ty, i128::from(value)))
                    .ok_or(ScalarEvalError::Overflow(ty));
                assert_eq!(
                    eval_binary(op, left.clone(), right.clone()),
                    expected,
                    "{a} {op:?} {b}"
                );
            }
            for (op, expected) in [
                (BinaryOp::Div, a.checked_div(b)),
                (BinaryOp::Mod, a.checked_rem(b)),
            ] {
                let error = if b == 0 {
                    ScalarEvalError::DivisionByZero
                } else {
                    ScalarEvalError::Overflow(ty)
                };
                let expected = expected
                    .map(|value| integer(ty, i128::from(value)))
                    .ok_or(error);
                assert_eq!(
                    eval_binary(op, left.clone(), right.clone()),
                    expected,
                    "{a} {op:?} {b}"
                );
            }
        }
    }
}

#[test]
fn arithmetic_preserves_every_declared_integer_type() {
    for primitive in [
        Primitive::Int,
        Primitive::Uint,
        Primitive::ISize,
        Primitive::USize,
        Primitive::I8,
        Primitive::I16,
        Primitive::I32,
        Primitive::I64,
        Primitive::U8,
        Primitive::U16,
        Primitive::U32,
        Primitive::U64,
        Primitive::Byte,
    ] {
        for width in [4, 8] {
            let ty = ty(primitive, width);
            assert_eq!(
                eval_binary(BinaryOp::Add, integer(ty, 20), integer(ty, 22)),
                Ok(integer(ty, 42))
            );
            assert_eq!(
                eval_binary(BinaryOp::Add, integer(ty, ty.max()), integer(ty, 1)),
                Err(ScalarEvalError::Overflow(ty))
            );
            assert_eq!(
                eval_binary(BinaryOp::Sub, integer(ty, ty.min()), integer(ty, 1)),
                Err(ScalarEvalError::Overflow(ty))
            );
            assert_eq!(
                eval_binary(BinaryOp::Mul, integer(ty, ty.max()), integer(ty, 2)),
                Err(ScalarEvalError::Overflow(ty))
            );
            if ty.is_signed() {
                assert_eq!(
                    eval_unary(UnaryOp::Neg, integer(ty, ty.min())),
                    Err(ScalarEvalError::Overflow(ty))
                );
                for op in [BinaryOp::Div, BinaryOp::Mod] {
                    assert_eq!(
                        eval_binary(op, integer(ty, ty.min()), integer(ty, -1)),
                        Err(ScalarEvalError::Overflow(ty))
                    );
                }
            } else {
                assert_eq!(
                    eval_unary(UnaryOp::Neg, integer(ty, 1)),
                    Err(ScalarEvalError::Overflow(ty))
                );
                assert_eq!(eval_unary(UnaryOp::Neg, integer(ty, 0)), Ok(integer(ty, 0)));
            }
        }
    }
}

#[test]
fn unsigned_64_values_keep_the_high_bit_and_handle_intermediate_overflow() {
    let ty = ty(Primitive::U64, 8);
    let high = integer(ty, 1_i128 << 63);
    assert_eq!(
        eval_binary(BinaryOp::Gt, high.clone(), integer(ty, 1)),
        Ok(ConstValue::Bool(true))
    );
    assert_eq!(
        eval_binary(BinaryOp::Div, high, integer(ty, 2)),
        Ok(integer(ty, 1_i128 << 62))
    );
    assert_eq!(
        eval_binary(BinaryOp::Mul, integer(ty, ty.max()), integer(ty, ty.max())),
        Err(ScalarEvalError::Overflow(ty))
    );
}

#[test]
fn signed_division_truncates_toward_zero_and_remainder_follows_dividend() {
    let ty = ty(Primitive::Int, 8);
    for (a, b, quotient, remainder) in [(-7, 3, -2, -1), (7, -3, -2, 1), (-7, -3, 2, -1)] {
        assert_eq!(
            eval_binary(BinaryOp::Div, integer(ty, a), integer(ty, b)),
            Ok(integer(ty, quotient))
        );
        assert_eq!(
            eval_binary(BinaryOp::Mod, integer(ty, a), integer(ty, b)),
            Ok(integer(ty, remainder))
        );
    }
}

#[test]
fn bitwise_operations_use_declared_width_and_signedness() {
    for a in u8::MIN..=u8::MAX {
        let unsigned = ty(Primitive::U8, 8);
        assert_eq!(
            eval_unary(UnaryOp::BitNot, integer(unsigned, i128::from(a))),
            Ok(integer(unsigned, i128::from(!a)))
        );
        for b in [0, 1, 127, 128, 255] {
            for (op, expected) in [
                (BinaryOp::BitAnd, a & b),
                (BinaryOp::BitOr, a | b),
                (BinaryOp::BitXor, a ^ b),
            ] {
                assert_eq!(
                    eval_binary(
                        op,
                        integer(unsigned, i128::from(a)),
                        integer(unsigned, i128::from(b))
                    ),
                    Ok(integer(unsigned, i128::from(expected)))
                );
            }
        }
    }
    for a in i8::MIN..=i8::MAX {
        let signed = ty(Primitive::I8, 8);
        assert_eq!(
            eval_unary(UnaryOp::BitNot, integer(signed, i128::from(a))),
            Ok(integer(signed, i128::from(!a)))
        );
        for b in [-128, -1, 0, 1, 127] {
            for (op, expected) in [
                (BinaryOp::BitAnd, a & b),
                (BinaryOp::BitOr, a | b),
                (BinaryOp::BitXor, a ^ b),
            ] {
                assert_eq!(
                    eval_binary(
                        op,
                        integer(signed, i128::from(a)),
                        integer(signed, i128::from(b))
                    ),
                    Ok(integer(signed, i128::from(expected)))
                );
            }
        }
    }
}

#[test]
fn byte_shifts_match_independent_wider_arithmetic_and_native_right_shift() {
    let unsigned = ty(Primitive::U8, 8);
    let signed = ty(Primitive::I8, 8);
    for amount in 0_u32..8 {
        let count = integer(unsigned, i128::from(amount));
        for a in u8::MIN..=u8::MAX {
            let shifted = u16::from(a) * (1_u16 << amount);
            let expected = u8::try_from(shifted)
                .map(|value| integer(unsigned, i128::from(value)))
                .map_err(|_| ScalarEvalError::Overflow(unsigned));
            assert_eq!(
                eval_binary(
                    BinaryOp::ShiftLeft,
                    integer(unsigned, i128::from(a)),
                    count.clone()
                ),
                expected
            );
            assert_eq!(
                eval_binary(
                    BinaryOp::ShiftRight,
                    integer(unsigned, i128::from(a)),
                    count.clone()
                ),
                Ok(integer(unsigned, i128::from(a >> amount)))
            );
        }
        for a in i8::MIN..=i8::MAX {
            let shifted = i16::from(a) * (1_i16 << amount);
            let expected = i8::try_from(shifted)
                .map(|value| integer(signed, i128::from(value)))
                .map_err(|_| ScalarEvalError::Overflow(signed));
            assert_eq!(
                eval_binary(
                    BinaryOp::ShiftLeft,
                    integer(signed, i128::from(a)),
                    count.clone()
                ),
                expected
            );
            assert_eq!(
                eval_binary(
                    BinaryOp::ShiftRight,
                    integer(signed, i128::from(a)),
                    count.clone()
                ),
                Ok(integer(signed, i128::from(a >> amount)))
            );
        }
    }
}

#[test]
fn shift_counts_are_checked_against_the_left_operand_type() {
    for primitive in [
        Primitive::I8,
        Primitive::I32,
        Primitive::I64,
        Primitive::U8,
        Primitive::U32,
        Primitive::U64,
        Primitive::ISize,
        Primitive::USize,
    ] {
        for width in [4, 8] {
            let ty = ty(primitive, width);
            let shift_ty = ty_for_shift();
            for amount in [-1, i128::from(ty.bit_width()), i128::from(u32::MAX) + 1] {
                for op in [BinaryOp::ShiftLeft, BinaryOp::ShiftRight] {
                    assert_eq!(
                        eval_binary(op, integer(ty, 1), integer(shift_ty, amount)),
                        Err(ScalarEvalError::InvalidShift {
                            amount,
                            bit_width: ty.bit_width()
                        })
                    );
                }
            }
            assert_eq!(
                eval_binary(BinaryOp::ShiftLeft, integer(ty, 1), integer(shift_ty, 0)),
                Ok(integer(ty, 1))
            );
            assert_eq!(
                eval_binary(
                    BinaryOp::ShiftLeft,
                    integer(ty, ty.max()),
                    integer(shift_ty, 1)
                ),
                Err(ScalarEvalError::Overflow(ty))
            );
        }
    }
}

fn ty_for_shift() -> IntegerType {
    ty(Primitive::I64, 8)
}

#[test]
fn right_shift_uses_arithmetic_for_signed_and_logical_for_unsigned() {
    let signed = ty(Primitive::I8, 8);
    let unsigned = ty(Primitive::U8, 8);
    assert_eq!(
        eval_binary(
            BinaryOp::ShiftRight,
            integer(signed, -128),
            integer(signed, 7)
        ),
        Ok(integer(signed, -1))
    );
    assert_eq!(
        eval_binary(
            BinaryOp::ShiftRight,
            integer(unsigned, 128),
            integer(unsigned, 7)
        ),
        Ok(integer(unsigned, 1))
    );
    assert_eq!(
        eval_binary(BinaryOp::ShiftLeft, integer(signed, -2), integer(signed, 1)),
        Ok(integer(signed, -4))
    );
}

#[test]
fn all_integer_comparisons_return_booleans() {
    for primitive in [
        Primitive::Int,
        Primitive::Uint,
        Primitive::ISize,
        Primitive::USize,
        Primitive::I8,
        Primitive::I16,
        Primitive::I32,
        Primitive::I64,
        Primitive::U8,
        Primitive::U16,
        Primitive::U32,
        Primitive::U64,
        Primitive::Byte,
    ] {
        let ty = ty(primitive, 8);
        for a in [ty.min(), 0, ty.max()] {
            for b in [ty.min(), 0, ty.max()] {
                for (op, expected) in [
                    (BinaryOp::Equal, a == b),
                    (BinaryOp::NotEqual, a != b),
                    (BinaryOp::Lt, a < b),
                    (BinaryOp::Gt, a > b),
                    (BinaryOp::LtEqual, a <= b),
                    (BinaryOp::GtEqual, a >= b),
                ] {
                    assert_eq!(
                        eval_binary(op, integer(ty, a), integer(ty, b)),
                        Ok(ConstValue::Bool(expected))
                    );
                }
            }
        }
    }
}

#[test]
fn boolean_operations_match_truth_tables() {
    for a in [false, true] {
        assert_eq!(
            eval_unary(UnaryOp::Not, ConstValue::Bool(a)),
            Ok(ConstValue::Bool(!a))
        );
        for b in [false, true] {
            for (op, expected) in [
                (BinaryOp::And, a && b),
                (BinaryOp::Or, a || b),
                (BinaryOp::Equal, a == b),
                (BinaryOp::NotEqual, a != b),
            ] {
                assert_eq!(
                    eval_binary(op, ConstValue::Bool(a), ConstValue::Bool(b)),
                    Ok(ConstValue::Bool(expected))
                );
            }
        }
    }
}

#[test]
fn no_implicit_conversion_occurs_between_integer_types_or_targets() {
    let int = integer(ty(Primitive::Int, 8), 1);
    for other in [
        integer(ty(Primitive::Uint, 8), 1),
        integer(ty(Primitive::I32, 8), 1),
        ConstValue::Bool(true),
        ConstValue::Void,
    ] {
        assert_eq!(
            eval_binary(BinaryOp::Add, int.clone(), other.clone()),
            Err(ScalarEvalError::TypeMismatch)
        );
        assert_eq!(
            eval_binary(BinaryOp::Equal, int.clone(), other),
            Err(ScalarEvalError::TypeMismatch)
        );
    }
    assert_eq!(
        eval_binary(
            BinaryOp::Add,
            integer(ty(Primitive::USize, 4), 1),
            integer(ty(Primitive::USize, 8), 1)
        ),
        Err(ScalarEvalError::TypeMismatch)
    );
    assert_eq!(
        eval_unary(UnaryOp::Not, int.clone()),
        Err(ScalarEvalError::TypeMismatch)
    );
    assert_eq!(
        eval_unary(UnaryOp::Neg, ConstValue::Bool(true)),
        Err(ScalarEvalError::TypeMismatch)
    );
    assert_eq!(
        eval_binary(BinaryOp::And, int.clone(), int),
        Err(ScalarEvalError::TypeMismatch)
    );
    assert_eq!(
        eval_binary(
            BinaryOp::Add,
            ConstValue::Bool(true),
            ConstValue::Bool(false)
        ),
        Err(ScalarEvalError::TypeMismatch)
    );
}

#[test]
fn unsupported_operators_fail_closed() {
    for op in [
        BinaryOp::NullCoalesce,
        BinaryOp::RangeExclusive,
        BinaryOp::RangeInclusive,
    ] {
        assert_eq!(
            eval_binary(op, ConstValue::Void, ConstValue::Void),
            Err(ScalarEvalError::UnsupportedBinary(op))
        );
    }
    for op in [
        UnaryOp::Await,
        UnaryOp::Ref,
        UnaryOp::RefMut,
        UnaryOp::Deref,
    ] {
        assert_eq!(
            eval_unary(op, ConstValue::Void),
            Err(ScalarEvalError::UnsupportedUnary(op))
        );
    }
}
