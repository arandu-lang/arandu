//! PROMOTE-L6 complete: pure-buffer `Vec<T>` free-func API + gold runs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(workspace_root())
        .args(args)
        .output()
        .expect("cli should run")
}

#[test]
fn check_vec_t_with_default_allocator() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_vec_defaults.aru");
    fs::write(
        &file,
        r#"
module tests.cli.vec_defaults

import std.alloc.vec as vec

func main(): int {
    let v: vec.Vec<int> = vec.Vec<int>.new()
    return 0
}
"#,
    )
    .expect("write fixture");

    let out = run_cli(&["check", &file.to_string_lossy()]);
    assert!(
        out.status.success(),
        "check Vec<int> defaults failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_vec_push_get_destroy_exits_78() {
    let out = run_cli(&["run", "examples/minimal/m13_vec.aru"]);
    assert_eq!(
        out.status.code(),
        Some(78),
        "m13_vec run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn check_alloc_vec_module_clean() {
    let out = run_cli(&["check", "stdlib/alloc/vec.aru"]);
    assert!(
        out.status.success(),
        "stdlib/alloc/vec.aru must be check-clean for HIR link: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Regression: mut-ref field store must not SIGSEGV (param base Store kept).
#[test]
fn run_mut_ref_field_store_via_free_func() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_mut_ref_field.aru");
    fs::write(
        &file,
        r#"
module tests.cli.mut_ref_field

struct S {
    n: int
}

func set_n(mut s: S, v: int): void {
    s.n = v
}

func main(): int {
    let mut s = S { n: 0 }
    set_n(s, 42)
    return s.n
}
"#,
    )
    .expect("write");
    let out = run_cli(&["run", &file.to_string_lossy()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "mut-ref field store failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Pure-buffer growth past initial capacity (realloc path).
#[test]
fn run_vec_grow_past_8() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_vec_grow.aru");
    fs::write(
        &file,
        r#"
module tests.cli.vec_grow
import std.alloc.vec as vec

func main(): int {
    let mut v = vec.Vec<int>.new()
    v.push(1)
    v.push(2)
    v.push(3)
    v.push(4)
    v.push(5)
    v.push(6)
    v.push(7)
    v.push(8)
    v.push(9)
    v.push(10)
    let n = v.len() as int
    let last = v.getCopy(9)
    match last {
        Some(x) => {
            return n + x
        }
        None => {
            return 1
        }
    }
}
"#,
    )
    .expect("write");
    let out = run_cli(&["run", &file.to_string_lossy()]);
    assert_eq!(
        out.status.code(),
        Some(20),
        "vec grow past 8 failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// with_capacity / capacity / reserve / clear / is_empty (m15).
#[test]
fn run_m15_vec_capacity_api() {
    let out = run_cli(&["run", "examples/minimal/m15_vec_capacity.aru"]);
    assert_eq!(
        out.status.code(),
        Some(21),
        "m15 capacity API failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_m15_vec_capacity_under_opt() {
    let out = run_cli(&["run", "examples/minimal/m15_vec_capacity.aru", "--opt"]);
    assert_eq!(
        out.status.code(),
        Some(21),
        "m15 --opt failed (return-slot DCE?): stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_m13_under_opt() {
    let out = run_cli(&["run", "examples/minimal/m13_vec.aru", "--opt"]);
    assert_eq!(
        out.status.code(),
        Some(78),
        "m13 --opt failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_generic_default_ref_and_destructor() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_generic_default_destructor.aru");
    fs::write(
        &file,
        r#"
module tests.cli.generic_default_destructor

struct Counter<T, A = int> {
    val: T
    tag: A
}

@Destructor
func Counter.destroy<T, A>(self: Counter<T, A>): void {
}

func inspect<T>(c: &Counter<T>): T {
    return c.val
}

func main(): int {
    let c: Counter<int> = Counter { val: 99, tag: 1 }
    return inspect(c)
}
"#,
    )
    .expect("write");
    let out = run_cli(&["run", &file.to_string_lossy()]);
    assert_eq!(
        out.status.code(),
        Some(99),
        "generic default with destructor failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
