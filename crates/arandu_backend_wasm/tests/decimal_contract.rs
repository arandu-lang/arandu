//! Execute the shared decimal contract in a real wasm32 runtime.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;

fn compile(source: &str) -> Vec<u8> {
    let decimal = include_str!("../../../stdlib/core/decimal.aru");
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
        ("stdlib/core/decimal.aru", decimal),
    ] {
        db.new_file(path.into(), source.into());
    }
    let file = db.new_file("decimal.aru".into(), source.into());
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
    .expect("emit decimal")
}

#[test]
fn decimal_limits_and_rounding_on_wasm32() {
    let bytes = compile(include_str!("../../../tests/corpus/stdlib_decimal.aru"));
    assert_eq!(common::run_main_i32(&bytes), 0);
}

#[test]
fn decimal_parsing_preserves_nested_loop_branches() {
    let mut results = Vec::new();
    for text in ["0", "1", "4294967296", "79228162514264337593543950335"] {
        let source = format!(
            "import std.core.decimal as dec\nimport std.core.intrinsics as intr\npublic func main(): int {{ let text = \"{text}\"; let bytes = intr.strBytes(text); if intr.sliceLen<u8>(bytes) != {} {{ return 100 }}; if bytes[0] != {} {{ return 101 }}; match dec.Decimal.fromStr(text) {{ Option.Some(value) => {{ return value.scale() as int }} Option.None => {{ return 102 }} }} }}",
            text.len(),
            text.as_bytes()[0]
        );
        results.push((text.to_string(), common::run_main_i32(&compile(&source)), 0));
    }
    for (name, source, expected) in [
        (
            "sliceLen",
            "import std.core.slice as slice\nimport std.core.intrinsics as intr\npublic func main(): int { return slice.len<u8>(intr.strBytes(\"0\")) as int }",
            1,
        ),
        (
            "strParam",
            "import std.core.intrinsics as intr\nfunc scan(text: str): int { let bytes = intr.strBytes(text); return bytes[0] as int }\npublic func main(): int { return scan(\"0\") }",
            48,
        ),
        (
            "loopBool",
            "import std.core.intrinsics as intr\nfunc scan(text: str): int { let bytes = intr.strBytes(text); let mut i: usize = 0; let mut seen = false; while i < intr.sliceLen<u8>(bytes) { let b = bytes[i]; if b >= (48 as u8) && b <= (57 as u8) { seen = true }; i = i + 1 }; if seen { return 1 }; return 0 }\npublic func main(): int { return scan(\"0\") }",
            1,
        ),
        (
            "fromParts",
            "import std.core.decimal as dec\npublic func main(): int { match dec.Decimal.fromParts(1 as u32, 0 as u32, 0 as u32, false, 0 as u32) { Option.Some(value) => { return value.toInt() } Option.None => { return 102 } } }",
            1,
        ),
    ] {
        results.push((
            name.to_string(),
            common::run_main_i32(&compile(source)),
            expected,
        ));
    }
    assert!(
        results
            .iter()
            .all(|(_, actual, expected)| actual == expected),
        "{results:?}"
    );
}
