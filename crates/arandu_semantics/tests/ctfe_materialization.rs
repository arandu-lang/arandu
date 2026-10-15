//! Residual scalar values use the ordinary HIR -> AMIR pipeline, not a second
//! backend path. These are internal staging proofs, not public syntax examples.
#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use arandu_middle::{
    DataLayout, Span, SymbolId,
    ctfe::{ConstAggregate, ConstInt, ConstString, ConstValue, IntegerType},
    hir::{HirDecl, HirExprKind},
    layout::{DataLayoutError, SizeAlign},
    types::{ArType, Primitive, TypeId, TypeInterner, TypeShape},
};
use arandu_mir::ctfe::{Budget, CtfeFunction, EvalErrorKind, FunctionProvider};
use arandu_semantics::{MaterializationError, materialize_ctfe_scalar, materialize_ctfe_value};

struct NoCalls;
impl FunctionProvider for NoCalls {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        Err(EvalErrorKind::MissingFunction(symbol))
    }
}

fn integer(primitive: Primitive, number: i128, layout: DataLayout) -> ConstValue {
    ConstValue::Integer(
        ConstInt::new(
            IntegerType::new(primitive, layout).expect("integer type"),
            number,
        )
        .expect("representable integer"),
    )
}

#[test]
fn all_integer_types_preserve_extremes_type_and_origin_without_pool_ids() {
    let types = TypeInterner::new();
    let span = Span::new(37, 12, 29);
    for layout in [
        DataLayout::ptr_width(4),
        DataLayout::i686_sysv(),
        DataLayout::ptr_width(8),
    ] {
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
            let ty = IntegerType::new(primitive, layout).expect("integer type");
            let expected = types.intern(ArType::Primitive(primitive));
            let before = types.len();
            for number in [ty.min(), 0, ty.max()] {
                let value = integer(primitive, number, layout);
                let expression = materialize_ctfe_scalar(value, expected, &types, layout, span)
                    .expect("materializable scalar");
                assert_eq!(expression.ty, expected);
                assert_eq!(expression.span, span);
                let HirExprKind::Int(text) = expression.kind else {
                    panic!("integer literal")
                };
                assert_eq!(text.parse::<i128>().expect("lossless decimal"), number);
                assert_eq!(
                    types.len(),
                    before,
                    "materialization does not intern a new type"
                );
            }
        }
    }
}

#[test]
fn mismatches_are_not_implicit_casts_or_integer_literal_defaulting() {
    let types = TypeInterner::new();
    let layout = DataLayout::ptr_width(8);
    let span = Span::new(0, 5, 10);
    let values = [
        ConstValue::Void,
        ConstValue::Bool(true),
        integer(Primitive::Int, 42, layout),
    ];
    for value in values {
        for expected_type in [
            ArType::Primitive(Primitive::Str),
            ArType::Primitive(Primitive::Char),
            ArType::Primitive(Primitive::Float),
            ArType::Primitive(Primitive::Any),
            ArType::IntLiteral,
            ArType::Error,
        ] {
            let expected = types.intern(expected_type);
            assert_eq!(
                materialize_ctfe_scalar(value.clone(), expected, &types, layout, span)
                    .expect_err("mismatch"),
                MaterializationError::TypeMismatch,
            );
        }
    }
    let int = types.intern(ArType::Primitive(Primitive::Int));
    let i32_ty = types.intern(ArType::Primitive(Primitive::I32));
    let bool_ty = types.intern(ArType::Primitive(Primitive::Bool));
    let unit = types.intern(ArType::Void);
    for (value, expected) in [
        (integer(Primitive::Int, 42, layout), i32_ty),
        (integer(Primitive::Int, 1, layout), bool_ty),
        (ConstValue::Bool(true), int),
        (ConstValue::Void, int),
        (ConstValue::Bool(false), unit),
    ] {
        assert_eq!(
            materialize_ctfe_scalar(value, expected, &types, layout, span).expect_err("mismatch"),
            MaterializationError::TypeMismatch
        );
    }
    assert_eq!(
        materialize_ctfe_scalar(ConstValue::Void, TypeId(u32::MAX), &types, layout, span)
            .expect_err("invalid id"),
        MaterializationError::InvalidType(TypeId(u32::MAX))
    );
}

#[test]
fn pointer_sized_values_cannot_cross_target_width_even_when_the_number_fits() {
    let types = TypeInterner::new();
    for primitive in [Primitive::ISize, Primitive::USize] {
        let expected = types.intern(ArType::Primitive(primitive));
        for (from, to) in [(4, 8), (8, 4)] {
            let value = integer(primitive, 42, DataLayout::ptr_width(from));
            assert!(matches!(
                materialize_ctfe_scalar(
                    value,
                    expected,
                    &types,
                    DataLayout::ptr_width(to),
                    Span::new(0, 0, 1)
                ),
                Err(MaterializationError::TargetWidthMismatch { .. })
            ));
        }
    }
    // Fixed-width integers are not tied to a pointer width or natural i64 alignment.
    let expected = types.intern(ArType::Primitive(Primitive::U64));
    let value = integer(
        Primitive::U64,
        i128::from(u64::MAX),
        DataLayout::ptr_width(8),
    );
    assert!(
        materialize_ctfe_scalar(
            value,
            expected,
            &types,
            DataLayout::i686_sysv(),
            Span::new(0, 0, 1)
        )
        .is_ok()
    );
}

#[test]
fn invalid_layout_is_rejected_for_every_value_including_unit() {
    let types = TypeInterner::new();
    let mut layout = DataLayout::ptr_width(8);
    layout.pointer = SizeAlign::new(8, 3);
    for (value, expected) in [
        (ConstValue::Void, types.intern(ArType::Void)),
        (
            ConstValue::Bool(false),
            types.intern(ArType::Primitive(Primitive::Bool)),
        ),
        (
            integer(Primitive::Int, 42, DataLayout::ptr_width(8)),
            types.intern(ArType::Primitive(Primitive::Int)),
        ),
    ] {
        assert_eq!(
            materialize_ctfe_scalar(value, expected, &types, layout, Span::new(0, 0, 1))
                .expect_err("invalid layout"),
            MaterializationError::InvalidLayout(DataLayoutError::Alignment)
        );
    }
}

#[test]
fn residual_literals_round_trip_through_canonical_lowering_and_vm() {
    for layout in [DataLayout::ptr_width(4), DataLayout::ptr_width(8)] {
        let program = arandu_parser::parse("func owner(): void {}").expect("parse");
        let resolution = arandu_semantics::resolve_for_test(0, &program);
        let mut tc = arandu_semantics::type_check(
            resolution,
            &program,
            arandu_semantics::TargetInfo {
                pointer_width: u8::try_from(layout.pointer_width() * 8).expect("pointer bits"),
            },
        );
        let mut hir =
            arandu_semantics::lower_declarations_to_hir(&mut tc, &program).expect("headers");
        let owner = hir
            .decls
            .iter()
            .find_map(|&id| match hir.pool.decl(id) {
                HirDecl::Func(function) => Some(function.clone()),
                _ => None,
            })
            .expect("owner");
        for (value, ty) in [
            (ConstValue::Void, ArType::Void),
            (ConstValue::Bool(false), ArType::Primitive(Primitive::Bool)),
            (ConstValue::Bool(true), ArType::Primitive(Primitive::Bool)),
            (
                integer(Primitive::I64, i128::from(i64::MIN), layout),
                ArType::Primitive(Primitive::I64),
            ),
            (
                integer(Primitive::U64, i128::from(u64::MAX), layout),
                ArType::Primitive(Primitive::U64),
            ),
            (
                integer(Primitive::USize, 42, layout),
                ArType::Primitive(Primitive::USize),
            ),
            (
                ConstValue::String(ConstString::new("Olá\0🦀")),
                ArType::Primitive(Primitive::Str),
            ),
            (
                ConstValue::Bytes(
                    ConstString::new("Olá\0🦀")
                        .bytes()
                        .view(2, 3)
                        .expect("UTF8 bytes"),
                ),
                ArType::Slice(
                    tc.type_info
                        .type_interner
                        .intern(ArType::Primitive(Primitive::Byte)),
                ),
            ),
            (
                ConstValue::Aggregate(
                    ConstAggregate::new(
                        TypeShape::Array(2, Box::new(TypeShape::Primitive(Primitive::Int))),
                        vec![
                            integer(Primitive::Int, 20, layout),
                            integer(Primitive::Int, 42, layout),
                        ],
                    )
                    .expect("array"),
                ),
                ArType::Array(
                    2,
                    tc.type_info
                        .type_interner
                        .intern(ArType::Primitive(Primitive::Int)),
                ),
            ),
            (
                ConstValue::Aggregate(
                    ConstAggregate::new(
                        TypeShape::Array(0, Box::new(TypeShape::Primitive(Primitive::Int))),
                        vec![],
                    )
                    .expect("empty array"),
                ),
                ArType::Array(
                    0,
                    tc.type_info
                        .type_interner
                        .intern(ArType::Primitive(Primitive::Int)),
                ),
            ),
            (
                ConstValue::Aggregate(
                    ConstAggregate::new(
                        TypeShape::Tuple(vec![
                            TypeShape::Primitive(Primitive::Int),
                            TypeShape::Primitive(Primitive::Bool),
                        ]),
                        vec![integer(Primitive::Int, 42, layout), ConstValue::Bool(true)],
                    )
                    .expect("tuple"),
                ),
                ArType::tuple(
                    &[
                        tc.type_info
                            .type_interner
                            .intern(ArType::Primitive(Primitive::Int)),
                        tc.type_info
                            .type_interner
                            .intern(ArType::Primitive(Primitive::Bool)),
                    ],
                    &tc.type_info.type_interner,
                ),
            ),
        ] {
            let ty = tc.type_info.type_interner.intern(ty);
            let expression = materialize_ctfe_value(
                &value,
                ty,
                &tc.type_info,
                &mut hir.pool,
                layout,
                program.span,
            )
            .expect("materialize");
            let expression = hir.pool.alloc_expr(expression);
            let unit = arandu_mir::lower_expression_unit(&tc, &hir, &owner, expression, layout)
                .expect("canonical AMIR");
            let scalar = CtfeFunction::new(
                Arc::unwrap_or_clone(unit.function),
                unit.literals,
                &tc.type_info.type_interner,
                layout,
            )
            .expect("scalar AMIR");
            let actual = arandu_mir::ctfe::evaluate_unit(
                &NoCalls,
                Arc::new(scalar),
                &[],
                Budget {
                    fuel: 10_000,
                    frames: 16,
                    values: 1_000,
                },
                || false,
            )
            .expect("residual evaluation");
            assert_eq!(actual, value);
        }
    }
}
