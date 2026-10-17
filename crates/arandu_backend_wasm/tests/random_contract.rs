//! Pure PRNG arithmetic executes without calling entropy/allocation hosts.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;

#[test]
fn random_reference_vectors_and_integer_ranges_on_wasm32() {
    let mut db = arandu_query::db::DatabaseImpl::new();
    db.set_target_config(common::wasm32());
    for (path, source) in [
        (
            "stdlib/core/intrinsics.aru",
            include_str!("../../../stdlib/core/intrinsics.aru"),
        ),
        (
            "stdlib/core/mem.aru",
            include_str!("../../../stdlib/core/mem.aru"),
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
        (
            "stdlib/std/random.aru",
            include_str!("../../../stdlib/std/random.aru"),
        ),
    ] {
        db.new_file(path.into(), source.into());
    }
    let file = db.new_file(
        "random.aru".into(),
        include_str!("../../../tests/corpus/stdlib_random.aru").into(),
    );
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
    let bytes = arandu_backend_wasm::emit_wasm(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        common::wasm32(),
    )
    .unwrap();
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    // The backend prunes uncalled externs: pure PRNG code requires zero host imports.
    assert_eq!(
        module.imports().count(),
        0,
        "pure PRNG must emit zero host imports"
    );
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let name = common::find_func_export(&bytes, "main").unwrap();
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .unwrap();
    assert_eq!(main.call(&mut store, ()).unwrap(), 0);
}
