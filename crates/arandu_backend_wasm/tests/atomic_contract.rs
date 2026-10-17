//! Execute the shared atomic contract in a real wasm32 runtime.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;

fn compile(source: &str) -> Vec<u8> {
    let atomic = include_str!("../../../stdlib/core/atomic.aru");
    let mut db = arandu_query::db::DatabaseImpl::new();
    db.set_target_config(common::wasm32());
    for (path, source) in [
        (
            "stdlib/core/intrinsics.aru",
            include_str!("../../../stdlib/core/intrinsics.aru"),
        ),
        (
            "stdlib/core/option.aru",
            include_str!("../../../stdlib/core/option.aru"),
        ),
        (
            "stdlib/core/result.aru",
            include_str!("../../../stdlib/core/result.aru"),
        ),
        (
            "stdlib/core/slice.aru",
            include_str!("../../../stdlib/core/slice.aru"),
        ),
        ("stdlib/core/atomic.aru", atomic),
    ] {
        db.new_file(path.into(), source.into());
    }
    let file = db.new_file("atomic.aru".into(), source.into());
    let lowered = arandu_query::passes::lower_amir(&db, file);
    assert!(
        lowered
            .type_check
            .diagnostics
            .iter()
            .all(|d| d.severity != arandu_middle::Severity::Error),
        "{:?}",
        lowered.type_check.diagnostics
    );
    arandu_backend_wasm::emit_wasm(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        common::wasm32(),
    )
    .expect("emit atomic")
}

#[test]
fn atomic_widths_wrapping_cas_and_pointer_operations_on_wasm32() {
    let bytes = compile(include_str!("../../../tests/corpus/stdlib_atomic.aru"));
    assert_eq!(common::run_main_i32(&bytes), 0);
    let mut count = 0;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
            let mut reader = body.get_operators_reader().unwrap();
            while !reader.eof() {
                if matches!(
                    reader.read().unwrap(),
                    wasmparser::Operator::I32AtomicRmwAdd { .. }
                        | wasmparser::Operator::I64AtomicRmwAdd { .. }
                        | wasmparser::Operator::I32AtomicRmwCmpxchg { .. }
                ) {
                    count += 1;
                }
            }
        }
    }
    assert!(count >= 3, "atomic calls must lower to atomic instructions");
}

#[test]
fn invalid_atomic_order_traps_on_wasm32() {
    let bytes = compile(
        "import std.core.atomic as atomic\npublic func main(): int { let mut value = atomic.AtomicInt.new(1); value.store(2, atomic.MemoryOrder.Acquire); return 0 }",
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    assert!(
        instance
            .get_typed_func::<(), i32>(&mut store, "main")
            .unwrap()
            .call(&mut store, ())
            .is_err()
    );
}
