//! SYN.3: `nil` as Option.None + match Some/None end-to-end.
//! SL_S thin: `import std.path` typechecks via stdlib/std rewrite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn run_cli_in(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("cli should run")
}

/// Regression: `?` (try), `??` (null-coalesce) and `match Some/None` on the real
/// `Option` ADT must agree on the tag layout (None=0, Some=1) at runtime,
/// regardless of the source declaration order in `stdlib/core/option.aru`.
#[test]
fn run_option_try_and_null_coalesce_agrees_on_tags() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_option_ops.aru");
    fs::write(
        &file,
        r#"
module tests.cli.option_ops

func try_option(opt: Option<int>): Option<int> {
    let val: int = opt?
    return Option.Some(val + 1)
}

func main(): int {
    // `?` on Some(10) unwraps to 10, then Some(11).
    let stepped = try_option(Option.Some(10))
    let r1 = match stepped {
        Some(x) => x
        None => 0
    }
    if r1 != 11 {
        return 100
    }
    // `?` on None short-circuits to Option.None (tag 0), caught by match None.
    let skipped = try_option(Option.None)
    let r2 = match skipped {
        Some(x) => x
        None => 999
    }
    if r2 != 999 {
        return 200
    }
    // `??` on None yields the fallback value.
    let none_val: Option<int> = Option.None
    let r3 = none_val ?? 77
    if r3 != 77 {
        return 300
    }
    return 42
}
"#,
    )
    .expect("write");

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let path = file.to_string_lossy();
    let run = run_cli_in(&root, &["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "Option ?/??/match tag layout: stderr={}\nstdout={}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn run_option_nil_and_some_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_option_nil.aru");
    fs::write(
        &file,
        r#"
module tests.cli.option_nil

func none_int(): Option<int> {
    return nil
}

func some_int(): Option<int> {
    return Option.Some(42)
}

func main(): int {
    let a = none_int()
    let n = match a {
        Some(v) => v
        None => 0
    }
    if n != 0 {
        return n
    }
    let b = some_int()
    return match b {
        Some(v) => v
        None => 0
    }
}
"#,
    )
    .expect("write");

    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "option nil/some run exit, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn check_std_path_import_sls() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_sls_path.aru");
    fs::write(
        &file,
        r#"
module tests.cli.sls_path

import std.path as path

func main(): int {
    let p = path.Path.from("/tmp")
    let _ = p.isAbsolute()
    return 0
}
"#,
    )
    .expect("write");

    // Walk-up from workspace root finds stdlib/std/path.aru.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let path = file.to_string_lossy();
    let check = run_cli_in(&root, &["check", &path]);
    assert!(
        check.status.success(),
        "SL_S path import check failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

/// Multi-file HIR linking: imported `std.path` bodies are codegen'd (not just signatures).
#[test]
fn run_std_path_is_empty_exits_0() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_sls_path_run.aru");
    fs::write(
        &file,
        r#"
module tests.cli.sls_path_run

import std.path as path

func main(): int {
    let emptyPath = path.Path.from("")
    let nonEmptyPath = path.Path.from("/tmp")
    let empty = emptyPath.isEmpty()
    let nonempty = nonEmptyPath.isEmpty()
    if !empty {
        return 1
    }
    if nonempty {
        return 2
    }
    let _ = nonEmptyPath.isAbsolute()
    return 0
}
"#,
    )
    .expect("write");

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let path = file.to_string_lossy();
    let run = run_cli_in(&root, &["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "std.path run failed: stderr={}\nstdout={}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
}
