//! Decimal limits and rounding exercised through the public CLI and generated C.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{fs, process::Command};

#[path = "../../../../tests/support/decimal_oracle.rs"]
mod decimal_oracle;

const CONTRACT: &str = include_str!("../../../../tests/corpus/stdlib_decimal.aru");

#[test]
fn decimal_limits_rounding_and_invalid_input() {
    let root = common::temp_dir("arandu-decimal-contract").expect("temporary directory");
    let source = root.join("decimal.aru");
    fs::write(&source, CONTRACT).expect("write contract");
    for mode in ["run", "emit-c", "emit-wasm"] {
        let output = common::cli_command()
            .arg(mode)
            .arg(&source)
            .output()
            .expect("run CLI");
        assert!(
            output.status.success(),
            "{mode}: {:?}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "emit-c"
            && Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c = root.join("decimal.c");
            fs::write(&c, &output.stdout).expect("write C");
            for optimization in ["-O0", "-O2"] {
                let executable = root.join(if cfg!(windows) {
                    "decimal.exe"
                } else {
                    "decimal"
                });
                let mut command = Command::new("clang");
                command.args([optimization, "-fwrapv"]);
                if cfg!(unix) {
                    command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
                }
                let compile = command
                    .arg(&c)
                    .arg("-o")
                    .arg(&executable)
                    .output()
                    .expect("compile C");
                assert!(
                    compile.status.success(),
                    "{}",
                    String::from_utf8_lossy(&compile.stderr)
                );
                let result = Command::new(executable).output().expect("execute C");
                assert!(
                    result.status.success(),
                    "{optimization}: {:?}\n{}",
                    result.status.code(),
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }
        if mode == "emit-wasm" {
            assert!(arandu_backend_wasm::is_core_module(&output.stdout));
        }
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn decimal_arithmetic_matches_independent_integer_oracle() {
    let root = common::temp_dir("arandu-decimal-oracle").expect("temporary directory");
    let path = root.join("oracle.aru");
    fs::write(&path, decimal_oracle::source()).expect("write corpus");
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .expect("execute corpus");
    assert!(
        output.status.success(),
        "run: {:?}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let emit_c = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .expect("emit C corpus");
    assert!(
        emit_c.status.success(),
        "emit-c: {:?}\n{}",
        emit_c.status.code(),
        String::from_utf8_lossy(&emit_c.stderr)
    );

    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|version| version.status.success())
    {
        let c = root.join("oracle.c");
        fs::write(&c, &emit_c.stdout).expect("write C");
        for optimization in ["-O0", "-O2"] {
            let executable = root.join(if cfg!(windows) {
                "oracle.exe"
            } else {
                "oracle"
            });
            let mut command = Command::new("clang");
            command.args([optimization, "-fwrapv"]);
            if cfg!(unix) {
                command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
            }
            let compile = command
                .arg(&c)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("compile C");
            assert!(
                compile.status.success(),
                "{}",
                String::from_utf8_lossy(&compile.stderr)
            );
            let result = Command::new(executable).output().expect("execute C");
            assert!(
                result.status.success(),
                "{optimization}: {:?}\n{}",
                result.status.code(),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    let emit_wasm = common::cli_command()
        .arg("emit-wasm")
        .arg(&path)
        .output()
        .expect("emit wasm corpus");
    assert!(
        emit_wasm.status.success(),
        "emit-wasm: {:?}\n{}",
        emit_wasm.status.code(),
        String::from_utf8_lossy(&emit_wasm.stderr)
    );
    assert!(arandu_backend_wasm::is_core_module(&emit_wasm.stdout));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn decimal_storage_cannot_bypass_scale_validation() {
    let root = common::temp_dir("arandu-decimal-private").expect("temporary directory");
    let source = root.join("forged.aru");
    fs::write(&source, "import std.core.decimal as dec\nfunc main(): int { let invalid = dec.Decimal { lo: 1 as u32, mid: 0 as u32, hi: 0 as u32, flags: 16711680 as u32 }; return 0 }\n").expect("write source");
    let output = common::cli_command()
        .arg("check")
        .arg(&source)
        .output()
        .expect("check forged value");
    assert!(
        !output.status.success(),
        "private Decimal storage must not be forgeable"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("private"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}
