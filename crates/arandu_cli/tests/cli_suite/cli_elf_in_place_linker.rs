//! Integration tests for Phase 2: In-Process ELF Linker via `mmap` (Linux ELF x86_64).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common;

fn run_cli_in(dir: &Path, args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cli should run")
}

fn files_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                files.extend(files_named(&path, name));
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn in_process_elf_linker_patches_when_supported_or_falls_back_safely() {
    let tmp = common::temp_dir("arandu_elf_test").unwrap();
    let project = tmp.join("elf_app");

    // 1. Create package
    let init = run_cli_in(&tmp, &["new", "elf_app", "--bin", "--vcs=none"]);
    assert!(init.status.success());

    // 2. Multi-function source
    let src_v1 = r#"module elf_app

func helper(): int {
    return 10;
}

func calculate(): int {
    return helper() + 32;
}

func main(): int {
    return calculate();
}
"#;
    fs::write(project.join("src/main.aru"), src_v1).unwrap();

    // 3. Cold build
    let build1 = run_cli_in(&project, &["build", "-v"]);
    assert!(
        build1.status.success(),
        "cold build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build1.stdout),
        String::from_utf8_lossy(&build1.stderr)
    );

    // Verify binary exists and returns 42
    let build_states1 = files_named(&project.join("target/dev"), "build-state.json");
    assert_eq!(build_states1.len(), 1);
    let state1: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states1[0]).unwrap()).unwrap();
    let bin_path1 = build_states1[0]
        .parent()
        .unwrap()
        .join(state1["artifact"].as_str().unwrap());
    assert!(bin_path1.is_file());
    let run1 = Command::new(&bin_path1).output().unwrap();
    assert_eq!(run1.status.code(), Some(42));

    // On Linux x86_64, verify elf_layout.json was generated
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let elf_layouts = files_named(&project.join("target/dev"), "elf_layout.json");
        assert_eq!(
            elf_layouts.len(),
            1,
            "elf_layout.json should be created after cold build"
        );
    }

    // 4. Modify calculate() body: 10 + 90 = 100
    let src_v2 = r#"module elf_app

func helper(): int {
    return 10;
}

func calculate(): int {
    return helper() + 90;
}

func main(): int {
    return calculate();
}
"#;
    fs::write(project.join("src/main.aru"), src_v2).unwrap();

    // 5. Incremental rebuild
    let build2 = run_cli_in(&project, &["build", "-v"]);
    assert!(
        build2.status.success(),
        "incremental build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build2.stdout),
        String::from_utf8_lossy(&build2.stderr)
    );
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let stderr2 = String::from_utf8_lossy(&build2.stderr);
        let stdout2 = String::from_utf8_lossy(&build2.stdout);
        let patched = stderr2.contains("[in-process-elf] patched 1 CGU(s) in-place")
            || stdout2.contains("backend=in-process-elf");
        let safe_fallback = stderr2.contains(
            "[in-process-elf] safety/determinism precondition not met; falling back to a full link",
        ) && stdout2.contains("backend=cranelift-dev");
        assert!(
            patched || safe_fallback,
            "expected a verified ELF patch or an explicit fail-closed fallback, got stdout={stdout2}, stderr={stderr2}"
        );
    }

    // 6. Verify modified binary returns 100
    let build_states2 = files_named(&project.join("target/dev"), "build-state.json");
    let state2: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states2[0]).unwrap()).unwrap();
    let bin_path2 = build_states2[0]
        .parent()
        .unwrap()
        .join(state2["artifact"].as_str().unwrap());
    assert!(bin_path2.is_file());
    let run2 = Command::new(&bin_path2).output().unwrap();
    assert_eq!(run2.status.code(), Some(100));

    // 7. Another incremental edit: modify helper() body to return 50: 50 + 90 = 140
    let src_v3 = r#"module elf_app

func helper(): int {
    return 50;
}

func calculate(): int {
    return helper() + 90;
}

func main(): int {
    return calculate();
}
"#;
    fs::write(project.join("src/main.aru"), src_v3).unwrap();

    let build3 = run_cli_in(&project, &["build", "-v"]);
    assert!(build3.status.success());
    let state3: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states2[0]).unwrap()).unwrap();
    let bin_path3 = build_states2[0]
        .parent()
        .unwrap()
        .join(state3["artifact"].as_str().unwrap());
    let run3 = Command::new(&bin_path3).output().unwrap();
    assert_eq!(run3.status.code(), Some(140));

    // An exact-size patched build must be byte-identical to a clean build of
    // the same final source; otherwise non-text ELF metadata went stale.
    let incremental_bytes = fs::read(&bin_path3).unwrap();
    let clean = run_cli_in(&project, &["clean"]);
    assert!(clean.status.success());
    let clean_build = run_cli_in(&project, &["build", "-v"]);
    assert!(clean_build.status.success());
    let clean_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states2[0]).unwrap()).unwrap();
    let clean_binary = build_states2[0]
        .parent()
        .unwrap()
        .join(clean_state["artifact"].as_str().unwrap());
    assert_eq!(incremental_bytes, fs::read(clean_binary).unwrap());

    // 8. Structural change: add a brand new function `bonus()`.
    // Since `bonus()` is a new function not in the previous layout,
    // in-process ELF linker pre-flight check should detect it and gracefully fall back to system linker!
    let src_v4 = r#"module elf_app

func helper(): int {
    return 50;
}

func bonus(): int {
    return 10;
}

func calculate(): int {
    return helper() + 90 + bonus();
}

func main(): int {
    return calculate();
}
"#;
    fs::write(project.join("src/main.aru"), src_v4).unwrap();

    let build4 = run_cli_in(&project, &["build", "-v"]);
    assert!(build4.status.success());
    let stdout4 = String::from_utf8_lossy(&build4.stdout);
    assert!(
        stdout4.contains("backend=cranelift-dev"),
        "expected fallback to system linker when new function is introduced, got: {stdout4}"
    );

    let state4: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states2[0]).unwrap()).unwrap();
    let bin_path4 = build_states2[0]
        .parent()
        .unwrap()
        .join(state4["artifact"].as_str().unwrap());
    let run4 = Command::new(&bin_path4).output().unwrap();
    assert_eq!(run4.status.code(), Some(150));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn rodata_edit_falls_back_and_produces_a_working_executable() {
    let tmp = common::temp_dir("arandu_elf_rodata_test").unwrap();
    let project = tmp.join("elf_rodata");
    assert!(
        run_cli_in(&tmp, &["new", "elf_rodata", "--bin", "--vcs=none"])
            .status
            .success()
    );
    let source = project.join("src/main.aru");
    fs::write(
        &source,
        "import io\nfunc main(): int { io.println(\"hello\"); return 0; }\n",
    )
    .unwrap();
    assert!(run_cli_in(&project, &["build", "-v"]).status.success());

    fs::write(
        &source,
        "import io\nfunc main(): int { io.println(\"world\"); return 0; }\n",
    )
    .unwrap();
    let rebuilt = run_cli_in(&project, &["build", "-v"]);
    assert!(
        rebuilt.status.success(),
        "rodata rebuild failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("backend=cranelift-dev"));
    let state_path = files_named(&project.join("target/dev"), "build-state.json")[0].clone();
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let executable = state_path
        .parent()
        .unwrap()
        .join(state["artifact"].as_str().unwrap());
    let output = Command::new(executable).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "world\n");

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn corrupted_layout_is_rejected_without_out_of_bounds_write() {
    let tmp = common::temp_dir("arandu_elf_layout_corruption_test").unwrap();
    let project = tmp.join("elf_layout_corruption");
    assert!(
        run_cli_in(
            &tmp,
            &["new", "elf_layout_corruption", "--bin", "--vcs=none"],
        )
        .status
        .success()
    );
    let source = project.join("src/main.aru");
    fs::write(&source, "func main(): int { return 1 }\n").unwrap();
    assert!(run_cli_in(&project, &["build", "-v"]).status.success());

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let layout_path = files_named(&project.join("target/dev"), "elf_layout.json")[0].clone();
        let mut layout: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&layout_path).unwrap()).unwrap();
        let slots = layout["function_slots"].as_object_mut().unwrap();
        let slot = slots.values_mut().next().unwrap();
        slot["file_offset"] = serde_json::Value::from(u64::MAX);
        fs::write(&layout_path, serde_json::to_vec_pretty(&layout).unwrap()).unwrap();

        fs::write(&source, "func main(): int { return 2 }\n").unwrap();
        let rebuilt = run_cli_in(&project, &["build", "-v"]);
        assert!(
            rebuilt.status.success(),
            "safe fallback failed: {}",
            String::from_utf8_lossy(&rebuilt.stderr)
        );
        assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("backend=cranelift-dev"));
        let state_path = files_named(&project.join("target/dev"), "build-state.json")[0].clone();
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        let executable = state_path
            .parent()
            .unwrap()
            .join(state["artifact"].as_str().unwrap());
        assert_eq!(Command::new(executable).status().unwrap().code(), Some(2));
    }

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn modified_source_never_patches_an_unverified_artifact_candidate() {
    let tmp = common::temp_dir("arandu_elf_artifact_corruption_test").unwrap();
    let project = tmp.join("elf_artifact_corruption");
    assert!(
        run_cli_in(
            &tmp,
            &["new", "elf_artifact_corruption", "--bin", "--vcs=none"],
        )
        .status
        .success()
    );
    let source = project.join("src/main.aru");
    fs::write(&source, "func main(): int { return 1 }\n").unwrap();
    assert!(run_cli_in(&project, &["build", "-v"]).status.success());

    let state_path = files_named(&project.join("target/dev"), "build-state.json")[0].clone();
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let executable = state_path
        .parent()
        .unwrap()
        .join(state["artifact"].as_str().unwrap());
    let mut corrupted = fs::read(&executable).unwrap();
    corrupted[0] ^= 0xff;
    fs::write(&executable, corrupted).unwrap();

    fs::write(&source, "func main(): int { return 2 }\n").unwrap();
    let rebuilt = run_cli_in(&project, &["build", "-v"]);
    assert!(
        rebuilt.status.success(),
        "safe rebuild failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("backend=cranelift-dev"));

    let repaired_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let repaired = state_path
        .parent()
        .unwrap()
        .join(repaired_state["artifact"].as_str().unwrap());
    assert_eq!(Command::new(repaired).status().unwrap().code(), Some(2));

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn eh_frame_mismatch_falls_back_to_full_link() {
    let tmp = common::temp_dir("arandu_elf_eh_frame_test").unwrap();
    let project = tmp.join("elf_eh_frame");
    assert!(
        run_cli_in(&tmp, &["new", "elf_eh_frame", "--bin", "--vcs=none"])
            .status
            .success()
    );
    let source = project.join("src/main.aru");
    fs::write(&source, "func main(): int { return 10 }\n").unwrap();
    assert!(run_cli_in(&project, &["build", "-v"]).status.success());

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let layout_path = files_named(&project.join("target/dev"), "elf_layout.json")[0].clone();
        let mut layout: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&layout_path).unwrap()).unwrap();
        let slots = layout["function_slots"].as_object_mut().unwrap();
        let slot = slots.values_mut().next().unwrap();
        // Mutate eh_frame_hash to simulate changed unwind info (e.g. prologue changed)
        slot["eh_frame_hash"] = serde_json::Value::from("mismatched_unwind_hash_0123456789abcdef");
        fs::write(&layout_path, serde_json::to_vec_pretty(&layout).unwrap()).unwrap();

        fs::write(&source, "func main(): int { return 20 }\n").unwrap();
        let rebuilt = run_cli_in(&project, &["build", "-v"]);
        assert!(
            rebuilt.status.success(),
            "unwind fallback rebuild failed: {}",
            String::from_utf8_lossy(&rebuilt.stderr)
        );
        // It must detect the unwind metadata mismatch and safely fall back to full link
        assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("backend=cranelift-dev"));
        let state_path = files_named(&project.join("target/dev"), "build-state.json")[0].clone();
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        let executable = state_path
            .parent()
            .unwrap()
            .join(state["artifact"].as_str().unwrap());
        assert_eq!(Command::new(executable).status().unwrap().code(), Some(20));
    }

    let _ = fs::remove_dir_all(tmp);
}
