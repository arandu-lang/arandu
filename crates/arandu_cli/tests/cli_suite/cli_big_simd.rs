//! Integration tests for BigInt, BigDecimal, and SIMD vectors.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{fs, process::Command};

const CONTRACT: &str = include_str!("../../../../tests/corpus/stdlib_big_simd.aru");

#[test]
fn big_numbers_and_simd_vectors() {
    let root = common::temp_dir("arandu-big-simd-contract").expect("temporary directory");
    let source = root.join("big_simd.aru");
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
            let c = root.join("big_simd.c");
            fs::write(&c, &output.stdout).expect("write C");
            for optimization in ["-O0", "-O2"] {
                let executable = root.join(if cfg!(windows) {
                    "big_simd.exe"
                } else {
                    "big_simd"
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
                    "clang {optimization}: {:?}\n{}",
                    compile.status.code(),
                    String::from_utf8_lossy(&compile.stderr)
                );
                let run = Command::new(&executable).output().expect("run compiled C");
                assert_eq!(
                    run.status.code(),
                    Some(0),
                    "compiled big-simd contract failed with exit code {:?}\n{}",
                    run.status.code(),
                    String::from_utf8_lossy(&run.stderr)
                );
            }
        }
    }
}
