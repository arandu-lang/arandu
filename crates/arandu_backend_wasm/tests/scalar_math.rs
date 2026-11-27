//! Execute scalar edge cases against independent host-f64 results.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
#[path = "../../../tests/support/math_oracle.rs"]
mod math_oracle;

#[test]
fn scalar_math_matches_f64_oracle_on_wasm32() {
    // The fused multiply-add helpers need the compiler ABI module, which a
    // user file cannot import; the oracle covers the scalar functions only.
    let math = include_str!("../../../stdlib/core/math.aru");
    let math = math
        .split("/// Fused multiply-add: `(a * b) + c`")
        .next()
        .unwrap_or(math)
        .replace("module std.core.math", "")
        .replace(
            "import std.core.intrinsics as intrinsics",
            "extern \"arandu-intrinsic\" {\n    func sqrt64(x: f64): f64\n}",
        )
        .replace("intrinsics.sqrt(x)", "sqrt64(x as f64) as float");
    let oracle = math_oracle::source()
        .replace("module mathOracle", "")
        .replace("import std.core.math as math", "")
        .replace("math.", "");
    let bytes = common::compile_trusted_core_fixture(&format!("{math}\n{oracle}"));
    assert_eq!(common::run_main_i32(&bytes), 0);
}
