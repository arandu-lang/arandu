#![allow(clippy::expect_used)]

use super::*;

const INTEGER_CASES: &[(Primitive, u8, bool)] = &[
    (Primitive::Int, 32, true),
    (Primitive::Uint, 32, false),
    (Primitive::ISize, 32, true),
    (Primitive::USize, 32, false),
    (Primitive::I8, 8, true),
    (Primitive::I16, 16, true),
    (Primitive::I32, 32, true),
    (Primitive::I64, 64, true),
    (Primitive::U8, 8, false),
    (Primitive::U16, 16, false),
    (Primitive::U32, 32, false),
    (Primitive::U64, 64, false),
    (Primitive::Byte, 8, false),
];

fn ty(primitive: Primitive, width: u64) -> IntegerType {
    IntegerType::new(primitive, DataLayout::ptr_width(width)).expect("admitted integer")
}

#[test]
fn widths_signedness_and_declared_primitives_are_preserved() {
    for &(primitive, bits32, signed) in INTEGER_CASES {
        for width in [4, 8] {
            let integer = ty(primitive, width);
            let bits = if width == 8 && matches!(primitive, Primitive::ISize | Primitive::USize) {
                64
            } else {
                bits32
            };
            assert_eq!(integer.primitive(), primitive);
            assert_eq!(integer.bit_width(), bits);
            assert_eq!(integer.is_signed(), signed);
            assert_eq!(
                integer.canonical_bytes().to_vec(),
                ConstValue::Integer(ConstInt::new(integer, 0).expect("integer zero"))
                    .canonical_bytes()
            );
        }
    }
}

#[test]
fn every_integer_accepts_its_bounds_and_rejects_neighbors() {
    for &(primitive, _, _) in INTEGER_CASES {
        for width in [4, 8] {
            let integer = ty(primitive, width);
            for value in [integer.min(), 0, integer.max()] {
                assert_eq!(
                    ConstInt::new(integer, value).expect("in range").value(),
                    value
                );
            }
            for value in [integer.min() - 1, integer.max() + 1, i128::MIN, i128::MAX] {
                assert_eq!(
                    ConstInt::new(integer, value),
                    Err(ConstValueError::OutOfRange { ty: integer, value })
                );
            }
        }
    }
}

#[test]
fn bounds_match_fixed_width_rust_oracles() {
    for (primitive, min, max) in [
        (Primitive::Int, i128::from(i32::MIN), i128::from(i32::MAX)),
        (Primitive::Uint, 0, i128::from(u32::MAX)),
        (Primitive::I8, i128::from(i8::MIN), i128::from(i8::MAX)),
        (Primitive::I16, i128::from(i16::MIN), i128::from(i16::MAX)),
        (Primitive::I64, i128::from(i64::MIN), i128::from(i64::MAX)),
        (Primitive::U8, 0, i128::from(u8::MAX)),
        (Primitive::U16, 0, i128::from(u16::MAX)),
        (Primitive::U64, 0, i128::from(u64::MAX)),
    ] {
        let integer = ty(primitive, 8);
        assert_eq!((integer.min(), integer.max()), (min, max));
    }
}

#[test]
fn unsupported_primitives_fail_instead_of_becoming_integers() {
    for primitive in [
        Primitive::Float,
        Primitive::F32,
        Primitive::F64,
        Primitive::Bool,
        Primitive::Char,
        Primitive::Str,
        Primitive::Any,
    ] {
        assert_eq!(
            IntegerType::new(primitive, DataLayout::ptr_width(8)),
            Err(ConstValueError::UnsupportedIntegerType(primitive))
        );
    }
}

#[test]
fn invalid_pointer_width_never_panics_or_uses_host_width() {
    for width in [0, 1, 2, 3, 16, u64::MAX] {
        let mut layout = DataLayout::ptr_width(8);
        layout.pointer.size = width;
        for primitive in [Primitive::ISize, Primitive::USize] {
            assert_eq!(
                IntegerType::new(primitive, layout),
                Err(ConstValueError::UnsupportedPointerWidth(width))
            );
        }
    }
}

#[test]
fn casts_check_destination_range_and_keep_the_mathematical_value() {
    let wide = ConstInt::new(ty(Primitive::U64, 8), 255).expect("u64 value");
    assert_eq!(
        wide.cast(ty(Primitive::U8, 8)).expect("u8 value").value(),
        255
    );
    assert!(matches!(
        wide.cast(ty(Primitive::I8, 8)),
        Err(ConstValueError::OutOfRange { .. })
    ));
    let negative = ConstInt::new(ty(Primitive::Int, 8), -1).expect("negative int");
    assert!(matches!(
        negative.cast(ty(Primitive::Uint, 8)),
        Err(ConstValueError::OutOfRange { .. })
    ));
    let large = ConstInt::new(ty(Primitive::USize, 8), 1_i128 << 32).expect("usize64 value");
    assert!(matches!(
        large.cast(ty(Primitive::USize, 4)),
        Err(ConstValueError::OutOfRange { .. })
    ));
}

#[test]
fn existing_const_generic_storage_is_reused_without_truncation() {
    for &(primitive, _, _) in INTEGER_CASES {
        let integer = ty(primitive, 8);
        let value = ConstInt::new(integer, integer.max()).expect("largest value");
        assert_eq!(
            value.to_const_generic().expect("u64 representable"),
            u64::try_from(integer.max()).expect("u64 max")
        );
        if integer.is_signed() {
            let negative = ConstInt::new(integer, -1).expect("negative value");
            assert_eq!(
                negative.to_const_generic(),
                Err(ConstValueError::NegativeConstGeneric(-1))
            );
        }
    }
}

#[test]
fn canonical_encoding_has_a_fixed_version_and_no_pool_identity() {
    let integer = ConstInt::new(ty(Primitive::Int, 8), -2).expect("integer");
    let mut expected = [0; 20];
    expected[..4].copy_from_slice(&[1, 2, 0, 32]);
    expected[4..].copy_from_slice(&(-2_i128).to_le_bytes());
    assert_eq!(ConstValue::Integer(integer).canonical_bytes(), expected);
    assert_eq!(ConstValue::Void.canonical_bytes()[..5], [1, 0, 0, 0, 0]);
    assert_eq!(
        ConstValue::Bool(false).canonical_bytes()[..5],
        [1, 1, 0, 0, 0]
    );
    assert_eq!(
        ConstValue::Bool(true).canonical_bytes()[..5],
        [1, 1, 0, 0, 1]
    );
}

#[test]
fn encoding_distinguishes_types_and_width_but_not_irrelevant_layout() {
    let mut encodings = std::collections::HashSet::new();
    for &(primitive, _, _) in INTEGER_CASES {
        let a = ConstValue::Integer(ConstInt::new(ty(primitive, 4), 0).expect("zero"));
        let b = ConstValue::Integer(ConstInt::new(ty(primitive, 8), 0).expect("zero"));
        assert!(
            encodings.insert(a.canonical_bytes()),
            "distinct primitive {primitive:?}"
        );
        if matches!(primitive, Primitive::ISize | Primitive::USize) {
            assert_ne!(a, b);
            assert_ne!(a.canonical_bytes(), b.canonical_bytes());
        } else {
            assert_eq!(a, b);
            assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        }
        let i686 = IntegerType::new(primitive, DataLayout::i686_sysv()).expect("integer");
        assert_eq!(
            a,
            ConstValue::Integer(ConstInt::new(i686, 0).expect("zero"))
        );
    }
    assert!(encodings.insert(ConstValue::Bool(false).canonical_bytes()));
    assert!(encodings.insert(ConstValue::Void.canonical_bytes()));
}
