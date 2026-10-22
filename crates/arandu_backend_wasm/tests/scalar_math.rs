//! Execute scalar edge cases against independent host-f64 results.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
#[path = "../../../tests/support/math_oracle.rs"]
mod math_oracle;

#[test]
fn scalar_math_matches_f64_oracle_on_wasm32() {
    let math = include_str!("../../../stdlib/core/math.aru").replace("module std.core.math", "");
    let oracle = math_oracle::source()
        .replace("module mathOracle", "")
        .replace("import std.core.math as math", "")
        .replace("math.", "");
    let bytes = common::compile_source(&format!("{math}\n{oracle}"));
    assert_eq!(common::run_main_i32(&bytes), 0);
}
