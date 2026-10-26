#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "../../../tests/support/native_simd.rs"]
mod fixture;
use arandu_middle::types::VectorType;
use std::{fs, process::Command};

fn run(mut case: fixture::Fixture, shape: VectorType, expected: f64, label: &str) {
    case.symbols
        .imported_symbols
        .get_mut(&fixture::symbol(0))
        .unwrap()
        .name = "evaluate".into();
    let emitted = arandu_backend_c::emit_c(
        &case.program,
        &case.symbols,
        &case.info,
        &case.info.type_interner,
        arandu_middle::layout::DataLayout::host(),
    )
    .unwrap();
    assert!(emitted.contains("vector_size(16)"));
    let scalar = match shape {
        VectorType::F32x4 => "float",
        VectorType::F64x2 => "double",
        VectorType::I64x2 => "int64_t",
        VectorType::I32x4 => "int32_t",
        VectorType::I16x8 => "int16_t",
        VectorType::U8x16 => "uint8_t",
    };
    let source = format!(
        "{emitted}\nint main(void) {{ {scalar} actual=evaluate(); {scalar} expected=({scalar})({expected:.17}); if(actual!=expected) {{ fprintf(stderr,\"actual=%g expected=%g\\n\",(double)actual,(double)expected); return 1; }} return 0; }}\n"
    );
    let directory = std::env::temp_dir().join(format!(
        "arandu-native-simd-{}-{shape:?}-{label}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let input = directory.join("case.c");
    let output = directory.join("case");
    fs::write(&input, source).unwrap();
    for (optimization, fallback) in [("-O0", false), ("-O2", false), ("-O2", true)] {
        let mut cc = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()));
        cc.args(["-std=gnu11", optimization, "-fstrict-aliasing"]);
        if fallback {
            cc.arg("-DARANDU_SCALAR_SIMD=1");
        }
        let result = cc.arg(&input).arg("-o").arg(&output).output().unwrap();
        assert!(
            result.status.success(),
            "{shape:?} {label} {optimization} fallback={fallback}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let result = Command::new(&output).output().unwrap();
        assert!(
            result.status.success(),
            "{shape:?} {label} {optimization} fallback={fallback}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn native_and_scalar_c_vectors_execute_the_same_operation_matrix() {
    for shape in VectorType::ALL {
        for (op, expected) in fixture::OPERATORS {
            if op.supports(shape) {
                run(
                    fixture::arithmetic(shape, op),
                    shape,
                    expected,
                    &format!("{op:?}"),
                );
            }
        }
    }
}

#[test]
fn c_vectors_preserve_memory_shuffle_and_function_abi() {
    for shape in VectorType::ALL {
        run(fixture::memory_shuffle(shape), shape, 49.0, "memory");
        run(fixture::function_abi(shape), shape, 14.0, "abi");
    }
}

#[test]
fn malformed_vector_ir_is_rejected_before_any_backend_verifier() {
    use arandu_middle::amir::{AmirRvalue, AmirStmt, VectorOp};
    for mode in 0..4 {
        let mut case = if mode == 0 {
            fixture::memory_shuffle(VectorType::F32x4)
        } else {
            fixture::arithmetic(VectorType::I32x4, VectorOp::Add)
        };
        for statement in case.program.funcs[0].stmts.payloads.iter_mut() {
            if let AmirStmt::Assign { rhs, .. } = statement {
                match (mode, rhs) {
                    (0, AmirRvalue::VectorShuffle { mask, .. }) => {
                        mask[0] = 32;
                        break;
                    }
                    (1, AmirRvalue::VectorExtractLane { lane, .. }) => {
                        *lane = 4;
                        break;
                    }
                    (2, AmirRvalue::VectorOp { op, .. }) => {
                        *op = VectorOp::Div;
                        break;
                    }
                    (3, AmirRvalue::VectorOp { left, .. }) => {
                        *left = arandu_middle::amir::AmirOperand::Copy(
                            arandu_middle::amir::TempId::from_usize(4),
                        );
                        break;
                    }
                    _ => {}
                }
            }
        }
        let errors = arandu_middle::validate_amir_program(
            &case.program,
            &case.symbols,
            &case.info.type_interner,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("SIMD-TYPE")),
            "{mode}: {errors:?}"
        );
        assert!(
            arandu_backend_c::emit_c(
                &case.program,
                &case.symbols,
                &case.info,
                &case.info.type_interner,
                arandu_middle::layout::DataLayout::host()
            )
            .is_err()
        );
    }
}

#[test]
fn native_and_scalar_c_integer_vectors_wrap_without_signed_overflow() {
    for (index, (shape, op, left, right, expected)) in
        fixture::INTEGER_BOUNDARIES.into_iter().enumerate()
    {
        run(
            fixture::arithmetic_values(shape, op, left, right),
            shape,
            expected,
            &format!("boundary{index}"),
        );
    }
}

#[test]
fn comparison_masks_and_bitselect_agree_in_native_and_scalar_c() {
    for shape in VectorType::ALL {
        for (op, expected) in fixture::COMPARISONS {
            run(
                fixture::comparison_select(shape, op, "7", "3"),
                shape,
                expected,
                &format!("cmp{op:?}"),
            );
        }
        if !shape.lane().is_float() && shape != VectorType::U8x16 {
            run(
                fixture::comparison_select(
                    shape,
                    arandu_middle::amir::VectorCompareOp::Lt,
                    "-7",
                    "3",
                ),
                shape,
                30.0,
                "signed-cmp",
            );
        }
    }
}
