#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

fn run_cli(args: &[&str]) -> std::process::Output {
    // The CLI's JIT runtime owns process-wide resources. Keep child CLI runs
    // serial inside this integration-test binary to avoid cross-test races.
    static CLI_RUN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = CLI_RUN_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("CLI test lock should not be poisoned");
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

#[test]
fn invalid_usage_exits_with_code_2() {
    let output = run_cli(&[]);

    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("usage:") && err.contains("arandu_cli"),
        "expected usage help, got: {err}"
    );
    assert!(err.contains("layout model only"));
    assert!(err.contains("cross compiler/sysroot are external"));
}

#[test]
fn version_is_stable_and_requires_no_project() {
    for flag in ["--version", "-V"] {
        let output = run_cli(&[flag]);
        assert!(output.status.success(), "{flag}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("arandu {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn missing_file_exits_with_code_1() {
    let output = run_cli(&["lex", "missing-file.aru"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to read"));
}

#[test]
fn run_forwards_only_arguments_after_separator_to_jit_program() {
    let file = std::env::temp_dir().join("arandu_cli_program_args.aru");
    fs::write(
        &file,
        r#"import io
import std.env as env

func main(): int {
    io.println(env.argsLen())
    io.println(env.arg(1))
    return 0
}
"#,
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path, "--", "program-sentinel"]);
    let _ = fs::remove_file(&file);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "2\nprogram-sentinel"
    );
}

#[test]
fn hash_map_with_user_hash_implementation_passes_amir_validation() {
    let file = std::env::temp_dir().join("arandu_cli_hash_map_check.aru");
    fs::write(
        &file,
        r#"import std.alloc.hash_map as hash_map
import std.core.hash as hash

struct Key { id: int }
public func Key.eq(self: ref Key, other: ref Key): bool { return self.id == other.id }
public func Key.hash<H: hash.Hasher>(self: ref Key, state: mut ref H): void {}

func main(): int {
    let mut map = hash_map.new<Key, int>()
    hash_map.insert(mut ref map, Key { id: 1 }, 10)
    return 0
}
"#,
    )
    .expect("HashMap fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    let _ = fs::remove_file(&file);
    assert!(
        output.status.success(),
        "HashMap's generic resize path must produce valid AMIR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn logical_operators_short_circuit_rhs_effects_with_and_without_opt() {
    let file = std::env::temp_dir().join("arandu_cli_short_circuit.aru");
    fs::write(
        &file,
        r#"import io

func observed(): bool {
    io.println("rhs evaluated")
    return true
}

func matchesMarker(value: u8): bool {
    return value == 32 || value == 9 || value == 13
}

func main(): int {
    let andValue = false && observed()
    let orValue = true || observed()
    if andValue || !orValue || matchesMarker(105) {
        return 1
    }
    return 0
}
"#,
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    for args in [
        vec!["run", path.as_ref()],
        vec!["run", "--opt", path.as_ref()],
    ] {
        let output = run_cli(&args);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "short-circuited RHS produced output: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let _ = fs::remove_file(file);
}

#[test]
fn lex_parse_and_check_valid_files_exit_successfully() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_smoke.aru");
    fs::write(
        &file,
        r"module tests.cli

func main() {
    let value = 1
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let lex = run_cli(&["lex", &path]);
    let parse = run_cli(&["parse", &path]);
    let check = run_cli(&["check", &path]);

    assert!(lex.status.success());
    assert!(String::from_utf8_lossy(&lex.stdout).contains("KW_MODULE"));
    assert!(parse.status.success());
    let parse_stdout = String::from_utf8_lossy(&parse.stdout);
    assert!(parse_stdout.contains("Func @"));
    assert!(parse_stdout.contains("main() -> void"));
    assert!(check.status.success());
    assert!(String::from_utf8_lossy(&check.stdout).contains("ok"));
}

#[test]
fn genref_report_is_opt_in_and_stays_off_stdout() {
    let file = std::env::temp_dir().join("arandu_cli_genref_report.aru");
    fs::write(&file, "func main(): int { return 0 }\n").expect("fixture");
    let path = file.to_string_lossy();

    let output = run_cli(&["check", &path, "--genref-report"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("ok {path}")
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[arandu][genref]"));
    assert!(stderr.contains("promotions=0 checks=0"));
}

/// Builtin prelude must resolve on the CLI/Salsa path without on-disk modules.
#[test]
fn check_import_io_prelude_succeeds() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_prelude_io.aru");
    fs::write(
        &file,
        r#"module tests.cli.prelude

import io
import err

func boom(): Result<int, Err> {
    return Result.Err(err.new("x"))
}

func main() {
    io.println("ok")
}
"#,
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));
}

/// Runnable `*_main.aru` demos must `run` with the expected exit codes.
#[test]
fn run_stable_main_demos() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = std::path::Path::new(manifest_dir)
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let demos = [
        ("examples/stable/syntax/enums_main.aru", 2),
        ("examples/stable/syntax/match_main.aru", 7),
        ("examples/stable/syntax/safe_main.aru", 3),
        ("examples/stable/syntax/try_main.aru", 42),
        ("examples/stable/syntax/fib_main.aru", 55),
        // Unix reports the low 8 bits; Windows preserves the full exit value.
        (
            "examples/stable/syntax/nullable_main.aru",
            if cfg!(windows) { 50099 } else { 179 },
        ),
        // catch 3*10+7 + prints missingPath
        ("examples/stable/syntax/catch_main.aru", 37),
    ];
    for (rel, expected) in demos {
        let path = workspace_root.join(rel);
        let path_str = path.to_string_lossy();
        let output = run_cli(&["run", &path_str]);
        assert!(
            output.status.success() || output.status.code() == Some(expected),
            "run failed for {rel}:\nstdout:{}\nstderr:{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.status.code(),
            Some(expected),
            "unexpected exit for {rel}"
        );
    }
}

/// Official stable examples must type-check end-to-end on the CLI.
#[test]
fn check_stable_examples_succeed() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = std::path::Path::new(manifest_dir)
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let stable_root = workspace_root.join("examples/stable");

    let mut files = Vec::new();
    fn collect_aru(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).expect("read examples/stable") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                collect_aru(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("aru") {
                out.push(path);
            }
        }
    }
    collect_aru(&stable_root, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "expected stable examples under {}",
        stable_root.display()
    );

    for file in files {
        let path = file.to_string_lossy();
        let output = run_cli(&["check", &path]);
        assert!(
            output.status.success(),
            "check failed for {}:\n{}",
            path,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn check_invalid_file_reports_name_error_and_exits_1() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_check_invalid.aru");
    fs::write(
        &file,
        r"module tests.cli

func main() {
    value = missing_name
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("N001"));
}

#[test]
fn check_missing_set_target_reports_specific_name_error() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_check_set_missing.aru");
    fs::write(
        &file,
        r"module tests.cli

func main() {
    missing = 1
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("N007"));
}

#[test]
fn amir_opt_flag_folds_constants_without_changing_default_command() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_amir_opt.aru");
    fs::write(
        &file,
        r"func main(): int {
    let value: int = 1 + 2
    return value
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let plain = run_cli(&["amir", &path]);
    let optimized = run_cli(&["amir", &path, "--opt"]);

    assert!(plain.status.success());
    assert!(optimized.status.success());

    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    let optimized_stdout = String::from_utf8_lossy(&optimized.stdout);
    assert!(plain_stdout.contains("add 1, 2"));
    assert!(!optimized_stdout.contains("add 1, 2"));
    assert!(optimized_stdout.contains("= 3"));
}

#[test]
#[cfg(target_pointer_width = "64")]
fn run_returns_main_exit_code() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_run_return.aru");
    fs::write(
        &file,
        r"func main(): int {
    return 42
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);

    assert_eq!(output.status.code(), Some(42));
}

#[test]
#[cfg(target_pointer_width = "64")]
fn run_signed_integer_arithmetic_exits_successfully() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_run_signed.aru");
    fs::write(
        &file,
        r"func main(): int {
    let div = -1 / 2
    let rem = -7 % 3
    if div != 0 {
        return 1
    }
    if rem != -1 {
        return 2
    }
    return 0
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);

    assert_eq!(output.status.code(), Some(0));
}

#[test]
#[cfg(target_pointer_width = "64")]
fn run_control_flow_returns_expected_code() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_run_control_flow.aru");
    fs::write(
        &file,
        r"func main(): int {
    let mut res = 0
    let a: int = 10
    let b: int = 20
    if a > b {
        res = a
    } else {
        res = b
    }
    return res
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);

    assert_eq!(output.status.code(), Some(20));
}

#[test]
#[cfg(target_pointer_width = "64")]
fn run_with_ztime_passes_emits_perf_timings() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_run_perf.aru");
    fs::write(
        &file,
        r"func main(): int {
    return 0
}
",
    )
    .expect("fixture should be writable");

    let path = file.to_string_lossy();
    let output = run_cli(&["-Ztime-passes", "run", &path]);

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[perf]")
            || stderr.contains("parse")
            || stderr.contains("type_check")
            || stderr.contains("codegen"),
        "expected perf output in stderr, got:\n{stderr}"
    );
}

/// ToStr v0.1: CLI check accepts println/interp of primitives.
#[test]
fn check_to_str_primitives_succeeds() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_to_str_check.aru");
    fs::write(
        &file,
        r#"module tests.cli.tostr

import io

func main(): int {
    let n: int = 7
    io.println(n)
    io.println("n=${n}")
    return 0
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ToStr v0.1: `run` executes println with int (prelude host stub).
#[test]
fn run_to_str_println_int_exits_0() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_to_str_run.aru");
    fs::write(
        &file,
        r#"module tests.cli.tostr_run

import io

func main(): int {
    let n: int = 42
    io.println(n)
    io.println("answer=${n}")
    return 0
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert!(
        output.status.success(),
        "stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42") && stdout.contains("answer=42"),
        "expected formatted output, got:\n{stdout}"
    );
}

/// ToStr v0.1: method `.to_str()` + fixed-width integers + float specials path.
#[test]
fn run_to_str_method_and_fixed_widths() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_to_str_method.aru");
    fs::write(
        &file,
        r#"module tests.cli.tostr_method

import io

func main(): int {
    let n: int = 42
    let i8v: i8 = -5
    let u64v: u64 = 99
    let f: float = 2.0
    io.println(n.to_str())
    io.println(i8v)
    io.println(u64v)
    io.println(f.to_str())
    io.println(true.to_str())
    return 0
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert!(
        output.status.success(),
        "stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in ["42", "-5", "99", "2", "true"] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}

/// ToStr v0.1: emit-c includes ToStr helpers and io_println stub.
#[test]
fn emit_c_to_str_includes_helpers_and_println() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_to_str_emit.aru");
    fs::write(
        &file,
        r#"module tests.cli.tostr_emit

import io

func main(): int {
    io.println(1)
    return 0
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["emit-c", &path, "--layout=host"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = String::from_utf8_lossy(&output.stdout);
    assert!(c.contains("ar_i64_to_str"), "missing ToStr helper:\n{c}");
    assert!(c.contains("io__println"), "missing io__println stub:\n{c}");
    assert!(c.contains("int main("), "missing main:\n{c}");
}

/// S5 gate: emit-c produces host C with int main and DataLayout-driven types.
#[test]
fn emit_c_host_fib_main_contains_int_main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fib = std::path::Path::new(manifest_dir)
        .join("../../examples/stable/syntax/fib_main.aru")
        .canonicalize()
        .expect("fib_main.aru");
    let path = fib.to_string_lossy();
    let output = run_cli(&["emit-c", &path, "--layout=host"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("int main("),
        "expected int main in C, got:\n{stdout}"
    );
    assert!(stdout.contains("fib("), "expected fib in C output");
    // No str runtime for pure-int program
    assert!(
        !stdout.contains("ar_str_concat_n"),
        "unexpected str runtime for fib_main"
    );
}

#[test]
fn emit_c_host_m25_fs_env_compiles_and_runs() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture = std::path::Path::new(manifest_dir)
        .join("../../examples/minimal/m25_fs_env.aru")
        .canonicalize()
        .expect("m25_fs_env.aru");
    let path = fixture.to_string_lossy();
    let output = run_cli(&["emit-c", &path, "--layout=host"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c_source = String::from_utf8_lossy(&output.stdout);
    assert!(c_source.contains("ar_fs_read_all"));
    assert!(c_source.contains("ar_fs_readdir"));
    assert!(c_source.contains("ar_env_arg"));

    if std::process::Command::new("gcc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let temp_dir = std::env::temp_dir();
        let c_file = temp_dir.join("arandu_test_m25_fs_env.c");
        let bin_name = if cfg!(windows) {
            "arandu_test_m25_fs_env.exe"
        } else {
            "arandu_test_m25_fs_env"
        };
        let bin_file = temp_dir.join(bin_name);
        fs::write(&c_file, c_source.as_bytes()).expect("write c file");
        let compile_output = std::process::Command::new("gcc")
            .arg(&c_file)
            .arg("-o")
            .arg(&bin_file)
            .output()
            .expect("invoke C compiler");
        assert!(
            compile_output.status.success(),
            "C compilation of m25_fs_env failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        );

        let run_output = std::process::Command::new(&bin_file)
            .current_dir(manifest_dir.to_owned() + "/../..")
            .output()
            .expect("run compiled m25_fs_env");
        let _ = fs::remove_file(&c_file);
        let _ = fs::remove_file(&bin_file);
        assert!(
            run_output.status.success(),
            "compiled m25_fs_env exited with non-zero:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&run_output.stdout),
            String::from_utf8_lossy(&run_output.stderr)
        );
    }
}

#[test]
fn emit_c_i686_uses_int32_for_platform_int() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_emit_c_i686.aru");
    fs::write(
        &file,
        r"func main(): int {
    return 7
}
",
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["emit-c", &path, "--layout=i686"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // main body still int; platform int locals/params use 32-bit when present
    assert!(stdout.contains("int main("));
    // return value temp is platform int width
    assert!(
        stdout.contains("int32_t") || stdout.contains("return (int)"),
        "expected 32-bit layout types:\n{stdout}"
    );
}

/// Regression: package demo (generics + Result/?/catch + nullable + enums) exits 42.
#[test]
fn run_package_main_exits_42() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/stable/syntax/package_main.aru"
    );
    let output = run_cli(&["run", path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Stable demos keep documented exit codes (Result/catch/enums/match/safe).
#[test]
fn run_stable_syntax_demos_exit_codes() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/stable/syntax/");
    let cases = [
        ("try_main.aru", 42),
        ("catch_main.aru", 37),
        ("enums_main.aru", 2),
        ("match_main.aru", 7),
        ("safe_main.aru", 3),
        ("fib_main.aru", 55),
    ];
    for (file, code) in cases {
        let path = format!("{root}{file}");
        let output = run_cli(&["run", &path]);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{file}: stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Simple free-function monomorphization: `id<int>` works end-to-end.
#[test]
fn run_generic_identity_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_id.aru");
    fs::write(
        &file,
        r#"func id<T>(x: T): T {
    return x
}

func main(): int {
    return id<int>(41) + 1
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Dual specialization: `id<str>` and `id<int>` expand to distinct mangled funcs
/// (str fat ABI must not share the int monomorph).
#[test]
fn run_generic_id_str_and_int_coexist() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_dual.aru");
    fs::write(
        &file,
        r#"func id<T>(x: T): T {
    return x
}

func main(): int {
    let s = id<str>("hi")
    let n = id<int>(41)
    return n + 1
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir dump failed: {}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let dump = String::from_utf8_lossy(&amir.stdout);
    assert!(
        dump.contains("_A$id$I_int_$E") && dump.contains("_A$id$I_str_$E"),
        "expected specialized mangled funcs, got:\n{dump}"
    );
    assert!(
        !dump.contains("Func id("),
        "generic template should not be lowered to AMIR:\n{dump}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Generic method monomorphization: `obj.m<int>(…)` expands and reuses `shared self`.
#[test]
fn run_generic_method_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_method.aru");
    fs::write(
        &file,
        r#"struct Holder {
    v: int
}

func Holder.id_val<T>(shared self, x: T): T {
    return x
}

func main(): int {
    let b = Holder { v: 10 }
    let n = b.id_val<int>(32)
    return n + b.v
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Dual method specialization: distinct mangled funcs for `id_val<int>` and `id_val<str>`.
#[test]
fn run_generic_method_str_and_int_coexist() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_method_dual.aru");
    fs::write(
        &file,
        r#"struct Holder {
    v: int
}

func Holder.id_val<T>(shared self, x: T): T {
    return x
}

func main(): int {
    let b = Holder { v: 1 }
    let n = b.id_val<int>(41)
    let s = b.id_val<str>("hi")
    return n + 1
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir dump failed: {}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let dump = String::from_utf8_lossy(&amir.stdout);
    assert!(
        dump.contains("id_val")
            && dump.contains("_A$")
            && dump.contains("int")
            && dump.contains("str"),
        "expected specialized method monomorphs, got:\n{dump}"
    );
    assert!(
        !dump.contains("Func Holder.id_val("),
        "generic method template should not be lowered to AMIR:\n{dump}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Generic struct + method monomorphization from receiver type args (`BoxG<int>.get`).
#[test]
fn run_generic_struct_method_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_struct_method.aru");
    fs::write(
        &file,
        r#"struct BoxG<T> {
    v: T
}

func BoxG.get(shared self): T {
    return self.v
}

func main(): int {
    let b = BoxG { v: 42 }
    return b.get()
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir dump failed: {}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let dump = String::from_utf8_lossy(&amir.stdout);
    assert!(
        dump.contains("_A$") && dump.contains("get") && dump.contains("int"),
        "expected specialized BoxG.get monomorph, got:\n{dump}"
    );
    assert!(
        !dump.contains("Func BoxG.get("),
        "generic method template must not lower to AMIR:\n{dump}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Free-function type-arg inference: `id(41)` without explicit `<int>`.
#[test]
fn run_generic_id_inferred_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_id_inferred.aru");
    fs::write(
        &file,
        r#"func id<T>(x: T): T {
    return x
}

func main(): int {
    return id(41) + 1
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Dual struct monomorphs for `BoxG<int>` and `BoxG<str>` methods.
#[test]
fn run_generic_struct_method_str_and_int_coexist() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_struct_dual.aru");
    fs::write(
        &file,
        r#"struct BoxG<T> {
    v: T
}

func BoxG.get(shared self): T {
    return self.v
}

func main(): int {
    let a = BoxG { v: 41 }
    let b = BoxG { v: "hi" }
    let s = b.get()
    return a.get() + 1
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir dump failed: {}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let dump = String::from_utf8_lossy(&amir.stdout);
    assert!(
        dump.contains("_A$")
            && dump.contains("get")
            && dump.contains("int")
            && dump.contains("str"),
        "expected int+str specialized get monomorphs, got:\n{dump}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `own self` moves the receiver; use-after-move is O001.
#[test]
fn check_own_self_use_after_move_fails() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_own_self_uam.aru");
    fs::write(
        &file,
        r#"struct Holder {
    handle: ptr[u8]
}

func Holder.take(own self): int {
    return 10
}

func main(): int {
    let b = Holder { handle: nil }
    let n = b.take()
    return n + b.take()
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected O001 failure, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("O001") || err.contains("moved"),
        "expected move diagnostic, got:\n{err}"
    );
}

/// M2: `&mut` then overlapping `&` while first loan is live → O003.
#[test]
fn check_o003_conflicting_borrows_fails() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_m2_o003.aru");
    fs::write(
        &file,
        r#"module tests.cli.m2_o003

func use_both(a: &mut int, b: &int): int {
    return *a
}

func main(): int {
    let n = 1
    let a = &mut n
    let b = &n
    return use_both(a, b)
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    assert!(
        !output.status.success(),
        "expected O003 failure, stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("O003") || err.contains("borrow conflict") || err.contains("mutable borrow"),
        "expected O003 diagnostic, got:\n{err}"
    );
}

/// Sequential borrows after the previous ref dies are allowed.
#[test]
fn run_sequential_borrows_exits_5() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_m2_seq.aru");
    fs::write(
        &file,
        r#"module tests.cli.m2_seq

func main(): int {
    let n = 5
    let a = &n
    let x = *a
    let b = &mut n
    let y = *b
    return x
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(5),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Unary `*` binds tighter than `+` (hand parser F2.0 fix).
#[test]
fn run_deref_add_exits_3() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_deref_add.aru");
    fs::write(
        &file,
        r#"module tests.cli.deref_add

func main(): int {
    let n = 1
    let m = 2
    let a = &n
    let b = &m
    return *a + *b
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// F2.3: returning `&local` is O010 (+ O004 note).
#[test]
fn check_return_ref_local_o010() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_o010.aru");
    fs::write(
        &file,
        r#"module tests.cli.o010

func bad(): &int {
    let x = 42
    return &x
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("O010") || err.contains("escape"),
        "expected O010, got:\n{err}"
    );
}

/// F2.0: `&T` / `*p` safe borrow + deref via stack home in Cranelift JIT.
#[test]
fn run_ref_borrow_deref_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_f20_ref.aru");
    fs::write(
        &file,
        r#"module tests.cli.f20_ref

func main(): int {
    let n = 42
    let p = &n
    return *p
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();

    let check = run_cli(&["check", &path]);
    assert!(
        check.status.success(),
        "check stderr:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );

    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir stderr:\n{}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("&") || amir_out.contains("Borrow") || amir_out.contains("&int"),
        "expected borrow in AMIR, got:\n{amir_out}"
    );

    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// F2.0: `&mut T` exclusive borrow + deref.
#[test]
fn run_refmut_borrow_deref_exits_7() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_f20_refmut.aru");
    fs::write(
        &file,
        r#"module tests.cli.f20_refmut

func main(): int {
    let n = 7
    let p = &mut n
    return *p
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(7),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// F2.0: automatic coercion `&mut T` → `&T` (exclusive may decay to shared).
#[test]
fn run_refmut_coerces_to_ref_exits_9() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_f20_refmut_coerce.aru");
    fs::write(
        &file,
        r#"module tests.cli.f20_coerce

func take_shared(p: &int): int {
    return *p
}

func main(): int {
    let n = 9
    return take_shared(&mut n)
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let check = run_cli(&["check", &path]);
    assert!(
        check.status.success(),
        "check stderr:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(9),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A3.1/A3.3: `await` inside `async { }` also Suspend-splits; ready state is stack.
#[test]
fn run_a3_async_block_suspend_stack_exits_11() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a33_block_suspend.aru");
    fs::write(
        &file,
        r#"module tests.cli.a33_block_suspend

async func tick(): int {
    return 1
}

func main(): int {
    let c = async {
        let a = 10
        let b = await tick()
        a + b
    }
    return await c
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir stderr:\n{}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("suspend"),
        "expected suspend inside async block lower, got:\n{amir_out}"
    );
    assert!(
        amir_out.contains("coroutine_ready.stack"),
        "expected stack-first CoroutineReady for async block, got:\n{amir_out}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(11),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A3.3: plain `async { 42 }` materialises as `coroutine_ready.stack`.
#[test]
fn run_a3_3_stack_ready_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a33_stack.aru");
    fs::write(
        &file,
        r#"module tests.cli.a33_stack

func main(): int {
    let x = async { 42 }
    return await x
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(amir.status.success());
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("coroutine_ready.stack"),
        "expected stack ready, got:\n{amir_out}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A3.6: await with disc/payload layout still runs (Cranelift block_on path).
#[test]
fn run_a3_6_await_ready_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a36_ready.aru");
    fs::write(
        &file,
        r#"module tests.cli.a36_ready

func main(): int {
    let x = async { 42 }
    return await x
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A3.5: Suspend.args carries live non-constant locals (dense state at frontier).
/// AMIR-only (no JIT). Constants may be folded out of params after phi simplify.
#[test]
fn check_a3_5_suspend_captures_live_local() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a35_capture.aru");
    fs::write(
        &file,
        r#"module tests.cli.a35_capture

async func tick(): int {
    return 1
}

async func outer(seed: int): int {
    let a = seed
    let _p = &a
    let b = await tick()
    return a + b + *_p
}

func main(): int {
    return 0
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir stderr:\n{}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    // Expect suspend with captured arg(s), e.g. `suspend _1 → bb1(_2)` or `bb1(…)`
    let has_suspend_args = amir_out.lines().any(|l| {
        let t = l.trim();
        t.starts_with("suspend") && t.contains('(') && t.contains(')')
    });
    assert!(
        has_suspend_args,
        "expected suspend with captured state args, got:\n{amir_out}"
    );
}

/// A3.1: `await` inside `async func` splits CFG with `suspend → resume`.
#[test]
fn run_a3_1_suspend_split_exits_11() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a31_suspend.aru");
    fs::write(
        &file,
        r#"module tests.cli.a31_suspend

async func inner(): int {
    return 1
}

async func outer(): int {
    let a = 10
    let b = await inner()
    return a + b
}

func main(): int {
    return await outer()
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir stderr:\n{}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("suspend") && amir_out.contains("bb1"),
        "expected suspend frontier in AMIR, got:\n{amir_out}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(11),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A3.4: local-only `&n` across `await` rewrites to pin-free `relative_borrow` (no O010).
/// Uses `check`/`amir` only — avoid Cranelift JIT in this suite when host is memory-tight.
#[test]
fn check_a3_4_relative_borrow_across_await() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a34_relative.aru");
    fs::write(
        &file,
        r#"module tests.cli.a34_relative

async func tick(): int {
    return 1
}

async func ok(): int {
    let n = 5
    let p = &n
    let x = await tick()
    return *p + x
}

func main(): int {
    return await ok()
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let check = run_cli(&["check", &path]);
    assert!(
        check.status.success(),
        "A3.4 should allow local-only ref across await, stderr:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let amir = run_cli(&["amir", &path]);
    assert!(amir.status.success());
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("relative_borrow") || amir_out.contains("RelativeBorrow"),
        "expected pin-free relative_borrow rewrite, got:\n{amir_out}"
    );
}

/// A3.2/A3.4: absolute ref escaped as call arg across `await` → still O010.
#[test]
fn check_a3_2_escaped_ref_across_await_o010() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a32_escape.aru");
    fs::write(
        &file,
        r#"module tests.cli.a32_escape

async func tick(): int {
    return 1
}

func take(p: &int): int {
    return *p
}

async func bad(): int {
    let n = 5
    let p = &n
    let x = await tick()
    return take(p) + x
}

func main(): int {
    return await bad()
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["check", &path]);
    assert!(
        !output.status.success(),
        "expected O010 for escaped ref, stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("O010") || err.contains("await") || err.contains("suspension"),
        "expected O010 escaped ref across await, got:\n{err}"
    );
}

/// A3.0: ready-only `async { … }` + `await` via CoroutineReady state blob.
#[test]
fn run_a3_async_block_await_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a3_async_block.aru");
    fs::write(
        &file,
        r#"module tests.cli.a3_async_block

func main(): int {
    let x = async { 42 }
    return await x
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir stderr:\n{}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("coroutine_ready") || amir_out.contains("await"),
        "expected coroutine_ready/await in AMIR, got:\n{amir_out}"
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A3.0: `async func f(): T` ≡ `func f(): Coroutine[T]` + await drive.
#[test]
fn run_a3_async_func_await_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_a3_async_func.aru");
    fs::write(
        &file,
        r#"module tests.cli.a3_async_func

async func answer(): int {
    return 42
}

func main(): int {
    return await answer()
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let check = run_cli(&["check", &path]);
    assert!(
        check.status.success(),
        "check stderr:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// BC.4a: reborrow through `Deref` place (`&*p`) — materialised pointer local, no stack_addr.
#[test]
fn run_bc4a_reborrow_deref_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_bc4a_reborrow.aru");
    fs::write(
        &file,
        r#"module tests.cli.bc4a_reborrow

func main(): int {
    let n = 42
    let p = &n
    let q = &*p
    return *q
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();

    let amir = run_cli(&["amir", &path]);
    assert!(
        amir.status.success(),
        "amir stderr:\n{}",
        String::from_utf8_lossy(&amir.stderr)
    );
    let amir_out = String::from_utf8_lossy(&amir.stdout);
    assert!(
        amir_out.contains("&(*") || amir_out.contains("Deref") || amir_out.contains("&(*s"),
        "expected Deref place in AMIR Borrow, got:\n{amir_out}"
    );

    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// BC.4a: borrow of field through materialised object pointer (`&p.x` on Named/heap).
#[test]
fn run_bc4a_field_borrow_exits_11() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_bc4a_field.aru");
    fs::write(
        &file,
        r#"module tests.cli.bc4a_field

struct Point {
    x: int
    y: int
}

func main(): int {
    let p = Point { x: 11, y: 22 }
    let r = &p.x
    return *r
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(11),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// BC.4a: reborrow passed to callee (pointer identity through Deref place).
#[test]
fn run_bc4a_reborrow_call_exits_33() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_bc4a_reborrow_call.aru");
    fs::write(
        &file,
        r#"module tests.cli.bc4a_reborrow_call

func read(p: &int): int {
    return *p
}

func main(): int {
    let n = 33
    let p = &n
    let q = &*p
    return read(q)
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(33),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `shared self` does not move; receiver field usable after call.
#[test]
fn run_shared_self_reuse_exits_40() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_shared_self_reuse.aru");
    fs::write(
        &file,
        r#"struct Holder {
    v: int
}

func Holder.get(shared self): int {
    return self.v
}

func main(): int {
    let b = Holder { v: 20 }
    let n = b.get()
    return n + b.v
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();
    let output = run_cli(&["run", &path]);
    assert_eq!(
        output.status.code(),
        Some(40),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn amir_cfg_dot_and_ascii_flags() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_amir_cfg_test.aru");
    fs::write(
        &file,
        r#"func main(): int {
    let a = 1
    let b = 2
    return a + b
}
"#,
    )
    .expect("fixture");
    let path = file.to_string_lossy();

    // 1. DOT output
    let dot_output = run_cli(&["amir", &path, "--cfg"]);
    assert!(
        dot_output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&dot_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&dot_output.stdout);
    assert!(stdout.contains("digraph \"CFG_main\""), "got:\n{stdout}");
    assert!(stdout.contains("bb0"), "got:\n{stdout}");

    // 2. ASCII output
    let ascii_output = run_cli(&["amir", &path, "--cfg", "--ascii"]);
    assert!(
        ascii_output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&ascii_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&ascii_output.stdout);
    assert!(stdout.contains("=== CFG: main ==="), "got:\n{stdout}");
    assert!(stdout.contains("bb0"), "got:\n{stdout}");
    assert!(stdout.contains("(returns)"), "got:\n{stdout}");
}
