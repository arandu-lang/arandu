//! Backend parity regressions from the 0.1.11 differential hunt: Cranelift
//! (`run`, `run --opt`), C at AMIR O2 and LLVM agree on results and traps.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use super::common::ProgramOutcome;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn llvm_outcome(source: &Path) -> Option<super::common::ProgramOutcome> {
    Command::new("clang").arg("--version").output().ok()?;
    let ir = cli(&["emit-llvm", &source.to_string_lossy()]);
    assert!(
        ir.status.success(),
        "{}",
        String::from_utf8_lossy(&ir.stderr)
    );
    let ll = source.with_extension("ll");
    let binary = source.with_extension("llvm.bin");
    fs::write(&ll, &ir.stdout).expect("write ll");
    let built = Command::new("clang")
        .args(["-O2", "-w"])
        .arg(&ll)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("clang");
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    Some(super::common::outcome(
        &Command::new(&binary).status().expect("run"),
    ))
}

/// Outcomes from every backend and level available on this host. LLVM
/// executables link no runtime, so programs that need one skip it.
fn outcomes_on(
    name: &str,
    source: &str,
    llvm: bool,
) -> Vec<(&'static str, super::common::ProgramOutcome)> {
    let file = std::env::temp_dir().join(format!("arandu_cli_parity_{name}.aru"));
    fs::write(&file, source).expect("write");
    let path = file.to_string_lossy().into_owned();
    let mut codes = vec![
        ("run", super::common::outcome(&cli(&["run", &path]).status)),
        (
            "run --opt",
            super::common::outcome(&cli(&["run", "--opt", &path]).status),
        ),
    ];
    if let Some(code) = super::common::c_release_outcome(&file) {
        codes.push(("c --release", code));
    }
    if llvm && let Some(code) = llvm_outcome(&file) {
        codes.push(("llvm", code));
    }
    codes
}

fn outcomes(name: &str, source: &str) -> Vec<(&'static str, super::common::ProgramOutcome)> {
    outcomes_on(name, source, true)
}

const RESULTS: &[(&str, &str, i32)] = &[
    (
        "narrow_shifts",
        r#"func shl(a: u8, b: u8): u8 { return a << b }
func shr(a: i16, b: i16): i16 { return a >> b }
func main(): int {
    return shl(1 as u8, 8 as u8) as int + (shr(-32768 as i16, 16 as i16) as int & 15)
}
"#,
        15,
    ),
    (
        "rem_of_min_by_minus_one",
        r#"func d(a: i32, b: i32): i32 { return a % b }
func main(): int {
    let x: i32 = -2147483647
    let y: i32 = x - 1
    return (d(y, -1) & 255) as int + 7
}
"#,
        7,
    ),
    (
        "division_rounds_toward_zero",
        r#"func d(a: i64, b: i64): i64 { return a / b + a % b }
func main(): int {
    return d(-17, 5) as int + 10
}
"#,
        5,
    ),
    (
        "unsigned_cast_zero_extends",
        r#"func c(x: usize): u32 { return (x as u8) as u32 }
func main(): int {
    let x: usize = 200
    return (c(x) >> 4) as int
}
"#,
        12,
    ),
    (
        "unsigned_cast_zero_extends_twice",
        r#"func c(x: i8): i64 { return (x as u16) as i64 }
func main(): int {
    let x: i8 = -3
    return (c(x) >> 14) as int
}
"#,
        3,
    ),
    (
        "signed_cast_sign_extends",
        r#"func c(x: u8): i32 { return (x as i8) as i32 }
func main(): int {
    let x: u8 = 250
    return (c(x) + 10) as int
}
"#,
        4,
    ),
    (
        "option_none_takes_the_other_operand_type",
        r#"func main(): int {
    let p: Option<u8> = Option.None
    if p == Option.None { return 4 }
    return 0
}
"#,
        4,
    ),
    (
        "zero_sized_array_elements",
        r#"struct Z {}
func main(): int {
    let a: [3]Z = [Z {}, Z {}, Z {}]
    let mut i: int = 0
    let mut c: int = 0
    while i < 3 {
        let z = a[i]
        c = c + 1
        i = i + 1
    }
    return c
}
"#,
        3,
    ),
    (
        "float_cast_wraps_after_widening",
        r#"func cv(x: f64): i8 { return x as i8 }
func main(): int {
    let x: f64 = 300.7
    return (cv(x) as int) % 256
}
"#,
        44,
    ),
    (
        "float_cast_in_range",
        r#"func cv(x: f64): u16 { return x as u16 }
func main(): int {
    let x: f64 = 1234.9
    return (cv(x) as int) % 256
}
"#,
        210,
    ),
    (
        "float_cast_negative",
        r#"func cv(x: f64): i64 { return x as i64 }
func main(): int {
    let x: f64 = -3.7
    return (cv(x) as int) % 256
}
"#,
        253,
    ),
    (
        "trailing_match_is_the_function_value",
        r#"func pick(v: u32): u32 {
    match v {
        0 => 7
        _ => 9
    }
}
func main(): int {
    return (pick(0) + pick(1) * 10) as int
}
"#,
        97,
    ),
];

#[test]
fn every_backend_computes_the_same_result() {
    for (name, source, expected) in RESULTS {
        for (backend, outcome) in outcomes(name, source) {
            assert_eq!(
                outcome,
                ProgramOutcome::Returned(*expected),
                "{name} on {backend}"
            );
        }
    }
}

const TRAPS: &[(&str, &str)] = &[
    (
        "unused_division_by_zero",
        r#"func f(z: u32): int {
    let r = 7 / z
    return 3
}
func g(a: i32, b: i32): int {
    let r = a % b
    return 3
}
func main(): int {
    return f(0)
}
"#,
    ),
    (
        "division_by_zero",
        r#"func d(a: u32, b: u32): u32 { return a / b }
func main(): int {
    let z: u32 = 0
    return d(7, z) as int + 3
}
"#,
    ),
    (
        "division_of_min_by_minus_one",
        r#"func d(a: i32, b: i32): i32 { return a / b }
func main(): int {
    let x: i32 = -2147483647
    let y: i32 = x - 1
    return d(y, -1) as int
}
"#,
    ),
    (
        "float_cast_below_unsigned_range",
        r#"func cv(x: f64): u8 { return x as u8 }
func main(): int {
    let x: f64 = -1.5
    return (cv(x) as int) % 256
}
"#,
    ),
    (
        "float_cast_above_range",
        r#"func cv(x: f64): i32 { return x as i32 }
func main(): int {
    let x: f64 = 1e20
    return (cv(x) as int) % 256
}
"#,
    ),
    (
        "float_cast_of_nan",
        r#"func cv(x: f64): i64 { return x as i64 }
func main(): int {
    let x: f64 = 0.0 / 0.0
    return (cv(x) as int) % 256
}
"#,
    ),
    (
        "float_cast_above_u64",
        r#"func cv(x: f64): u64 { return x as u64 }
func main(): int {
    let x: f64 = 1.9e19
    return (cv(x) as int) % 256
}
"#,
    ),
];

#[test]
fn every_backend_traps() {
    for (name, source) in TRAPS {
        for (backend, outcome) in outcomes(name, source) {
            assert_eq!(
                outcome,
                ProgramOutcome::Trapped,
                "{name} must trap on {backend}"
            );
        }
    }
}

#[test]
fn value_functions_must_return_on_every_path() {
    let rejected = "func f(n: u32): int {\n    if n > 2 {\n        return 5\n    }\n}\nfunc main(): int { return f(1) }\n";
    let file = std::env::temp_dir().join("arandu_cli_parity_missing_return.aru");
    fs::write(&file, rejected).expect("write");
    let output = cli(&["check", &file.to_string_lossy()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("T004"));

    // A loop without `break` and `abort` never reach the end of the body.
    let accepted = r#"
func f(n: u32): int {
    while true {
        if n > 2 { return 5 }
    }
}
func g(n: u32): int {
    let mut i: u32 = 0
    while true {
        i = i + 1
        if i > n { break }
    }
    return i as int
}
func main(): int { return f(3) + g(4) }
"#;
    for (backend, outcome) in outcomes("loops_that_return", accepted) {
        assert_eq!(outcome, ProgramOutcome::Returned(10), "{backend}");
    }
}

#[test]
fn float_text_matches_between_cranelift_and_c() {
    let source = r#"
func main(): int {
    let h: f64 = 0.1 + 0.2
    let c: f64 = 100.0
    let t: f64 = c * 1e18
    let s: f64 = c * 1.5e-12
    let e: f64 = c / 3.0
    let text = "${h}|${c}|${t}|${s}|${e}"
    if text == "0.30000000000000004|100|100000000000000000000|0.00000000015|33.333333333333336" {
        return 1
    }
    return 2
}
"#;
    for (backend, outcome) in outcomes_on("float_text", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(1), "{backend}");
    }
}

/// `T?` of a value type is a boxed handle on LLVM as on Cranelift and C. The
/// IR calls the runtime allocator, which a minimal stub provides here.
#[test]
fn value_nullables_match_on_llvm() {
    if Command::new("clang").arg("--version").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir();
    let stub = dir.join("arandu_cli_parity_raw_malloc.c");
    fs::write(
        &stub,
        "#include <stdint.h>\n#include <stdlib.h>\nvoid *ar_rt_raw_malloc(uintptr_t n) { return malloc(n); }\n",
    )
    .expect("write stub");
    for (name, source, expected) in RUNS_NULLABLE {
        let file = dir.join(format!("arandu_cli_parity_nullable_{name}.aru"));
        fs::write(&file, source).expect("write");
        let jit = super::common::outcome(&cli(&["run", &file.to_string_lossy()]).status);
        assert_eq!(jit, ProgramOutcome::Returned(*expected), "{name} on run");
        let ir = cli(&["emit-llvm", &file.to_string_lossy()]);
        assert!(
            ir.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&ir.stderr)
        );
        let ll = file.with_extension("ll");
        let binary = file.with_extension("llvm.bin");
        fs::write(&ll, &ir.stdout).expect("write ll");
        let built = Command::new("clang")
            .args(["-O2", "-w"])
            .arg(&ll)
            .arg(&stub)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("clang");
        assert!(
            built.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let code = super::common::outcome(&Command::new(&binary).status().expect("run"));
        assert_eq!(code, ProgramOutcome::Returned(*expected), "{name} on llvm");
    }
}

const RUNS_NULLABLE: &[(&str, &str, i32)] = &[
    (
        "struct_assigned_in_branch",
        "struct Inner { v: int }\nfunc main(): int {\n    let mut inn: Inner? = nil\n    let c = 1\n    if c > 0 { inn = Inner { v: 3 } }\n    return inn?.v ?? 7\n}\n",
        3,
    ),
    (
        "scalar_from_variable",
        "func main(): int {\n    let x = 5\n    let k: int? = x\n    let c = 1\n    if c > 0 { }\n    return k ?? 7\n}\n",
        5,
    ),
    (
        "nested_safe_navigation",
        "struct Inner { v: int }\nstruct Outer { inner: Inner? }\nfunc mk(): Inner? { return Inner { v: 4 } }\nfunc mko(): Outer? { return Outer { inner: mk() } }\nfunc main(): int {\n    return mko()?.inner?.v ?? 9\n}\n",
        4,
    ),
    (
        "struct_return_or_nil",
        "struct P { v: int }\nfunc g(x: int): P? {\n    if x > 0 { return P { v: x } }\n    return nil\n}\nfunc main(): int {\n    return (g(6)?.v ?? 0) + (g(-1)?.v ?? 10)\n}\n",
        16,
    ),
];
