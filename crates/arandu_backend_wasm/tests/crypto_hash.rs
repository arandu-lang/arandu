//! Execute the shared hash contract in a real wasm32 runtime.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;

fn compile(source: &str) -> Vec<u8> {
    compile_with_hash(source, include_str!("../../../stdlib/std/crypto/hash.aru"))
}

fn compile_with_hash(source: &str, hash: &str) -> Vec<u8> {
    let mut db = arandu_query::db::DatabaseImpl::new();
    db.set_target_config(common::wasm32());
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
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
        ("stdlib/std/crypto/hash.aru", hash),
    ] {
        db.new_stdlib_file(path.into(), source.into());
    }
    let file = db.new_file("hash.aru".into(), source.into());
    let runtime = arandu_query::runtime::runtime_program(&db, file);
    let lowered = &runtime.artifacts;
    assert!(
        runtime
            .diagnostics
            .iter()
            .all(|d| d.severity != arandu_middle::Severity::Error),
        "{:?}",
        runtime.diagnostics
    );
    arandu_backend_wasm::emit_wasm(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        common::wasm32(),
    )
    .expect("emit hash")
}

#[test]
fn hash_vectors_and_streaming_on_wasm32() {
    assert_eq!(
        common::run_main_i32(&compile(include_str!(
            "../../../tests/corpus/stdlib_hash.aru"
        ))),
        0
    );
}
