//! Loop deletion removes only effect-free loops it proves finite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

const SOURCE: &str = r#"
import std.core.slice as slice
import io

func counted(n: usize): void {
    let mut i: usize = 0
    while i < n {
        i = i + 1
    }
}

func signedCount(n: int): void {
    let mut i: int = -5
    while n > i {
        i = i + 1
    }
}

func provenIndex(s: []u8): void {
    let mut i: usize = 0
    while i < slice.len<u8>(s) {
        let x = s[i]
        i = i + 1
    }
}

func forever(): void {
    while true {
    }
}

func byTwo(n: usize): void {
    let mut i: usize = 0
    while i < n {
        i = i + 2
    }
}

func notEqual(n: usize): void {
    let mut i: usize = 0
    while i != n {
        i = i + 1
    }
}

func usedAfter(n: usize): usize {
    let mut i: usize = 0
    let mut total: usize = 0
    while i < n {
        total = total + 3
        i = i + 1
    }
    return total
}

func mayTrap(s: []u8, n: usize): void {
    let mut i: usize = 0
    while i < n {
        let x = s[i]
        i = i + 1
    }
}

func effect(x: usize): void {
    io.println("tick")
}

func calls(n: usize): void {
    let mut i: usize = 0
    while i < n {
        effect(i)
        i = i + 1
    }
}

func nestedForever(n: usize): void {
    let mut i: usize = 0
    while i < n {
        while true {
        }
        i = i + 1
    }
}

func main(): int {
    counted(10)
    signedCount(3)
    let a: [3]u8 = [1 as u8, 2 as u8, 3 as u8]
    provenIndex(a[0..3])
    return usedAfter(4) as int
}
"#;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

/// Control-flow terminators (`branch`/`goto`) per function in the AMIR dump.
fn jumps(amir: &str, function: &str) -> usize {
    let mut current = "";
    let mut count = 0;
    for line in amir.lines() {
        if let Some(rest) = line.strip_prefix("Func ") {
            current = rest.split('(').next().unwrap_or_default();
        } else if current == function {
            let line = line.trim_start();
            count += usize::from(line.starts_with("branch ") || line.starts_with("goto "));
        }
    }
    count
}

#[test]
fn deletes_only_finite_effect_free_loops() {
    let file = std::env::temp_dir().join("arandu_cli_loop_deletion.aru");
    fs::write(&file, SOURCE).expect("write");
    let path = file.to_string_lossy();
    let output = cli(&["amir", "--release", &path]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let amir = String::from_utf8_lossy(&output.stdout);
    for deleted in ["counted", "signedCount", "provenIndex"] {
        assert_eq!(
            jumps(&amir, deleted),
            0,
            "{deleted} should be deleted:\n{amir}"
        );
    }
    // Possibly infinite (no `< bound` counter, a step other than one, a nested
    // `while true`), observable (a value used after the loop, an unproven
    // index that may trap, an effectful call): all stay.
    for kept in [
        "forever",
        "byTwo",
        "notEqual",
        "nestedForever",
        "usedAfter",
        "mayTrap",
        "calls",
    ] {
        assert!(jumps(&amir, kept) > 0, "{kept} must keep its loop:\n{amir}");
    }
    let run = cli(&["run", "--release", &path]);
    assert_eq!(
        run.status.code(),
        Some(12),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// An unused index still traps: DCE keeps an access that `bounds_check_elim`
/// did not prove, so every profile agrees.
#[test]
fn an_unused_out_of_bounds_index_traps_at_every_level() {
    let file = std::env::temp_dir().join("arandu_cli_dead_index_trap.aru");
    fs::write(
        &file,
        "func probe(s: []u8, i: usize): int {\n    let x = s[i]\n    return 7\n}\n\nfunc main(): int {\n    let a: [3]u8 = [1 as u8, 2 as u8, 3 as u8]\n    return probe(a[0..3], 9)\n}\n",
    )
    .expect("write");
    let path = file.to_string_lossy();
    for profile in [None, Some("--opt"), Some("--release")] {
        let mut args = vec!["run"];
        args.extend(profile);
        args.push(&path);
        let run = cli(&args);
        assert!(
            !run.status.success(),
            "{profile:?}: s[9] of a 3-byte slice must trap"
        );
    }
}
