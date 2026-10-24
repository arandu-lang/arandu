//! Regression: inspection commands print the AMIR the matching `build` compiles.
//!
//! `emit-c --release` used to ignore the profile and print baseline AMIR, and
//! `emit-llvm` printed baseline AMIR although the LLVM product is always O2.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

const SOURCE: &str = r#"
func up(raw: u32): u32 {
    return raw & (3 as u32)
}

func neutral(raw: u32): u32 {
    return raw | (5 as u32)
}

@Export("process_inputs")
func process_inputs(raw: u32, socd_mode: u32): u32 {
    if socd_mode == (1 as u32) {
        return up(raw)
    }
    return neutral(raw)
}

func main() {
    let _ = process_inputs(7 as u32, 1 as u32)
}
"#;

fn emit(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run");
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 output")
}

/// Body of the exported function, from its definition to the closing brace.
fn body<'a>(text: &'a str, header: &str) -> &'a str {
    let start = text
        .lines()
        .position(|line| line.contains(header) && line.trim_end().ends_with('{'))
        .expect("function definition");
    let offset: usize = text.lines().take(start).map(|line| line.len() + 1).sum();
    let rest = &text[offset..];
    &rest[..rest.find("\n}").expect("closing brace")]
}

#[test]
fn inspection_commands_follow_the_build_opt_level() {
    let file = std::env::temp_dir().join("arandu_cli_inspection_opt_level.aru");
    fs::write(&file, SOURCE).expect("write");
    let path = file.to_string_lossy();

    let baseline = emit(&["emit-c", &path]);
    assert!(
        body(&baseline, "process_inputs(uint32_t").contains("ar__up("),
        "plain emit-c must keep the baseline AMIR of a plain build"
    );

    let release = emit(&["emit-c", "--release", &path]);
    let release = body(&release, "process_inputs(uint32_t");
    assert!(
        !release.contains("ar__up(") && !release.contains("ar__neutral("),
        "emit-c --release must print O2 AMIR:\n{release}"
    );
    let lines: Vec<&str> = release.lines().map(str::trim).collect();
    assert!(
        !lines
            .windows(2)
            .any(|pair| pair[0].ends_with(':') && pair[1].starts_with("goto ")),
        "optimized C must not keep goto-only trampolines:\n{release}"
    );

    let llvm = emit(&["emit-llvm", &path]);
    let llvm = body(&llvm, "@process_inputs(");
    assert!(
        !llvm.contains(" call "),
        "emit-llvm must print the always-O2 AMIR of the LLVM product:\n{llvm}"
    );
}
