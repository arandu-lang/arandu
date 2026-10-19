//! Native v128 execution, register ABI, and aligned memory roundtrips.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
#[path = "../../../tests/support/native_simd.rs"]
mod fixture;

use arandu_middle::types::VectorType;

fn emit(fixture: &fixture::Fixture) -> Vec<u8> {
    let bytes = arandu_backend_wasm::emit_wasm(
        &fixture.program,
        &fixture.symbols,
        &fixture.info.type_interner,
        &fixture.info,
        common::wasm32(),
    )
    .unwrap();
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    bytes
}

fn run(bytes: &[u8], shape: VectorType, expected: f64) {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).unwrap();
    assert_eq!(module.imports().count(), 0);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    match shape {
        VectorType::F32x4 => {
            let main = instance
                .get_typed_func::<(), f32>(&mut store, "main")
                .unwrap();
            assert_eq!(main.call(&mut store, ()).unwrap(), expected as f32);
        }
        VectorType::F64x2 => {
            let main = instance
                .get_typed_func::<(), f64>(&mut store, "main")
                .unwrap();
            assert_eq!(main.call(&mut store, ()).unwrap(), expected);
        }
        VectorType::I64x2 => {
            let main = instance
                .get_typed_func::<(), i64>(&mut store, "main")
                .unwrap();
            assert_eq!(main.call(&mut store, ()).unwrap(), expected as i64);
        }
        VectorType::I32x4 | VectorType::I16x8 | VectorType::U8x16 => {
            let main = instance
                .get_typed_func::<(), i32>(&mut store, "main")
                .unwrap();
            assert_eq!(main.call(&mut store, ()).unwrap(), expected as i32);
        }
    }
}

#[test]
fn all_shapes_execute_native_arithmetic_and_bitwise_operators() {
    for shape in VectorType::ALL {
        for (op, expected) in fixture::OPERATORS {
            if op.supports(shape) {
                run(&emit(&fixture::arithmetic(shape, op)), shape, expected);
            }
        }
    }
}

#[test]
fn vectors_roundtrip_memory_black_box_insert_and_two_input_byte_shuffle() {
    for shape in VectorType::ALL {
        let bytes = emit(&fixture::memory_shuffle(shape));
        run(&bytes, shape, 49.0);
        let mut saw_load = false;
        let mut saw_store = false;
        let mut saw_shuffle = false;
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                for operator in body.get_operators_reader().unwrap() {
                    match operator.unwrap() {
                        wasmparser::Operator::V128Load { memarg } => {
                            assert_eq!(memarg.align, 4);
                            saw_load = true;
                        }
                        wasmparser::Operator::V128Store { memarg } => {
                            assert_eq!(memarg.align, 4);
                            saw_store = true;
                        }
                        wasmparser::Operator::I8x16Shuffle { .. } => saw_shuffle = true,
                        _ => {}
                    }
                }
            }
        }
        assert!(saw_load && saw_store && saw_shuffle);
    }
}

#[test]
fn vectors_cross_internal_function_parameters_and_returns() {
    for shape in VectorType::ALL {
        run(&emit(&fixture::function_abi(shape)), shape, 14.0);
    }
}

#[test]
fn component_model_rejects_vector_boundary_instead_of_lowering_it_to_u32() {
    let mut fixture = fixture::vector_boundary(VectorType::I32x4);
    let origin = arandu_base::Span::new(999, 11, 39);
    fixture
        .symbols
        .imported_symbols
        .get_mut(&fixture::symbol(0))
        .unwrap()
        .span = origin;
    let error = arandu_backend_wasm::emit_component(
        &fixture.program,
        &fixture.symbols,
        &fixture.info.type_interner,
        &fixture.info,
        common::wasm32(),
        "simd-boundary",
    )
    .unwrap_err();
    assert_eq!(
        error.code,
        arandu_middle::diagnostics::DiagCode::U001FeatureNotSupported
    );
    assert_eq!(error.span, origin);
}

#[test]
fn native_integer_vectors_wrap_at_their_lane_width() {
    for (shape, op, left, right, expected) in fixture::INTEGER_BOUNDARIES {
        run(
            &emit(&fixture::arithmetic_values(shape, op, left, right)),
            shape,
            expected,
        );
    }
}

#[test]
fn standalone_wit_rejects_direct_and_nested_vectors_without_u32_fallback() {
    use arandu_middle::{
        amir::{AmirRvalue, AmirStmt},
        types::ArType,
    };
    for shape in VectorType::ALL {
        for nested in [false, true] {
            let mut case = fixture::vector_boundary(shape);
            if nested {
                let function = &mut case.program.funcs[0];
                let option = case
                    .info
                    .type_interner
                    .intern(ArType::Option(function.return_type));
                function.return_type = option;
                function.temps[0].ty = option;
                for statement in function.stmts.payloads.iter_mut() {
                    if let AmirStmt::Assign { lhs, rhs } = statement
                        && lhs.as_usize() == 0
                    {
                        *rhs = AmirRvalue::EnumConstruct {
                            variant_tag: 0,
                            payload: None,
                        };
                    }
                }
            }
            assert!(
                arandu_middle::validate_amir_program(
                    &case.program,
                    &case.symbols,
                    &case.info.type_interner
                )
                .is_empty()
            );
            assert!(
                arandu_backend_wasm::wit_gen::generate_wit(
                    &case.program,
                    &case.symbols,
                    &case.info.type_interner,
                    &case.info,
                    "simd-boundary"
                )
                .is_none()
            );
        }
    }
}

#[test]
fn comparison_masks_and_bitselect_use_portable_simd() {
    for shape in VectorType::ALL {
        for (op, expected) in fixture::COMPARISONS {
            let bytes = emit(&fixture::comparison_select(shape, op, "7", "3"));
            run(&bytes, shape, expected);
        }
        if !shape.lane().is_float() && shape != VectorType::U8x16 {
            run(
                &emit(&fixture::comparison_select(
                    shape,
                    arandu_middle::amir::VectorCompareOp::Lt,
                    "-7",
                    "3",
                )),
                shape,
                30.0,
            );
        }
    }
}

#[test]
fn canonical_source_vectors_and_exclusive_views_execute_without_host_imports() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .unwrap();
    let mut db = arandu_query::db::DatabaseImpl::default();
    db.set_target_config(common::wasm32());
    db.set_stdlib_root(root.clone());
    for relative in arandu_query::scan_aru_entries(&root) {
        let path = root.join(relative);
        let source = std::fs::read_to_string(&path).unwrap();
        db.new_stdlib_file(path.to_string_lossy().into_owned(), source);
    }
    let file = db.new_file(
        "source-simd.aru".into(),
        include_str!("../../../tests/corpus/stdlib_native_simd.aru").into(),
    );
    let output = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error),
        "{:?}",
        output.diagnostics
    );
    let checked = &output.artifacts.type_check;
    let bytes = arandu_backend_wasm::emit_wasm(
        &output.artifacts.amir,
        &checked.symbols,
        &checked.type_info.type_interner,
        checked.type_info.as_ref(),
        common::wasm32(),
    )
    .unwrap();
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    assert_eq!(module.imports().count(), 0);
    assert_eq!(common::run_main_i32(&bytes), 0);
}
