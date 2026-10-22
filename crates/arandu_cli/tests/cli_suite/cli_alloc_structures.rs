//! Integration tests for Rc, Arc, Slab, and VecDeque across CLI compilation modes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{fs, process::Command};

const CONTRACT: &str = include_str!("../../../../tests/corpus/stdlib_alloc_structures.aru");

#[test]
fn alloc_structures_rc_arc_slab_and_deque() {
    run_contract(CONTRACT, None);
}

#[test]
fn rc_arc_unwrap_and_last_drop_do_not_resurrect_payloads_through_weak() {
    run_contract(
        include_str!("../../../../tests/corpus/rc_lifecycle.aru"),
        Some("11\n22\n33\n44\n55\n66\n"),
    );
}

#[test]
#[cfg(target_pointer_width = "64")]
fn deque_rejects_capacity_that_cannot_be_rounded_without_overflow() {
    let root = common::temp_dir("arandu-deque-capacity-overflow").expect("temporary directory");
    let source = root.join("deque_capacity_overflow.aru");
    fs::write(
        &source,
        r#"
import std.alloc.deque as deque

func main(): int {
    let _q = deque.VecDeque<u8>.withCapacity((18446744073709551615 as u64) as usize)
    return 0
}
"#,
    )
    .expect("write overflow contract");

    let output = common::cli_command()
        .arg("run")
        .arg(&source)
        .output()
        .expect("run CLI");
    assert!(
        !output.status.success(),
        "unrepresentable deque capacity must abort instead of wrapping or hanging"
    );
}

fn run_contract(contract: &str, expected_stdout: Option<&str>) {
    let root = common::temp_dir("arandu-alloc-contract").expect("temporary directory");
    let source = root.join("alloc_structures.aru");
    fs::write(&source, contract).expect("write contract");

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
        if mode == "run"
            && let Some(expected) = expected_stdout
        {
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
                expected
            );
        }
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
                if let Some(expected) = expected_stdout {
                    assert_eq!(
                        String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n"),
                        expected
                    );
                }
            }
        }
    }
}
