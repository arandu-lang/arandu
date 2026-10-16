//! Cross-check freestanding fixed-point arithmetic against independent i128 math.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
#[path = "../../../tests/support/fixed_oracle.rs"]
mod fixed_oracle;

use std::{fs, process::Command};

#[test]
fn fixed_formats_match_wide_oracle_in_jit_and_c() {
    let directory = common::temp_dir("arandu-fixed-oracle").expect("isolated directory");
    let source = directory.join("fixed_oracle.aru");
    fs::write(&source, fixed_oracle::source()).unwrap();
    for optimization in [None, Some("--opt")] {
        let mut command = common::cli_command();
        command.arg("run").arg(&source);
        if let Some(optimization) = optimization {
            command.arg(optimization);
        }
        let execution = command.output().unwrap();
        assert!(
            execution.status.success(),
            "JIT {optimization:?}: {}",
            String::from_utf8_lossy(&execution.stderr)
        );
    }

    let compiler = std::env::var("CC").unwrap_or_else(|_| "clang".into());
    if Command::new(&compiler)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        let emitted = common::cli_command()
            .arg("emit-c")
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            emitted.status.success(),
            "{}",
            String::from_utf8_lossy(&emitted.stderr)
        );
        let c_source = directory.join("fixed_oracle.c");
        fs::write(&c_source, &emitted.stdout).unwrap();
        for optimization in ["-O0", "-O2", "-Os"] {
            let executable = directory.join(format!("fixed{optimization}.exe"));
            let mut command = Command::new(&compiler);
            command.args([optimization, "-fwrapv"]);
            if cfg!(unix) {
                command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
            }
            let compilation = command
                .arg(&c_source)
                .arg("-o")
                .arg(&executable)
                .arg("-lm")
                .output()
                .unwrap();
            assert!(
                compilation.status.success(),
                "C {optimization}: {}",
                String::from_utf8_lossy(&compilation.stderr)
            );
            let execution = Command::new(&executable).output().unwrap();
            assert!(
                execution.status.success(),
                "C {optimization}: {}",
                String::from_utf8_lossy(&execution.stderr)
            );
        }
    }
    fs::remove_dir_all(directory).unwrap();
}
