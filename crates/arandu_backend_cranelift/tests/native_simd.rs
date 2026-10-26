#![cfg(target_pointer_width = "64")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../../../tests/support/native_simd.rs"]
mod fixture;

use arandu_backend_cranelift::CraneliftBackend;
use arandu_semantics::types::VectorType;
use arandu_semantics::{OptLevel, optimize_amir_checked_with_level};

fn run(mut fixture: fixture::Fixture, shape: VectorType, expected: f64, level: OptLevel) {
    optimize_amir_checked_with_level(
        &mut fixture.program,
        &fixture.symbols,
        &fixture.info.type_interner,
        level,
    )
    .unwrap();
    let module = CraneliftBackend::try_new()
        .unwrap()
        .compile(&fixture.program, &fixture.symbols, &fixture.info)
        .unwrap();
    // SAFETY: every fixture declares a zero-argument main returning the exact
    // fixed-width scalar lane selected below. The JIT module stays alive.
    unsafe {
        match shape {
            VectorType::F32x4 => {
                let main: extern "C" fn() -> f32 = module.get_fn("main").unwrap();
                assert_eq!(main(), expected as f32, "{shape:?} {level:?}");
            }
            VectorType::F64x2 => {
                let main: extern "C" fn() -> f64 = module.get_fn("main").unwrap();
                assert_eq!(main(), expected, "{shape:?} {level:?}");
            }
            VectorType::I64x2 => {
                let main: extern "C" fn() -> i64 = module.get_fn("main").unwrap();
                assert_eq!(main(), expected as i64, "{shape:?} {level:?}");
            }
            VectorType::I32x4 => {
                let main: extern "C" fn() -> i32 = module.get_fn("main").unwrap();
                assert_eq!(main(), expected as i32, "{shape:?} {level:?}");
            }
            VectorType::I16x8 => {
                let main: extern "C" fn() -> i16 = module.get_fn("main").unwrap();
                assert_eq!(main(), expected as i16, "{shape:?} {level:?}");
            }
            VectorType::U8x16 => {
                let main: extern "C" fn() -> u8 = module.get_fn("main").unwrap();
                assert_eq!(main(), expected as u8, "{shape:?} {level:?}");
            }
        }
    }
}

#[test]
fn native_vectors_execute_all_supported_operators_before_and_after_optimization() {
    for level in [OptLevel::O0, OptLevel::O2] {
        for shape in VectorType::ALL {
            for (op, expected) in fixture::OPERATORS {
                if op.supports(shape) {
                    run(fixture::arithmetic(shape, op), shape, expected, level);
                }
            }
        }
    }
}

#[test]
fn native_vector_memory_shuffle_and_register_abi_preserve_values() {
    for level in [OptLevel::O0, OptLevel::O2] {
        for shape in VectorType::ALL {
            run(fixture::memory_shuffle(shape), shape, 49.0, level);
            run(fixture::function_abi(shape), shape, 14.0, level);
        }
    }
}

#[test]
fn native_integer_vectors_wrap_at_their_lane_width() {
    for level in [OptLevel::O0, OptLevel::O2] {
        for (shape, op, left, right, expected) in fixture::INTEGER_BOUNDARIES {
            run(
                fixture::arithmetic_values(shape, op, left, right),
                shape,
                expected,
                level,
            );
        }
    }
}

#[test]
fn comparison_masks_and_bitselect_preserve_lanes() {
    for level in [OptLevel::O0, OptLevel::O2] {
        for shape in VectorType::ALL {
            for (op, expected) in fixture::COMPARISONS {
                run(
                    fixture::comparison_select(shape, op, "7", "3"),
                    shape,
                    expected,
                    level,
                );
            }
            if !shape.lane().is_float() && shape != VectorType::U8x16 {
                run(
                    fixture::comparison_select(
                        shape,
                        arandu_semantics::amir::VectorCompareOp::Lt,
                        "-7",
                        "3",
                    ),
                    shape,
                    30.0,
                    level,
                );
            }
        }
    }
}
