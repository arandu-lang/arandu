//! Regression: the C emitter declares only locals the function body names.
//!
//! Storage markers and block parameters used to declare their locals, leaving
//! `-Wunused-variable` noise in every loop with a scalarized induction variable.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

const SOURCE: &str = r#"
import std.core.slice as slice

func matchAt(haystack: []u8, pos: usize, needle: []u8): bool {
    let nlen = slice.len<u8>(needle)
    let hlen = slice.len<u8>(haystack)
    if pos + nlen > hlen {
        return false
    }
    let mut k: usize = 0
    while k < nlen {
        if haystack[pos + k] != needle[k] {
            return false
        }
        k = k + 1
    }
    return true
}
"#;

/// Name of a `    <type> lN;` declaration, if `line` is one.
fn declared_local(line: &str) -> Option<&str> {
    let name = line
        .strip_prefix("    ")?
        .strip_suffix(';')?
        .rsplit(' ')
        .next()?;
    let digits = name.strip_prefix('l')?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(name)
}

fn mentions(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + name.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[test]
fn emitted_c_declares_only_referenced_locals() {
    let file = std::env::temp_dir().join("arandu_cli_emit_c_locals.aru");
    fs::write(&file, SOURCE).expect("write");
    for profile in [None, Some("--release")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arandu_cli"));
        command.arg("emit-c");
        command.args(profile);
        let output = command.arg(&file).output().expect("cli should run");
        assert!(
            output.status.success(),
            "emit-c {profile:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let c = String::from_utf8(output.stdout).expect("utf-8 output");
        let mut offset = 0;
        let start = c
            .lines()
            .find_map(|line| {
                let found =
                    (line.starts_with("bool matchAt(") && line.ends_with(") {")).then_some(offset);
                offset += line.len() + 1;
                found
            })
            .expect("function definition");
        let body = &c[start..start + c[start..].find("\n}").expect("closing brace")];
        let lines: Vec<&str> = body.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let Some(name) = declared_local(line) else {
                continue;
            };
            assert!(
                lines
                    .iter()
                    .enumerate()
                    .any(|(other, line)| other != index && mentions(line, name)),
                "{profile:?}: `{name}` is declared but never used:\n{body}"
            );
        }
    }
}
