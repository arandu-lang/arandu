//! `base[start..end]` lowers to a checked subslice of an array or slice.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn run_everywhere(name: &str, source: &str) -> Vec<Option<i32>> {
    let file = std::env::temp_dir().join(format!("arandu_cli_range_index_{name}.aru"));
    fs::write(&file, source).expect("write");
    let path = file.to_string_lossy();
    let mut codes: Vec<Option<i32>> = [None, Some("--opt")]
        .into_iter()
        .map(|profile| {
            let mut args = vec!["run"];
            args.extend(profile);
            args.push(&path);
            cli(&args).status.code()
        })
        .collect();
    codes.extend(super::common::c_release_exit_code(std::path::Path::new(
        &*path,
    )));
    codes
}

#[test]
fn range_indices_slice_arrays_and_slices() {
    let source = r#"
import std.core.slice as slice

func sum(s: []u8): int {
    let mut acc: int = 0
    let mut i: usize = 0
    while i < slice.len<u8>(s) {
        acc = acc + (s[i] as int)
        i = i + 1
    }
    return acc
}

func main(): int {
    let a: [5]u8 = [1 as u8, 2 as u8, 3 as u8, 4 as u8, 5 as u8]
    let whole: []u8 = a[0..5]
    let tail: []u8 = whole[2..=4]
    let mid: []u8 = whole[1..3]
    let empty: []u8 = whole[5..5]
    return sum(tail) * 10 + sum(mid) + sum(empty) + (slice.len<u8>(tail) as int) * 100
}
"#;
    // tail = [3, 4, 5], mid = [2, 3], empty = []: 120 + 5 + 0 + 300 = 425,
    // which the exit status reports modulo 256.
    let codes = run_everywhere("values", source);
    assert!(codes.iter().all(|code| *code == Some(169)), "{codes:?}");
}

#[test]
fn out_of_range_bounds_trap() {
    for (name, ty, range, lo, hi) in [
        ("past_end", "usize", "lo..hi", "1", "5"),
        ("reversed", "usize", "lo..hi", "3", "2"),
        ("negative", "int", "lo..hi", "-1", "2"),
        ("inclusive_end", "usize", "lo..=hi", "0", "4"),
    ] {
        let source = format!(
            "import std.core.slice as slice\n\nfunc cut(s: []u8, lo: {ty}, hi: {ty}): usize {{\n    let t: []u8 = s[{range}]\n    return slice.len<u8>(t)\n}}\n\nfunc main(): int {{\n    let a: [4]u8 = [1 as u8, 2 as u8, 3 as u8, 4 as u8]\n    return cut(a[0..4], {lo}, {hi}) as int\n}}\n"
        );
        // A valid cut would exit with its length, at most 4.
        for code in run_everywhere(name, &source) {
            assert!(
                code.is_none_or(|code| code > 4),
                "{name}: must trap, got {code:?}"
            );
        }
    }
}
