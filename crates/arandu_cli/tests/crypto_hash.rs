//! SHA-256/BLAKE3 independent vectors at padding, block and tree boundaries.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::{fs, process::Command};

#[test]
fn hashes_match_reference_vectors_in_jit_and_generated_c() {
    let root = common::temp_dir("arandu-hash-vectors").unwrap();
    let source = root.join("hash.aru");
    fs::write(
        &source,
        include_str!("../../../tests/corpus/stdlib_hash.aru"),
    )
    .unwrap();
    for mode in ["run", "emit-c"] {
        let output = common::cli_command()
            .arg(mode)
            .arg(&source)
            .output()
            .unwrap();
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
                .is_ok_and(|output| output.status.success())
        {
            let path = root.join("hash.c");
            let executable = root.join("hash.exe");
            fs::write(&path, output.stdout).unwrap();
            let mut command = Command::new("clang");
            command.args(["-O2", "-fwrapv"]);
            if cfg!(unix) {
                command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
            }
            let compiled = command
                .arg(path)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let result = Command::new(executable).output().unwrap();
            assert!(
                result.status.success(),
                "{:?}\n{}",
                result.status.code(),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}
