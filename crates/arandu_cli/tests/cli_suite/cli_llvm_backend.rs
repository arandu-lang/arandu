//! Experimental LLVM backend via `run -b llvm` / `build -b llvm`.
//!
//! Covers string interpolation (m09) and GenArena (m16) against the packaged
//! runtime linked by clang — the same path as production `build -b llvm`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn stdlib_path() -> PathBuf {
    workspace_root().join("stdlib")
}

fn clang_available() -> bool {
    Command::new("clang")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_cli_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let stdlib = stdlib_path();
    let stdlib_arg = stdlib.to_string_lossy();
    let mut full = Vec::with_capacity(args.len() + 2);
    full.extend_from_slice(args);
    full.push("--stdlib-path");
    full.push(stdlib_arg.as_ref());
    common::cli_command()
        .current_dir(dir)
        .args(&full)
        .output()
        .expect("cli should run")
}

#[test]
fn run_b_llvm_interpolation_matches_minimal_gold() {
    if !clang_available() {
        return;
    }
    let root = workspace_root();
    let out = run_cli_in(
        &root,
        &["run", "-b", "llvm", "examples/minimal/m09_interp_tostr.aru"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "m09 run -b llvm: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "n=3",
        "m09 stdout mismatch"
    );
}

#[test]
fn run_b_llvm_gen_arena_matches_minimal_gold() {
    if !clang_available() {
        return;
    }
    let root = workspace_root();
    let out = run_cli_in(
        &root,
        &["run", "-b", "llvm", "examples/minimal/m16_gen_arena.aru"],
    );
    assert_eq!(
        out.status.code(),
        Some(83),
        "m16 run -b llvm: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn build_b_llvm_interpolation_and_gen_arena_binaries() {
    if !clang_available() {
        return;
    }
    let tmp = common::temp_dir("arandu_llvm_cli_parity").unwrap();
    let cases = [
        (
            "llvm_interp",
            include_str!("../../../../examples/minimal/m09_interp_tostr.aru"),
            0i32,
            Some("n=3"),
        ),
        (
            "llvm_gen_arena",
            include_str!("../../../../examples/minimal/m16_gen_arena.aru"),
            83,
            None,
        ),
    ];

    for (name, source, exit, stdout) in cases {
        let project = tmp.join(name);
        let init = run_cli_in(&tmp, &["new", name, "--bin", "--vcs=none"]);
        assert!(
            init.status.success(),
            "new {name}: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        fs::write(project.join("src/main.aru"), source).expect("write main");

        let build = run_cli_in(&project, &["build", "-b", "llvm"]);
        let build_out = String::from_utf8_lossy(&build.stdout);
        let build_err = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "build -b llvm {name}:\nstdout={build_out}\nstderr={build_err}"
        );
        assert!(
            build_out.contains("backend=llvm-release"),
            "expected llvm-release backend label, got: {build_out}"
        );
        let artifact_path = build_out
            .lines()
            .find_map(|line| {
                line.split("artifact=")
                    .nth(1)
                    .map(|value| value.trim().trim_end_matches(')').to_owned())
            })
            .unwrap_or_else(|| panic!("missing artifact= in build output:\n{build_out}"));
        let binary = Path::new(&artifact_path);
        assert!(binary.is_file(), "LLVM artifact missing at {artifact_path}");
        let run = Command::new(binary).output().expect("run built binary");
        assert_eq!(
            run.status.code(),
            Some(exit),
            "built {name} exit: stderr={}",
            String::from_utf8_lossy(&run.stderr)
        );
        if let Some(expected) = stdout {
            assert_eq!(
                String::from_utf8_lossy(&run.stdout).trim(),
                expected,
                "built {name} stdout"
            );
        }
    }
}

#[test]
fn emit_llvm_includes_interp_and_gen_runtime_helpers() {
    let root = workspace_root();
    let interp = run_cli_in(
        &root,
        &["emit-llvm", "examples/minimal/m09_interp_tostr.aru"],
    );
    assert!(
        interp.status.success(),
        "emit-llvm m09: {}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let interp_ir = String::from_utf8_lossy(&interp.stdout);
    assert!(
        interp_ir.contains("ar_str_concat")
            || interp_ir.contains("ar_rt_raw_malloc")
            || interp_ir.contains("ar_jit_"),
        "expected string runtime helpers in m09 IR"
    );

    let arena = run_cli_in(&root, &["emit-llvm", "examples/minimal/m16_gen_arena.aru"]);
    assert!(
        arena.status.success(),
        "emit-llvm m16: {}",
        String::from_utf8_lossy(&arena.stderr)
    );
    let arena_ir = String::from_utf8_lossy(&arena.stdout);
    assert!(
        arena_ir.contains("ar_gen_insert_raw") || arena_ir.contains("ar_gen_get_raw"),
        "expected GenRef runtime helpers in m16 IR"
    );
}
