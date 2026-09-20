//! SYN.2 string interpolation + SYN.4 advanced patterns (ranges, `_`, or).
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
fn syn2_dollar_name_and_brace_interp_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_syn2.aru");
    fs::write(
        &file,
        r#"
module tests.cli.syn2

import io

func main(): int {
    let name = "Ada"
    let n = 41
    let s = "hi $name count=${n + 1}"
    io.println(s)
    return n + 1
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "SYN.2 run, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hi Ada count=42"),
        "expected interp stdout, got: {stdout}"
    );
}

#[test]
fn syn4_range_wildcard_or_exits_30() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_syn4.aru");
    fs::write(
        &file,
        r#"
module tests.cli.syn4

func f(x: int): int {
    return match x {
        1 | 2 | 3 => 10
        4..=6 => 20
        _ => 0
    }
}

func main(): int {
    return f(2) + f(5) + f(9)
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(30),
        "SYN.4 or/range/wild, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn syn41_qualified_builtin_patterns_exit_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_syn41.aru");
    fs::write(
        &file,
        r#"
module tests.cli.syn41

func main(): int {
    let option: Option<int> = Option.Some(20)
    if option is Option.Some(left) {
        let result: Result<int, str> = Result.Ok(22)
        if result is Result.Ok(right) {
            return left + right
        }
    }
    return 0
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "SYN.4.1 run, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn syn5_keyword_member_name_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_syn5.aru");
    fs::write(
        &file,
        r#"
module tests.cli.syn5

struct Counter { value: int }

func Counter.set(value: int): int {
    return value
}

func main(): int {
    return Counter.set(42)
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "SYN.5 run, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn syn42_compound_pattern_conditions_exit_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_syn42.aru");
    fs::write(
        &file,
        r#"
module tests.cli.syn42

func explode(divisor: int): bool {
    return 1 / divisor == 0
}

func main(): int {
    let missing: Option<int> = Option.None
    if missing is Option.Some(unreachable) && explode(0) {
        return unreachable
    }
    let first: Option<int> = Option.Some(20)
    let second: Option<int> = Option.Some(22)
    if first is Option.Some(left) && left == 20 && second is Option.Some(right) {
        return left + right
    }
    return 0
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "SYN.4.2 run, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn typ4_scalar_const_generic_array_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_typ4.aru");
    fs::write(
        &file,
        r#"
module tests.cli.typ4

struct StaticMatrix<T, const M: uint, const N: uint> {
    data: [M][N]T
}

func StaticMatrix.set<T, const M: uint, const N: uint>(self: mut ref StaticMatrix<T, M, N>, r: uint, c: uint, value: T): void {
    self.data[r][c] = value
}

func sum<const M: uint, const N: uint>(matrix: ref StaticMatrix<int, M, N>): int {
    let mut total: int = 0
    let mut r: uint = 0
    while r < M {
        let mut c: uint = 0
        while c < N {
            total = total + matrix.data[r][c]
            c = c + 1
        }
        r = r + 1
    }
    return total
}

func main(): int {
    let mut matrix: StaticMatrix<int, 1, 2> = StaticMatrix<int, 1, 2> { data: [[20, 0]] }
    matrix.set(0, 1, 22)
    return sum(matrix)
}
"#,
    )
    .expect("write");
    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "TYP.4 run, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}
