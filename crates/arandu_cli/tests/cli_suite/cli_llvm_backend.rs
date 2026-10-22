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
fn explicit_output_outside_the_profile_builds_without_incremental_noise() {
    if !clang_available() {
        return;
    }
    let tmp = common::temp_dir("arandu_llvm_explicit_output").unwrap();
    let project = tmp.join("explicit_output");
    let init = run_cli_in(&tmp, &["new", "explicit_output", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new explicit_output: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/main.aru"),
        "func main(): int { let raw: u32 = 7 as u32 return 0 }\n",
    )
    .expect("write main");

    let object = tmp.join("artifacts").join("explicit.o");
    let object_arg = object.to_string_lossy().into_owned();
    let build = run_cli_in(
        &project,
        &[
            "build",
            "-b",
            "llvm",
            "--target=thumbv6m-none-eabi",
            "--emit=obj",
            "-o",
            &object_arg,
        ],
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build.status.success(),
        "--emit=obj with an explicit -o must succeed: {stderr}"
    );
    assert!(
        !stderr.contains("artifact is outside the active build profile"),
        "an explicit -o is not an incremental-cache failure: {stderr}"
    );
    assert!(
        !stderr.contains("override-module"),
        "the module header must use clang's canonical triple: {stderr}"
    );
    assert!(object.is_file(), "missing object at {}", object.display());

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn export_emits_a_global_c_symbol_in_the_object() {
    if !clang_available() {
        return;
    }
    let nm = Command::new("llvm-nm").arg("--version").output();
    if nm.is_err() || !nm.is_ok_and(|output| output.status.success()) {
        return;
    }
    let tmp = common::temp_dir("arandu_llvm_export").unwrap();
    let project = tmp.join("export_symbol");
    let init = run_cli_in(&tmp, &["new", "export_symbol", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new export_symbol: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/main.aru"),
        r#"
module hitbox.core.socd

@Export
func apply_socd(raw: u32): u32 { return raw }

@Export("arandu_core_process_inputs")
func process_inputs(raw: u32): u32 { return raw }

func helper(raw: u32): u32 { return raw }

func main(): int { return 0 }
"#,
    )
    .expect("write main");

    let object = tmp.join("export.o");
    let object_arg = object.to_string_lossy().into_owned();
    let build = run_cli_in(
        &project,
        &["build", "-b", "llvm", "--emit=obj", "-o", &object_arg],
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(build.status.success(), "build failed: {stderr}");

    let symbols = Command::new("llvm-nm")
        .arg("--defined-only")
        .arg("--extern-only")
        .arg(&object)
        .output()
        .expect("run llvm-nm");
    let symbols = String::from_utf8_lossy(&symbols.stdout);
    let mut globals: Vec<&str> = symbols
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .collect();
    globals.sort_unstable();
    assert_eq!(
        globals,
        ["apply_socd", "arandu_core_process_inputs", "main"],
        "only @Export functions and main may stay global:\n{symbols}"
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn freestanding_export_rejects_host_runtime_dependency() {
    let tmp = common::temp_dir("arandu_llvm_freestanding_export").unwrap();
    let project = tmp.join("freestanding_export");
    let init = run_cli_in(&tmp, &["new", "freestanding_export", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new freestanding_export: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/main.aru"),
        r#"
module hitbox.core.socd

@Export
func decorate(n: i64): i64 {
    let text = "n=${n}"
    return text.len() as i64
}
"#,
    )
    .expect("write main");

    let output = tmp.join("freestanding.ll");
    let output_arg = output.to_string_lossy().into_owned();
    let build = run_cli_in(
        &project,
        &[
            "build",
            "-b",
            "llvm",
            "--target=thumbv6m-none-eabi",
            "--emit=llvm-ir",
            "-o",
            &output_arg,
        ],
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        !build.status.success(),
        "an exported path that needs the host runtime must fail: {stderr}"
    );
    assert!(
        stderr.contains("T049") && stderr.contains("host runtime"),
        "expected T049 naming the host runtime dependency: {stderr}"
    );

    fs::write(
        project.join("src/main.aru"),
        r#"
module hitbox.core.socd

@Export
func apply_socd(raw: u32): u32 { return raw }
"#,
    )
    .expect("write scalar export");
    let build = run_cli_in(
        &project,
        &[
            "build",
            "-b",
            "llvm",
            "--target=thumbv6m-none-eabi",
            "--emit=llvm-ir",
            "-o",
            &output_arg,
        ],
    );
    assert!(
        build.status.success(),
        "a scalar export must stay freestanding: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn incremental_build_does_not_reuse_cranelift_artifact_for_llvm() {
    if !clang_available() {
        return;
    }
    let tmp = common::temp_dir("arandu_llvm_incremental_backend").unwrap();
    let project = tmp.join("llvm_incremental_backend");
    let init = run_cli_in(
        &tmp,
        &["new", "llvm_incremental_backend", "--bin", "--vcs=none"],
    );
    assert!(
        init.status.success(),
        "new project: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let cranelift = run_cli_in(&project, &["build", "--release"]);
    assert!(
        cranelift.status.success(),
        "Cranelift build: {}",
        String::from_utf8_lossy(&cranelift.stderr)
    );

    let llvm = run_cli_in(&project, &["build", "--backend=llvm"]);
    assert!(
        llvm.status.success(),
        "LLVM build: {}",
        String::from_utf8_lossy(&llvm.stderr)
    );
    let llvm_stdout = String::from_utf8_lossy(&llvm.stdout);
    assert!(
        llvm_stdout.contains("backend=llvm-release"),
        "expected LLVM build, got: {llvm_stdout}"
    );
    assert!(
        !llvm_stdout.contains("incremental: up-to-date"),
        "LLVM incorrectly reused Cranelift session: {llvm_stdout}"
    );

    let repeated_llvm = run_cli_in(&project, &["build", "--backend=llvm"]);
    assert!(
        repeated_llvm.status.success(),
        "repeated LLVM build: {}",
        String::from_utf8_lossy(&repeated_llvm.stderr)
    );
    assert!(String::from_utf8_lossy(&repeated_llvm.stdout).contains("incremental: up-to-date"));
}

#[test]
fn emit_llvm_includes_interp_runtime_helpers() {
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
    // `GenArena` is a pure-buffer stdlib type today: it allocates through the
    // allocator helpers, not the legacy GenRef runtime, so no `ar_gen_*`
    // symbol is expected here. GenRef lowering itself is covered by the LLVM
    // backend's direct-AMIR tests.
    assert!(
        arena_ir.contains("define i32 @main"),
        "expected a main definition in m16 IR:\n{arena_ir}"
    );
}

#[test]
fn exported_aggregates_link_and_run_against_a_c_consumer() {
    if !clang_available() {
        return;
    }
    let tmp = common::temp_dir("arandu_llvm_export_abi").unwrap();
    let project = tmp.join("abi");
    let init = run_cli_in(&tmp, &["new", "abi", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new abi: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
import std.core.slice as slice

@Repr("C")
struct Frame {
    id: u32
    weight: f32
}

@Repr("C")
struct Pair {
    a: u64
    b: u64
}

@Export
func frame_id(frame: Frame): u32 { return frame.id }

@Export
func bump(frame: Frame): Frame {
    let moved = Frame { id: frame.id + 1, weight: frame.weight + 0.5 }
    return moved
}

@Export
func swap_pair(pair: Pair): Pair {
    let flipped = Pair { a: pair.b, b: pair.a }
    return flipped
}

@Export
func buffer_len(data: []u8): u64 {
    return slice.len<u8>(data) as u64
}
"#,
    )
    .expect("write library source");

    let object = tmp.join("abi.o");
    let object_arg = object.to_string_lossy().into_owned();
    let build = run_cli_in(
        &project,
        &["build", "-b", "llvm", "--emit=obj", "-o", &object_arg],
    );
    assert!(
        build.status.success(),
        "build abi object: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    // The consumer uses the generated header, so a mismatch between Arandu's
    // layout and the C declaration fails the `_Static_assert`s at compile time.
    let header = tmp.join("abi.h");
    let header_arg = header.to_string_lossy().into_owned();
    let emit_header = run_cli_in(
        &project,
        &["build", "-b", "llvm", "--emit=header", "-o", &header_arg],
    );
    assert!(
        emit_header.status.success(),
        "emit C header: {}",
        String::from_utf8_lossy(&emit_header.stderr)
    );
    let header_text = fs::read_to_string(&header).expect("header should exist");
    assert!(
        header_text.contains("_Static_assert(sizeof(Frame) == 8"),
        "the header must pin the aggregate size:\n{header_text}"
    );
    assert!(
        header_text.contains("AranduSliceU8"),
        "a slice crosses as a {{ptr, len}} typedef:\n{header_text}"
    );

    let consumer = tmp.join("consumer.c");
    fs::write(
        &consumer,
        r#"
#include "abi.h"
#include <stdio.h>

int main(void) {
    Frame frame = { .id = 41, .weight = 1.5f };
    if (frame_id(frame) != 41) { return 1; }
    Frame moved = bump(frame);
    if (moved.id != 42) { return 2; }
    if (moved.weight < 1.99f || moved.weight > 2.01f) { return 3; }
    Pair pair = { .a = 7, .b = 9 };
    Pair flipped = swap_pair(pair);
    if (flipped.a != 9 || flipped.b != 7) { return 4; }
    const uint8_t bytes[] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11 };
    AranduSliceU8 view = { bytes, sizeof bytes };
    if (buffer_len(view) != sizeof bytes) { return 5; }
    puts("ABI OK");
    return 0;
}
"#,
    )
    .expect("write consumer");

    let binary = tmp.join("consumer");
    let link = Command::new("clang")
        .arg("-I")
        .arg(&tmp)
        .arg("-Wall")
        .arg("-Wextra")
        .arg(&consumer)
        .arg(&object)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("clang should run");
    assert!(
        link.status.success(),
        "link C consumer: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&binary).output().expect("run consumer");
    assert_eq!(
        run.status.code(),
        Some(0),
        "C consumer disagreed with the exported ABI: {}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ABI OK");
}
