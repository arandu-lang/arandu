//! Scalar mathematics exercised through the public CLI across backends.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{fs, process::Command};

#[path = "../../../../tests/support/math_oracle.rs"]
mod math_oracle;

const CONTRACT: &str = include_str!("../../../../tests/corpus/stdlib_math.aru");

#[test]
fn scalar_math_constants_transcendentals_and_inherent_methods() {
    run_contract(CONTRACT);
}

#[test]
fn scalar_math_matches_f64_oracle_at_integer_and_ieee_boundaries() {
    run_contract(&math_oracle::source());
}

fn run_contract(source_text: &str) {
    let root = common::temp_dir("arandu-math-contract").expect("temporary directory");
    let source = root.join("math_contract.aru");
    fs::write(&source, source_text).expect("write contract");

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
            let c = root.join("math.c");
            fs::write(&c, &output.stdout).expect("write C");
            for optimization in ["-O0", "-O2"] {
                let executable = root.join(if cfg!(windows) { "math.exe" } else { "math" });
                let mut command = Command::new("clang");
                command.args([optimization, "-fwrapv"]);
                if cfg!(unix) {
                    command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
                }
                let compile = command
                    .arg(&c)
                    .arg("-o")
                    .arg(&executable)
                    .arg("-lm")
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
                    "compiled math contract failed with exit code {:?}\n{}",
                    run.status.code(),
                    String::from_utf8_lossy(&run.stderr)
                );
            }
        }
    }
}
