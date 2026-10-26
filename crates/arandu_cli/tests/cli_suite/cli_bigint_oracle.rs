//! Differential testing for BigInt and BigDecimal against num-bigint oracle.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{fs, process::Command};

#[path = "../../../../tests/support/bigint_oracle.rs"]
mod bigint_oracle;

#[test]
fn bigint_and_bigdecimal_match_independent_num_bigint_oracle() {
    let root = common::temp_dir("arandu-bigint-oracle").expect("temporary directory");
    let path = root.join("oracle.aru");
    fs::write(&path, bigint_oracle::source()).expect("write corpus");

    // 1. Cranelift JIT
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .expect("execute corpus via JIT");
    assert!(
        output.status.success(),
        "run (JIT) failed: {:?}\nstderr: {}\nstdout: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    // 2. C Backend
    let emit_c = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .expect("emit C corpus");
    assert!(
        emit_c.status.success(),
        "emit-c failed: {:?}\n{}",
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
                "clang {optimization} compile failed: {}",
                String::from_utf8_lossy(&compile.stderr)
            );
            let result = Command::new(executable)
                .output()
                .expect("execute compiled C");
            assert!(
                result.status.success(),
                "clang {optimization} execution failed: {:?}\nstderr: {}\nstdout: {}",
                result.status.code(),
                String::from_utf8_lossy(&result.stderr),
                String::from_utf8_lossy(&result.stdout),
            );
        }
    }

    // 3. Wasm Backend
    let emit_wasm = common::cli_command()
        .arg("emit-wasm")
        .arg(&path)
        .output()
        .expect("emit wasm corpus");
    assert!(
        emit_wasm.status.success(),
        "emit-wasm failed: {:?}\n{}",
        emit_wasm.status.code(),
        String::from_utf8_lossy(&emit_wasm.stderr)
    );
    // Execute the same oracle in Wasmtime: `main` returns the 1-based index
    // of the first failing case, so any mismatch is reported by number.
    crate::wasm_host::run_main(&emit_wasm.stdout).assert_returns(0, "BigInt oracle in Wasmtime");

    let _ = fs::remove_dir_all(root);
}
