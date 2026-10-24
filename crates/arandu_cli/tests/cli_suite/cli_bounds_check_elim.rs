//! Bounds-check elimination keeps only checks a dominating guard does not
//! discharge, and never trusts wrapping arithmetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

const SOURCE: &str = r#"
import std.core.slice as slice

func guarded(s: []u8, i: usize): u8 {
    if i < slice.len<u8>(s) {
        return s[i]
    }
    return 0
}

func otherSlice(a: []u8, b: []u8, i: usize): u8 {
    if i < slice.len<u8>(a) {
        return b[i]
    }
    return 0
}

func afterJoin(s: []u8, i: usize): u8 {
    let mut x: u8 = 0
    if i < slice.len<u8>(s) {
        x = 1
    } else {
        x = 2
    }
    return s[i] + x
}

func wrapped(hay: []u8, pos: usize, needle: []u8): u8 {
    let n = slice.len<u8>(needle)
    if pos + n > slice.len<u8>(hay) {
        return 0
    }
    let mut k: usize = 0
    let mut acc: u8 = 0
    while k < n {
        acc = acc + hay[pos + k] + needle[k]
        k = k + 1
    }
    return acc
}

func main(): int {
    let hayArray: [3]u8 = [1 as u8, 2 as u8, 3 as u8]
    let needleArray: [2]u8 = [1 as u8, 2 as u8]
    let hay: []u8 = hayArray[0..3]
    let needle: []u8 = needleArray[0..2]
    let big: usize = 18446744073709551615
    return wrapped(hay, big, needle) as int
}
"#;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

/// `(function, proven)` for every slice index access in the optimized AMIR.
fn index_accesses(amir: &str) -> Vec<(String, bool)> {
    let mut function = String::new();
    let mut accesses = Vec::new();
    for line in amir.lines() {
        if let Some(rest) = line.strip_prefix("Func ") {
            function = rest.split('(').next().unwrap_or_default().to_owned();
        } else if let Some((_, rhs)) = line.split_once(" = ")
            && rhs.starts_with('_')
            && rhs.contains("[_")
        {
            accesses.push((function.clone(), rhs.ends_with("/* in bounds */")));
        }
    }
    accesses
}

#[test]
fn only_dominating_unsigned_guards_on_the_same_slice_prove_an_index() {
    let file = std::env::temp_dir().join("arandu_cli_bounds_check_elim.aru");
    fs::write(&file, SOURCE).expect("write");
    let path = file.to_string_lossy();

    let output = cli(&["amir", "--release", &path]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let accesses = index_accesses(&String::from_utf8_lossy(&output.stdout));
    let proven = |function: &str| -> Vec<bool> {
        accesses
            .iter()
            .filter(|(name, _)| name == function)
            .map(|(_, proven)| *proven)
            .collect()
    };
    assert_eq!(proven("guarded"), [true], "{accesses:?}");
    assert_eq!(proven("otherSlice"), [false], "a guard on another slice");
    assert_eq!(
        proven("afterJoin"),
        [false],
        "the guard no longer dominates"
    );
    // `pos + n` may wrap, so the haystack check stays; `k < n` proves the needle.
    assert_eq!(proven("wrapped"), [false, true], "{accesses:?}");

    // `pos = usize::MAX` wraps `pos + n` below the length: the kept check traps.
    for profile in [None, Some("--opt")] {
        let mut args = vec!["run"];
        args.extend(profile);
        args.push(&path);
        let run = cli(&args);
        assert!(
            !run.status.success(),
            "{profile:?}: out-of-bounds index must trap"
        );
    }
}
