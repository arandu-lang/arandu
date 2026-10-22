//! Real Wasm execution of freestanding fixed-point arithmetic against i128 math.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
#[path = "../../../tests/support/fixed_oracle.rs"]
mod fixed_oracle;

#[test]
fn fixed_formats_match_wide_oracle_in_wasm() {
    // CLI coverage exercises module imports; flatten only the pure fixed module
    // here to reuse the surface-language Wasm harness without a filesystem host.
    let fixed = include_str!("../../../stdlib/core/fixed.aru").replace("module std.core.fixed", "");
    let oracle = fixed_oracle::source()
        .replace("module fixed_oracle", "")
        .replace("import std.core.fixed as fixed", "")
        .replace("fixed.", "");
    let bytes = common::compile_source(&format!("{fixed}\n{oracle}"));
    assert_eq!(common::run_main_i32(&bytes), 0);
}
