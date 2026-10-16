//! Integration tests for Rc, Arc, Slab, and VecDeque across CLI compilation modes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{fs, process::Command};

const CONTRACT: &str = include_str!("../../../../tests/corpus/stdlib_alloc_structures.aru");

#[test]
fn alloc_structures_rc_arc_slab_and_deque() {
    let root = common::temp_dir("arandu-alloc-contract").expect("temporary directory");
    let source = root.join("alloc_structures.aru");
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
            let c = root.join("alloc_structures.c");
            fs::write(&c, &output.stdout).expect("write C");
            for optimization in ["-O0", "-O2"] {
                let executable = root.join(if cfg!(windows) {
                    "alloc_structures.exe"
                } else {
                    "alloc_structures"
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
                    "compiled alloc contract failed with exit code {:?}\n{}",
                    run.status.code(),
                    String::from_utf8_lossy(&run.stderr)
                );
            }
        }
    }
}
