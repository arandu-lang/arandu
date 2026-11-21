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
        // A generic literal infers its parameters through nested literals,
        // nominal values, generic fields and plain calls.
        "generic_literal_inference",
        r#"struct Fresh { tag: int }
struct One<T> { value: T }
struct Pair<T> { first: T, second: T }
struct Holder<T> { inner: One<T>, count: int }
func makeFresh(tag: int): Fresh { return Fresh { tag: tag } }
func main(): int {
    let p = Pair { first: Fresh { tag: 1 }, second: Fresh { tag: 0 } }
    let h = Holder { inner: One { value: Fresh { tag: 5 } }, count: 2 }
    let o = One { value: makeFresh(9) }
    let n = One { value: One { value: p } }
    return p.first.tag + h.inner.value.tag + o.value.tag + n.value.value.second.tag
}
"#,
        15,
    ),
    (
        // `*holder.field` loads the pointee, not the reference bits.
        "deref_of_reference_field",
        r#"struct Env { n: mut ref int, k: ref int }
func read(e: Env): int { return *e.n + *e.k }
func main(): int {
    let mut n = 5
    let k = 2
    return read(Env { n: mut ref n, k: ref k })
}
"#,
        7,
    ),
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
        // Reduced from fuzz seed rfc22-smith-stdlib: the remainder by the
        // empty vector's length traps. Before it, LLVM read a scalar enum
        // payload as the byte union of the payload field, and rejected the
        // backslash character literal.
        "enum_scalar_payload_before_a_trap",
        r#"import std.alloc.vec as vec

enum GeneratedStepError {
    StepFailed,
}

func generated_try_step(v: int, fail: bool): Result<int, GeneratedStepError> {
    if fail {
        return Result.Err(GeneratedStepError.StepFailed)
    }
    return Result.Ok(v + 10)
}

func generated_try_pipeline(v: int, fail_first: bool, fail_second: bool): Result<int, GeneratedStepError> {
    let a = generated_try_step(v, fail_first)?
    let b = generated_try_step(a, fail_second)?
    return Result.Ok(b + 5)
}


struct Sample {
    left: int
    right: int
    enabled: bool
    character: char
}

enum Choice {
    Left(int),
    Right(int),
    Flag(bool),
    Letter(char),
}


func identity<T>(value: T): T {
    return value
}




func transfer<T>(own value: T): T {
    return value
}

func relay<T>(own value: T): T {
    return value
}

func make_vec<T>(): vec.Vec<T> {
    return vec.Vec<T>.new()
}

func read_ref(value: ref int): int {
    return *value
}

func read_exclusive(value: mut ref int): int {
    return *value
}

func adjust(value: int, enabled: bool): int {
    if enabled { return value + 2 }
    return value - 2
}



func main(): int {
    let base: int = 2
    let uint_base: uint = 2 as uint
    let values = [identity<int>((21 - ((base + (base - ((24 - base) % 7))) * 2))), identity<int>(base), base]
    let index: int = base % 3
    let mut total: int = 0
    let generated_character: char = '\\'
    let sample = Sample { left: values[index], right: values[(index + 1) % 3] + total, enabled: identity<bool>(((((uint_base / 2 as uint) * 2 as uint) * 2 as uint) == ((uint_base + (((uint_base + uint_base) % 2 as uint) % 2 as uint)) + (((uint_base * 2 as uint) % 2 as uint) + (26 as uint + ((uint_base / 2 as uint) + (5 as uint * 2 as uint))))))), character: identity<char>(generated_character) }
    let mut borrow_target: int = sample.left
    let shared_value = read_ref(ref borrow_target)
    let exclusive_value = read_exclusive(mut ref borrow_target)
    let mut dynamic = transfer<vec.Vec<int>>(relay<vec.Vec<int>>(make_vec<int>()))
    let dynamic_len = dynamic.len() as int
    let dynamic_index = (base % dynamic_len) as usize
    let dynamic_element = dynamic.getCopy(dynamic_index)
    let dynamic_value = match dynamic_element {
        Some(value) => value
        None => 0
    }
    let adjusted = adjust(shared_value, sample.enabled) + adjust(exclusive_value, !sample.enabled) + dynamic_value
    let selected = if base % 3 == 0 { Choice.Flag(sample.enabled) } else if base % 3 == 1 { Choice.Left(adjusted) } else { Choice.Right(sample.right) }
    let result = match selected {
        Choice.Left(value) => value
        Choice.Right(value) => value
        Choice.Flag(enabled) => if enabled { 1 } else { 0 }
        Choice.Letter(character) => if character == sample.character { 1 } else { -73 }
    }
    return result
}
"#,
    ),
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

/// The standard error stream of a native binary built by `compiler` from
/// the source `emit` prints, when that compiler is installed.
fn native_stderr(source: &Path, emit: &str, compiler: &str, extension: &str) -> Option<String> {
    Command::new(compiler).arg("--version").output().ok()?;
    let emitted = cli(&[emit, &source.to_string_lossy()]);
    assert!(
        emitted.status.success(),
        "{emit}: {}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let text = source.with_extension(extension);
    let binary = source.with_extension(format!("{extension}.bin"));
    fs::write(&text, &emitted.stdout).expect("write emitted source");
    let built = Command::new(compiler)
        .args(["-O2", "-w"])
        .arg(&text)
        .arg("-o")
        .arg(&binary)
        .arg("-lm")
        .output()
        .expect("compile emitted source");
    assert!(
        built.status.success(),
        "{compiler}: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&binary).output().expect("run binary");
    assert_eq!(super::common::outcome(&run.status), ProgramOutcome::Trapped);
    Some(String::from_utf8_lossy(&run.stderr).into_owned())
}

/// A failed run-time check says which check failed before it traps, on
/// every native backend, like Rust's panics; the exit status stays the
/// platform's trap.
#[test]
fn every_backend_reports_the_failed_check() {
    let cases = [
        (
            "trap_division",
            "func div(a: int, b: int): int { return a / b }\nfunc main(): int { return div(7, 0) }\n",
            "panic: attempt to divide by zero",
        ),
        (
            "trap_division_overflow",
            "func div(a: i32, b: i32): i32 { return a / b }\nfunc main(): int { return div(-2147483647 - 1, -1) as int }\n",
            "panic: attempt to divide with overflow",
        ),
        (
            "trap_index",
            "func at(xs: []int, i: usize): int { return xs[i] }\nfunc main(): int {\n    let xs = [1, 2]\n    return at(xs, 5)\n}\n",
            "panic: index out of bounds",
        ),
        (
            "trap_range",
            "func part(xs: []int, a: usize, b: usize): usize { return xs[a..b].len() }\nfunc main(): int {\n    let xs = [1, 2]\n    return part(xs, 1, 9) as int\n}\n",
            "panic: range out of bounds",
        ),
    ];
    for (name, source, message) in cases {
        let file = std::env::temp_dir().join(format!("arandu_cli_parity_{name}.aru"));
        fs::write(&file, source).expect("write");
        let path = file.to_string_lossy().into_owned();
        for mode in [&["run"][..], &["run", "--opt"][..]] {
            let output = cli(&[mode, &[path.as_str()][..]].concat());
            assert_eq!(
                super::common::outcome(&output.status),
                ProgramOutcome::Trapped,
                "{name} {mode:?}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(message), "{name} {mode:?}: {stderr}");
        }
        for (emit, compiler, extension) in [("emit-c", "cc", "c"), ("emit-llvm", "clang", "ll")] {
            if let Some(stderr) = native_stderr(&file, emit, compiler, extension) {
                assert!(stderr.contains(message), "{name} {emit}: {stderr}");
            }
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
fn float_text_matches_between_cranelift_c_and_wasm() {
    let source = r#"
func main(): int {
    let h: f64 = 0.1 + 0.2
    let c: f64 = 100.0
    let t: f64 = c * 1e18
    let s: f64 = c * 1.5e-12
    let e: f64 = c / 3.0
    let n: f64 = 0.0 - e
    let m: f64 = 1e300 * 1e8
    let d: f64 = 5e-324
    let text = "${h}|${c}|${t}|${s}|${e}|${n}|${d}"
    if text != "0.30000000000000004|100|100000000000000000000|0.00000000015|33.333333333333336|-33.333333333333336|0.000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000005" {
        return 2
    }
    // 1e308 is 1 and 308 zeros.
    let big = "${m}"
    if big.len != 309 {
        return 3
    }
    return 1
}
"#;
    for (backend, outcome) in outcomes_on("float_text", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(1), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_float_text_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(1, "float text in Wasmtime");
    let _ = fs::remove_file(path);
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

/// Non-escaping callbacks (RFC 0032, L1) are closure structs called directly:
/// every backend, Wasm included, computes the same results.
#[test]
fn closure_callbacks_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/closure_callbacks.aru");
    for (backend, outcome) in outcomes("closure_callbacks", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!(
        "arandu_closure_callbacks_{}.aru",
        std::process::id()
    ));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "closure callbacks in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Every spelling of a generic receiver (`Pair`, `ref Pair`, `ref Pair<T>`,
/// restated `<T>`) and method-only parameters agree on every backend.
#[test]
fn generic_receivers_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/generic_receivers.aru");
    for (backend, outcome) in outcomes("generic_receivers", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!(
        "arandu_generic_receivers_{}.aru",
        std::process::id()
    ));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "generic receivers in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Calls through function values (RFC 0032, L2) agree on every backend,
/// including Wasm's function table.
#[test]
fn function_values_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/function_values.aru");
    for (backend, outcome) in outcomes("function_values", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path =
        std::env::temp_dir().join(format!("arandu_function_values_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "function values in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Owning closures (RFC 0032, L3): returned, stored in fields, passed by
/// `mut ref` and by value, with their environments destroyed exactly once.
/// LLVM links no runtime, so its IR is only compiled to an object there.
#[test]
fn own_closures_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/own_closures.aru");
    for (backend, outcome) in outcomes_on("own_closures", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_own_closures_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "own closures in Wasmtime");
    let ir = cli(&["emit-llvm", &path.to_string_lossy()]);
    assert!(
        ir.status.success(),
        "{}",
        String::from_utf8_lossy(&ir.stderr)
    );
    if Command::new("clang").arg("--version").output().is_ok() {
        let ll = path.with_extension("ll");
        fs::write(&ll, &ir.stdout).unwrap();
        let object = Command::new("clang")
            .args(["-c", "-O2", "-w"])
            .arg(&ll)
            .arg("-o")
            .arg(path.with_extension("o"))
            .output()
            .expect("clang");
        assert!(
            object.status.success(),
            "{}",
            String::from_utf8_lossy(&object.stderr)
        );
        let _ = fs::remove_file(ll);
        let _ = fs::remove_file(path.with_extension("o"));
    }
    let _ = fs::remove_file(path);
}

/// Closures over values of generic type, borrowed and owning, instantiated
/// once per instance of the enclosing item, on every backend.
#[test]
fn generic_closures_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/generic_closures.aru");
    for (backend, outcome) in outcomes_on("generic_closures", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!(
        "arandu_generic_closures_{}.aru",
        std::process::id()
    ));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "generic closures in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Borrowing a value with no place borrows a temporary on every backend.
#[test]
fn rvalue_borrows_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/rvalue_borrows.aru");
    for (backend, outcome) in outcomes_on("rvalue_borrows", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
}

/// Arrays become slices where one is expected and answer slice methods.
#[test]
fn array_slices_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/array_slices.aru");
    for (backend, outcome) in outcomes_on("array_slices", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_array_slices_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "array slices in Wasmtime");
    let _ = fs::remove_file(path);
}

/// `+` and `+=` on `str` concatenate on every backend, in chains, loops and
/// with interpolated operands.
#[test]
fn string_concatenation_agrees_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/string_concat.aru");
    for (backend, outcome) in outcomes_on("string_concat", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path =
        std::env::temp_dir().join(format!("arandu_string_concat_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "string concatenation in Wasmtime");
    let _ = fs::remove_file(path);
}

/// An `@Export` function with a scalar signature used as a function value,
/// and a raw pointer indexed in a place, agree on every backend.
#[test]
fn exported_function_values_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/exported_function_values.aru");
    for (backend, outcome) in outcomes("exported_function_values", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!(
        "arandu_exported_function_values_{}.aru",
        std::process::id()
    ));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout)
        .assert_returns(0, "exported function values in Wasmtime");
    let _ = fs::remove_file(path);
}

/// `p.offsetFrom(q)` on pointers to structs and bytes, beside a method of
/// the pointee called through the pointer, agrees on every backend.
#[test]
fn pointer_offsets_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/pointer_offsets.aru");
    for (backend, outcome) in outcomes("pointer_offsets", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path =
        std::env::temp_dir().join(format!("arandu_pointer_offsets_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "pointer offsets in Wasmtime");
    let _ = fs::remove_file(path);
}

/// C strings as the initial values of `static mut` pointers agree on every
/// backend: the data image points at their NUL-terminated bytes.
#[test]
fn static_c_strings_agree_on_every_backend() {
    // Wasm is left out: reading a `ptr[u8]` stored in a `static mut` and
    // indexing it fails there even when assigned at run time, a defect of
    // the Wasm backend recorded in the roadmap, not of the data image.
    let source = include_str!("../../../../tests/corpus/static_c_strings.aru");
    for (backend, outcome) in outcomes("static_c_strings", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
}

/// `types.info<T>()`: descriptions of structs, enums, tuples and builtin
/// types, in a `const` and in a generic function, agree on every backend.
#[test]
fn type_descriptions_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/type_info.aru");
    for (backend, outcome) in outcomes("type_info", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_type_info_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "type descriptions in Wasmtime");
    let _ = fs::remove_file(path);
}

/// A `const` holding views of elements lives in static storage: functions
/// return it and every backend reads the same elements.
#[test]
fn const_views_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/const_views.aru");
    for (backend, outcome) in outcomes("const_views", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_const_views_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "const views in Wasmtime");
    let _ = fs::remove_file(path);
}

/// `static mut` storage holding text, byte views and arrays of text: the
/// data image points at objects of its own on every backend.
#[test]
fn static_text_agrees_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/static_text.aru");
    for (backend, outcome) in outcomes("static_text", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_static_text_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "static text in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Values moved on some paths only, and or-patterns that bind names.
#[test]
fn conditional_moves_and_or_patterns_agree_on_every_backend() {
    for (name, source) in [
        (
            "conditional_moves",
            include_str!("../../../../tests/corpus/conditional_moves.aru"),
        ),
        (
            "guarded_or_patterns",
            include_str!("../../../../tests/corpus/guarded_or_patterns.aru"),
        ),
    ] {
        for (backend, outcome) in outcomes_on(name, source, false) {
            assert_eq!(outcome, ProgramOutcome::Returned(0), "{name} on {backend}");
        }
        let path = std::env::temp_dir().join(format!("arandu_{name}_{}.aru", std::process::id()));
        fs::write(&path, source).unwrap();
        let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
        assert!(
            wasm.status.success(),
            "{}",
            String::from_utf8_lossy(&wasm.stderr)
        );
        crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, name);
        let _ = fs::remove_file(path);
    }
}

/// Range values: `Range<T>` and `RangeInclusive<T>` stored, passed,
/// returned, looped over and used as slice indexes.
#[test]
fn range_values_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/range_values.aru");
    for (backend, outcome) in outcomes_on("range_values", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_range_values_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "range values in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Tuple values: construction, `t.0`, element assignment, destructuring in
/// `let` and `match`, tuples in parameters and results, and `for i, x`.
#[test]
fn tuples_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/tuples.aru");
    for (backend, outcome) in outcomes_on("tuples", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_tuples_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "tuples in Wasmtime");
    let _ = fs::remove_file(path);
}

/// `mem.replace`, `mem.swap` and `mem.take` on values with destructors.
#[test]
fn mem_replace_of_owned_values_agrees_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/mem_replace_owned.aru");
    for (backend, outcome) in outcomes_on("mem_replace_owned", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
}

/// A block after a call is its last argument, `() => { ... }`: borrowed
/// callbacks, mutation of a local through one and an owned closure.
#[test]
fn trailing_blocks_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/trailing_blocks.aru");
    for (backend, outcome) in outcomes_on("trailing_blocks", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path =
        std::env::temp_dir().join(format!("arandu_trailing_blocks_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "trailing blocks in Wasmtime");
    let _ = fs::remove_file(path);
}

/// `own once (A) -> R` closures consume their captures, convert from
/// `own (A) -> R`, run as temporaries and destroy captures never called.
#[test]
fn once_closures_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/once_closures.aru");
    for (backend, outcome) in outcomes_on("once_closures", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path =
        std::env::temp_dir().join(format!("arandu_once_closures_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "once closures in Wasmtime");
    let ir = cli(&["emit-llvm", &path.to_string_lossy()]);
    assert!(
        ir.status.success(),
        "{}",
        String::from_utf8_lossy(&ir.stderr)
    );
    let _ = fs::remove_file(path);
}

/// Inference variables (S1): element and result types fixed by later uses,
/// including a literal whose width a later call decides.
#[test]
fn inference_variables_agree_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/inference_vars.aru");
    for (backend, outcome) in outcomes_on("inference_vars", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path =
        std::env::temp_dir().join(format!("arandu_inference_vars_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "inference variables in Wasmtime");
    let ir = cli(&["emit-llvm", &path.to_string_lossy()]);
    assert!(
        ir.status.success(),
        "{}",
        String::from_utf8_lossy(&ir.stderr)
    );
    let _ = fs::remove_file(path);
}

/// `own () -> R + Send` closures spawned directly and from a queue. Native
/// backends with the thread runtime only.
#[test]
fn send_closures_run_on_threads_on_native_backends() {
    let source = include_str!("../../../../tests/corpus/own_closures_threads.aru");
    for (backend, outcome) in outcomes_on("own_closures_threads", source, false) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
}

/// `*place = value` writes through `mut ref`, through a reference to a
/// reference and into `static mut` storage, on every backend.
#[test]
fn deref_assignment_agrees_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/deref_assign.aru");
    for (backend, outcome) in outcomes("deref_assign", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_deref_assign_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "deref assignment in Wasmtime");
    let _ = fs::remove_file(path);
}

/// `static mut` storage starts from its constant image and is shared by every
/// function, on every backend, Wasm included.
#[test]
fn static_mut_storage_agrees_on_every_backend() {
    let source = include_str!("../../../../tests/corpus/static_mut.aru");
    for (backend, outcome) in outcomes("static_mut", source) {
        assert_eq!(outcome, ProgramOutcome::Returned(0), "{backend}");
    }
    let path = std::env::temp_dir().join(format!("arandu_static_mut_{}.aru", std::process::id()));
    fs::write(&path, source).unwrap();
    let wasm = cli(&["emit-wasm", &path.to_string_lossy()]);
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    crate::wasm_host::run_main(&wasm.stdout).assert_returns(0, "static mut in Wasmtime");
    let _ = fs::remove_file(path);
}

/// Moved and borrowed captures of owned runtime values. LLVM, which links no
/// runtime, is left out.
#[test]
fn closure_owned_value_captures_agree_on_native_backends() {
    for (name, source) in [
        (
            "closure_move_capture",
            include_str!("../../../../tests/corpus/closure_move_capture.aru"),
        ),
        (
            "closure_borrow_capture",
            include_str!("../../../../tests/corpus/closure_borrow_capture.aru"),
        ),
    ] {
        for (backend, outcome) in outcomes_on(name, source, false) {
            assert_eq!(outcome, ProgramOutcome::Returned(0), "{name} on {backend}");
        }
    }
}
