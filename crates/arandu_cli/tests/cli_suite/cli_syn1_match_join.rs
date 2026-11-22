//! Residual CFG fix (diverging match/if) + SYN.1 implicit tail return.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

#[test]
fn match_stmt_all_arms_return_exits_0() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_match_stmt_ret.aru");
    fs::write(
        &file,
        r#"
module tests.cli.match_stmt_ret

func main(): int {
    let x: Option<int> = nil
    match x {
        Some(v) => { return v }
        None => { return 0 }
    }
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "match stmt return arms, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn if_both_arms_return_exits_1() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_if_both_ret.aru");
    fs::write(
        &file,
        r#"
module tests.cli.if_both_ret

func main(): int {
    if true {
        return 1
    } else {
        return 2
    }
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(1),
        "if both return, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn implicit_tail_return_exits_44() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_syn1.aru");
    fs::write(
        &file,
        r#"
module tests.cli.syn1

func answer(): int {
    42
}

func add(a: int, b: int): int {
    let s = a + b
    s
}

func main(): int {
    answer() + add(1, 1)
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(44),
        "SYN.1 implicit return, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn void_tail_non_result_and_void_if_match_compile_and_run() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_void_tail_ergonomics.aru");
    fs::write(
        &file,
        r#"
module tests.cli.void_tail_ergonomics
import std.alloc.vec as vec

func setTail(v: mut ref vec.Vec<int>, idx: usize, val: int): void {
    v.set(idx, val)
}

func adjust(v: mut ref vec.Vec<int>, opt: Option<int>, flag: bool): void {
    if flag {
        v.set(0 as usize, 10)
    } else {
        let _unused = 1
    }
    match opt {
        Some(x) => { v.set(1 as usize, x) }
        None => {}
    }
}

func main(): int {
    let mut v = vec.Vec<int>.new()
    v.push(0)
    v.push(0)
    setTail(mut ref v, 0 as usize, 5)
    adjust(mut ref v, Option.Some(32), true)
    let a = v.getCopy(0 as usize) ?? -1
    let b = v.getCopy(1 as usize) ?? -1
    return a + b - 42
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy().into_owned();
    let run_dbg = run_cli(&["run", &path]);
    assert_eq!(
        run_dbg.status.code(),
        Some(0),
        "void tail ergonomics (debug), stderr={}",
        String::from_utf8_lossy(&run_dbg.stderr)
    );
    let run_opt = run_cli(&["run", "--opt", &path]);
    let _ = fs::remove_file(&file);
    assert_eq!(
        run_opt.status.code(),
        Some(0),
        "void tail ergonomics (opt), stderr={}",
        String::from_utf8_lossy(&run_opt.stderr)
    );
}
