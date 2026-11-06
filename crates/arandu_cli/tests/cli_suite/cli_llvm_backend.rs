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
        // Single-file examples declare their repository path; as a package
        // entry the module is the package itself (N020).
        let source = source
            .lines()
            .filter(|line| !line.starts_with("module "))
            .collect::<Vec<_>>()
            .join("\n");
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
module export_symbol

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
module freestanding_export

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
module freestanding_export

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

/// Compiles `source` through `emit-llvm` and clang, runs it and returns its
/// exit status.
fn llvm_native_status(name: &str, source: &str) -> std::process::ExitStatus {
    let dir = std::env::temp_dir().join(format!("arandu_cli_llvm_{name}"));
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("main.aru"), source).expect("write");
    let ir = run_cli_in(&dir, &["emit-llvm", "main.aru"]);
    assert!(
        ir.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&ir.stderr)
    );
    let ll = dir.join("main.ll");
    std::fs::write(&ll, &ir.stdout).expect("write ll");
    let binary = dir.join("main.bin");
    let clang = Command::new("clang")
        .args(["-O2", "-w"])
        .arg(&ll)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("clang");
    assert!(
        clang.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&clang.stderr)
    );
    Command::new(&binary).output().expect("run").status
}

/// Arrays and slices trap on an out-of-bounds index in the LLVM product too,
/// matching the C, Cranelift and Wasm backends, whether the index reads a
/// value, a place through `ref []T`, or bounds a subslice.
#[test]
fn llvm_product_traps_on_an_out_of_bounds_index() {
    if !clang_available() {
        return;
    }
    let array = "func at(values: [3]u8, i: usize): u8 {\n    return values[i]\n}\n\nfunc main(): int {\n    let values: [3]u8 = [1 as u8, 2 as u8, 3 as u8]\n    return at(values, INDEX as usize) as int\n}\n";
    let borrowed = "func at(s: ref []u8, i: usize): u8 {\n    return s[i]\n}\n\nfunc main(): int {\n    let values: [4]u8 = [10 as u8, 20 as u8, 30 as u8, 40 as u8]\n    let s: []u8 = values[0..4]\n    return at(ref s, INDEX) as int\n}\n";
    let subslice = "import std.core.slice as slice\n\nfunc cut(s: []u8, end: usize): usize {\n    let t: []u8 = s[1..end]\n    return slice.len<u8>(t)\n}\n\nfunc main(): int {\n    let values: [4]u8 = [1 as u8, 2 as u8, 3 as u8, 4 as u8]\n    return cut(values[0..4], INDEX) as int\n}\n";
    for (name, source, inside, expected) in [
        ("array", array, "2", 3),
        ("borrowed", borrowed, "2", 30),
        ("subslice", subslice, "4", 3),
    ] {
        let status = llvm_native_status(name, &source.replace("INDEX", inside));
        assert_eq!(status.code(), Some(expected), "{name} in bounds");
        let status = llvm_native_status(&format!("{name}_oob"), &source.replace("INDEX", "9"));
        assert!(
            !status.success(),
            "{name}: index 9 must trap, got {status:?}"
        );
    }
}

/// Values reach their destination without identity copies: SSA temps alias
/// the computed value and stack-slot temps store it directly. LLVM textual IR
/// has no `%a = %b`, so the old fallback was `select i1 true, x, x`.
#[test]
fn emit_llvm_names_values_without_identity_selects() {
    let root = workspace_root();
    let mut examples: Vec<_> = std::fs::read_dir(root.join("examples/minimal"))
        .expect("minimal examples")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "aru"))
        .collect();
    examples.sort();
    let mut emitted = 0;
    for example in &examples {
        let output = run_cli_in(&root, &["emit-llvm", &example.to_string_lossy()]);
        if !output.status.success() {
            continue;
        }
        emitted += 1;
        let ir = String::from_utf8_lossy(&output.stdout);
        assert!(
            !ir.contains("select i1 true,"),
            "{} still copies a value through `select i1 true`",
            example.display()
        );
    }
    assert!(
        emitted >= 20,
        "only {emitted} minimal examples emitted LLVM IR"
    );
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

/// C has no sum types, so an exported `Result<T, E>` becomes a status code
/// with the payload written through a trailing out-parameter. The header and
/// the object must agree, and a real C consumer must see `0` for `Ok`.
#[test]
fn exported_result_returns_a_c_status_code_and_an_out_parameter() {
    if !clang_available() {
        return;
    }
    let tmp = common::temp_dir("arandu_llvm_export_status").unwrap();
    let project = tmp.join("status");
    let init = run_cli_in(&tmp, &["new", "status", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new status: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
@Export
func read(v: u32): Result<u32, u32> {
    if v == 0 as u32 {
        return Result.Err(7 as u32)
    }
    return Result.Ok(v + 1 as u32)
}
"#,
    )
    .expect("write lib.aru");

    let header = tmp.join("abi.h");
    let emit_header = run_cli_in(
        &project,
        &["build", "--emit=header", "-o", header.to_str().unwrap()],
    );
    assert!(
        emit_header.status.success(),
        "emit header: {}",
        String::from_utf8_lossy(&emit_header.stderr)
    );
    let header_text = fs::read_to_string(&header).expect("read header");
    assert!(
        header_text.contains("uint32_t read(uint32_t, uint32_t *);"),
        "a Result crosses as a status plus an out-parameter:\n{header_text}"
    );
    assert!(
        header_text.contains("0 on success"),
        "the header must state the status convention:\n{header_text}"
    );

    let object = tmp.join("lib.o");
    let emit_object = run_cli_in(
        &project,
        &[
            "build",
            "-b",
            "llvm",
            "--emit=obj",
            "-o",
            object.to_str().unwrap(),
        ],
    );
    assert!(
        emit_object.status.success(),
        "emit object: {}",
        String::from_utf8_lossy(&emit_object.stderr)
    );

    let consumer = tmp.join("consumer.c");
    fs::write(
        &consumer,
        r#"
#include "abi.h"
#include <stdio.h>

int main(void) {
    uint32_t out = 0;
    if (read(0, &out) != 7) { return 1; }
    if (read(5, &out) != 0) { return 2; }
    if (out != 6) { return 3; }
    puts("STATUS OK");
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
        "C consumer disagreed with the status ABI: {}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "STATUS OK");
}

/// A status crosses to C, so its width must not depend on the target the
/// object ends up being linked for. `Result<void, E>` needs no out-parameter:
/// the status is the whole result.
#[test]
fn exported_result_status_is_fixed_width_and_void_needs_no_out_parameter() {
    let tmp = common::temp_dir("arandu_llvm_export_status_shape").unwrap();
    let project = tmp.join("status_shape");
    let init = run_cli_in(&tmp, &["new", "status_shape", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new status_shape: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
@Export
func ping(): Result<void, u32> {
    return nil
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("status_shape.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    assert!(
        build.status.success(),
        "void result header: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let text = fs::read_to_string(&header).expect("read header");
    assert!(text.contains("uint32_t ping(void);"), "{text}");
    assert!(text.contains("0 on success"), "{text}");

    fs::write(
        project.join("src/lib.aru"),
        r#"
@Export
func bad(v: u32): Result<u32, usize> {
    return Result.Err(7)
}
"#,
    )
    .expect("write target-dependent status");
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(!build.status.success(), "must reject: {stderr}");
    assert!(stderr.contains("T049"), "{stderr}");
    assert!(stderr.contains("fixed width"), "{stderr}");
}

/// The rewrite changes the signature at the boundary, so an in-language call
/// would silently observe the status instead of the sum type. It is rejected
/// rather than emitting a call that disagrees with its own definition.
#[test]
fn exported_result_rejects_an_in_language_caller() {
    let tmp = common::temp_dir("arandu_llvm_export_status_caller").unwrap();
    let project = tmp.join("status_caller");
    let init = run_cli_in(&tmp, &["new", "status_caller", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new status_caller: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
@Export
func read(v: u32): Result<u32, u32> {
    return Result.Ok(v)
}

public func readTwice(v: u32): u32 {
    match read(v) {
        Result.Ok(value) => value
        Result.Err(code) => code
    }
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("status_caller.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(!build.status.success(), "must reject: {stderr}");
    assert!(stderr.contains("T049"), "{stderr}");
    assert!(stderr.contains("still calls"), "{stderr}");
    assert!(stderr.contains("readTwice"), "{stderr}");
    assert!(!header.exists(), "no header may be published");
}

/// Release is the one method allowed across the boundary, and it stays `void`:
/// the `Result` status contract does not extend to it, so a destructor that
/// tries to report failure through a status is rejected by its own shape rule.
#[test]
fn exported_destructor_rejects_a_result_return() {
    let tmp = common::temp_dir("arandu_llvm_export_destructor_status").unwrap();
    let project = tmp.join("destructor_status");
    let init = run_cli_in(&tmp, &["new", "destructor_status", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new destructor_status: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
public struct Buffer {
    len: usize
}

@Destructor
@Export("buffer_release")
func Buffer.close(own self): Result<void, u32> {
    return nil
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("destructor_status.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(!build.status.success(), "must reject: {stderr}");
    assert!(stderr.contains("T035"), "{stderr}");
    assert!(stderr.contains("void"), "{stderr}");
    assert!(!header.exists(), "no header may be published");
}

/// The header is a description of the boundary, not of a backend: it must be
/// producible without `-b llvm`, and it must describe the *requested* target
/// rather than the host the compiler happens to run on.
#[test]
fn header_describes_the_boundary_without_a_backend() {
    let tmp = common::temp_dir("arandu_llvm_export_header").unwrap();
    let project = tmp.join("hdr");
    let init = run_cli_in(&tmp, &["new", "hdr", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new hdr: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
@Repr("C")
struct Packet {
    tag: int
    count: usize
    name: str
    payload: [8]u8
}

@Export
func read_packet(buf: []u8): Packet {
    return Packet { tag: 1, count: 0, name: "x", payload: [0, 0, 0, 0, 0, 0, 0, 0] }
}

@Export
func fill(buf: mut []u8): void {
}

@Export
func label(text: str): int {
    return 0
}
"#,
    )
    .expect("write library source");

    let host_header = tmp.join("host.h");
    let host_arg = host_header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &host_arg]);
    assert!(
        build.status.success(),
        "header without a backend: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let host = fs::read_to_string(&host_header).expect("read host header");

    assert!(
        host.contains("_Static_assert(sizeof(void *) == 8"),
        "{host}"
    );
    // A guard derived from the export set, so two headers can share a TU.
    assert!(host.contains("#ifndef ARANDU_EXPORT_"), "{host}");
    assert!(host.contains("_H\n"), "{host}");
    assert!(!host.contains("ARANDU_EXPORT_HEADER"), "{host}");
    // `[]u8` and `mut []u8` are different contracts and must not collapse.
    assert!(
        host.contains("const uint8_t *ptr; size_t len; } AranduSliceU8;"),
        "{host}"
    );
    assert!(
        host.contains("uint8_t *ptr; size_t len; } AranduMutSliceU8;"),
        "{host}"
    );
    // Text is nominal: it says UTF-8, it is not a byte slice in disguise.
    assert!(host.contains("AranduStr"), "{host}");
    // C array fields keep their declarator syntax, and layouts are asserted.
    assert!(host.contains("uint8_t payload[8];"), "{host}");
    assert!(host.contains("_Alignof(Packet) == 8"), "{host}");
    assert!(host.contains("int32_t tag;"), "{host}");
    assert!(host.contains("size_t count;"), "{host}");

    let m0_header = tmp.join("m0.h");
    let m0_arg = m0_header.to_string_lossy().into_owned();
    let build = run_cli_in(
        &project,
        &[
            "build",
            "--emit=header",
            "--target=thumbv6m-none-eabi",
            "-o",
            &m0_arg,
        ],
    );
    assert!(
        build.status.success(),
        "32-bit header: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let m0 = fs::read_to_string(&m0_header).expect("read cortex-m0 header");
    assert!(m0.contains("_Static_assert(sizeof(void *) == 4"), "{m0}");
    assert!(m0.contains("_Alignof(Packet) == 4"), "{m0}");
    assert!(m0.contains("sizeof(Packet) == 24"), "{m0}");
}

/// The boundary owns a small C namespace: `AranduStr` and the slice typedefs.
/// An Arandu type with that name would redeclare a C type, so it is rejected
/// at header time instead of producing a header clang refuses to compile.
#[test]
fn header_rejects_types_that_collide_with_generated_typedefs() {
    let tmp = common::temp_dir("arandu_llvm_export_reserved").unwrap();
    let project = tmp.join("reserved");
    let init = run_cli_in(&tmp, &["new", "reserved", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new reserved: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
@Repr("C")
struct AranduStr {
    len: usize
}

@Export
func take(value: AranduStr): u32 {
    return 0
}

@Export
func text(value: str): u32 {
    return 0
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("reserved.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(!build.status.success(), "must reject: {stderr}");
    assert!(stderr.contains("T049"), "{stderr}");
    assert!(stderr.contains("AranduStr"), "{stderr}");
    assert!(!header.exists(), "no header may be published");
}

/// A pointer is an opaque handle: C needs the name, never the layout. A type
/// that holds runtime-only fields (here a `Vec`) cannot cross by value, but it
/// must still be exportable behind `ptr`, and its fields must not leak.
#[test]
fn header_forward_declares_opaque_handles() {
    let tmp = common::temp_dir("arandu_llvm_export_opaque").unwrap();
    let project = tmp.join("opaque");
    let init = run_cli_in(&tmp, &["new", "opaque", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new opaque: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
import std.alloc.vec as vec

public struct Buffer {
    data: vec.Vec<u8>
    len: usize
}

@Export
func buffer_len(handle: ptr[Buffer]): usize {
    return 0
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("opaque.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    assert!(
        build.status.success(),
        "opaque header: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let text = fs::read_to_string(&header).expect("read header");
    assert!(text.contains("typedef struct Buffer Buffer;"), "{text}");
    assert!(text.contains("size_t buffer_len(Buffer *);"), "{text}");
    assert!(!text.contains("struct Buffer {"), "{text}");
    assert!(!text.contains("Vec"), "{text}");
}

/// Documentation travels with the export contract: the header a C consumer
/// reads should carry the same doc comment the Arandu source has, and a
/// function without docs must not gain an empty block.
#[test]
fn header_carries_export_doc_comments() {
    let tmp = common::temp_dir("arandu_llvm_export_docs").unwrap();
    let project = tmp.join("docs");
    let init = run_cli_in(&tmp, &["new", "docs", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new docs: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
/// Computes the total.
/// Second line.
@Export
func total(raw: u32): u32 {
    return raw
}

@Export
func bare(raw: u32): u32 {
    return raw
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("docs.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    assert!(
        build.status.success(),
        "documented header: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let text = fs::read_to_string(&header).expect("read documented header");
    assert!(
        text.contains(
            "/**\n * Computes the total.\n * Second line.\n */\nuint32_t total(uint32_t);"
        ),
        "{text}"
    );
    assert!(text.contains("\nuint32_t bare(uint32_t);"), "{text}");
    assert!(!text.contains("/**\n */"), "{text}");
}

/// Ownership is a contract, not a comment the user writes: an exported
/// `@Destructor` is the release function, C only ever sees the opaque handle,
/// and the header states which symbol releases it.
#[test]
fn exported_destructor_is_the_release_contract_for_an_opaque_handle() {
    let tmp = common::temp_dir("arandu_llvm_export_ownership").unwrap();
    let project = tmp.join("ownership");
    let init = run_cli_in(&tmp, &["new", "ownership", "--lib", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new ownership: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        project.join("src/lib.aru"),
        r#"
public struct Buffer {
    data: ptr[u8]
    len: usize
}

@Destructor
@Export("buffer_release")
func Buffer.close(own self): void {
    return
}

@Export
func buffer_len(handle: ptr[Buffer]): usize {
    return 0
}
"#,
    )
    .expect("write library source");

    let header = tmp.join("ownership.h");
    let header_arg = header.to_string_lossy().into_owned();
    let build = run_cli_in(&project, &["build", "--emit=header", "-o", &header_arg]);
    assert!(
        build.status.success(),
        "ownership header: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let text = fs::read_to_string(&header).expect("read ownership header");
    assert!(text.contains("typedef struct Buffer Buffer;"), "{text}");
    assert!(text.contains("void buffer_release(Buffer *);"), "{text}");
    assert!(
        text.contains("Release with buffer_release(Buffer *)"),
        "{text}"
    );
    // The handle stays opaque: no fields, no `ptr[u8]` spelling in C.
    assert!(!text.contains("struct Buffer {"), "{text}");

    let object = tmp.join("ownership.o");
    let object_arg = object.to_string_lossy().into_owned();
    let build = run_cli_in(
        &project,
        &["build", "-b", "llvm", "--emit=obj", "-o", &object_arg],
    );
    assert!(
        build.status.success(),
        "ownership object: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let Ok(symbols) = Command::new("llvm-nm")
        .args(["--defined-only", "--extern-only"])
        .arg(&object)
        .output()
    else {
        return;
    };
    let symbols = String::from_utf8_lossy(&symbols.stdout);
    assert!(symbols.contains("T buffer_release"), "{symbols}");
    assert!(symbols.contains("T buffer_len"), "{symbols}");
}
