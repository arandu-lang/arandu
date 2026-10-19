//! Canonical source SIMD execution, checked scalar-aligned slices, and opaque types.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::{fs, process::Command};

#[test]
fn source_vectors_masks_and_checked_unaligned_slices_execute_in_jit_and_c() {
    let root = common::temp_dir("arandu-native-simd-source").unwrap();
    let path = root.join("simd.aru");
    fs::write(
        &path,
        include_str!("../../../tests/corpus/stdlib_native_simd.aru"),
    )
    .unwrap();
    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&path);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "JIT optimized={optimized}, status={:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = root.join("simd.c");
    fs::write(&c, &output.stdout).unwrap();
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        for (optimization, fallback) in [("-O0", false), ("-O2", false), ("-O2", true)] {
            let executable = root.join("simd.exe");
            let mut command = Command::new("clang");
            command.args([optimization, "-fwrapv"]);
            if fallback {
                command.arg("-DARANDU_SCALAR_SIMD");
            }
            if cfg!(unix) {
                command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
            }
            let compile = command.arg(&c).arg("-o").arg(&executable).output().unwrap();
            assert!(
                compile.status.success(),
                "C {optimization} fallback={fallback}: {}",
                String::from_utf8_lossy(&compile.stderr)
            );
            let run = Command::new(&executable).output().unwrap();
            assert!(
                run.status.success(),
                "C {optimization} fallback={fallback}, status={:?}: {}",
                run.status.code(),
                String::from_utf8_lossy(&run.stderr)
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_source_lane_indices_trap_instead_of_aliasing_last_lane() {
    let root = common::temp_dir("arandu-native-simd-lane-bounds").unwrap();
    let path = root.join("bad.aru");
    for expression in [
        "v.extractLane(-1)",
        "v.extractLane(4)",
        "v.replaceLane(-1, 5.0)",
        "v.replaceLane(4, 5.0)",
    ] {
        fs::write(&path, format!("import std.core.simd as simd\nfunc main(): int {{ let v = simd.F32x4.splat(1.0); let bad = {expression}; return 0 }}")).unwrap();
        let output = common::cli_command()
            .arg("run")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {expression}");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert!(
                matches!(
                    output.status.signal(),
                    Some(libc::SIGILL | libc::SIGTRAP | libc::SIGABRT)
                ),
                "expected a deliberate trap, got {:?} for {expression}",
                output.status
            );
        }
        assert!(
            output.stderr.is_empty(),
            "expected execution trap for {expression}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mutable_slice_storage_requires_an_exclusive_owner_borrow() {
    let root = common::temp_dir("arandu-simd-exclusive-view").unwrap();
    let path = root.join("view.aru");
    let prefix = r#"import std.alloc.vec as vec
import std.core.simd as simd
func main(): int {
    let mut data = vec.Vec<f32>.new()
    data.push(1.0 as f32)
    data.push(2.0 as f32)
    data.push(3.0 as f32)
    data.push(4.0 as f32)
"#;
    for (body, expected) in [
        (
            "let immutable = data.asSlice(); let mut copied = immutable; simd.F32x4.splat(9.0).storeUnaligned<vec.Vec<f32>>(mut ref copied)",
            "T003",
        ),
        (
            "let view = data.asMutSlice(); let moved = data; view.len(); moved.len()",
            "O0",
        ),
        (
            "let view = data.asMutSlice(); data.push(5.0 as f32); view.len()",
            "O0",
        ),
        (
            "let view = data.asMutSlice(); let first = view; let second = view; first.len(); second.len()",
            "O0",
        ),
        (
            "let view = data.asMutSlice(); let immutable = data.asSlice(); view.len(); immutable[0]",
            "O0",
        ),
        (
            "let mut view = data.asMutSlice(); match view.get(0 as usize) { Option.Some(value) => { view.set(0 as usize, 7.0 as f32); if *value == (0.0 as f32) { return 3 } } Option.None => {} }",
            "O0",
        ),
        (
            "let view = data.asMutSlice(); match view.get(0 as usize) { Option.Some(value) => { data.push(7.0 as f32); if *value == (0.0 as f32) { return 3 } } Option.None => {} }",
            "O0",
        ),
        (
            "let view = data.asMutSlice(); match view.get(0 as usize) { Option.Some(value) => { let moved = data; if *value == (0.0 as f32) { return 3 }; let length = moved.len() } Option.None => {} }",
            "O0",
        ),
    ] {
        fs::write(&path, format!("{prefix}{body}; return 0 }}")).unwrap();
        let output = common::cli_command()
            .arg("check")
            .arg(&path)
            .output()
            .unwrap();
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "accepted invalid exclusive view: {body}"
        );
        assert!(diagnostics.contains(expected), "{body}: {diagnostics}");
    }
    let body = r#"
    let mut view = data.asMutSlice()
    if !simd.F32x4.splat(9.0).storeUnaligned<vec.Vec<f32>>(mut ref view) { return 1 }
    match view.get(0 as usize) {
        Option.Some(value) => { if *value != (9.0 as f32) { return 2 } }
        Option.None => { return 3 }
    }
    return 0
}"#;
    fs::write(&path, format!("{prefix}{body}")).unwrap();
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "exclusive Vec view: {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
