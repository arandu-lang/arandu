//! Integration test for Phase 0: Incremental session persistence between CLI runs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

mod common;

fn run_cli_in(dir: &Path, args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cli should run")
}

#[test]
fn consecutive_builds_perform_early_cutoff_noop() {
    let tmp = common::temp_dir("arandu_incremental_test").unwrap();
    let project = tmp.join("inc_proj");

    // 1. Create a new package
    let init = run_cli_in(&tmp, &["new", "inc_proj", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // 2. Cold build
    let build1 = run_cli_in(&project, &["build"]);
    assert!(
        build1.status.success(),
        "build 1 failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build1.stdout),
        String::from_utf8_lossy(&build1.stderr)
    );
    let stdout1 = String::from_utf8_lossy(&build1.stdout);
    assert!(!stdout1.contains("incremental: up-to-date"));

    // Verify session-fingerprint.json was created
    let fingerprint_files = files_named(&project.join("target/dev"), "session-fingerprint.json");
    assert_eq!(
        fingerprint_files.len(),
        1,
        "expected exactly 1 session-fingerprint.json in target/dev"
    );
    assert!(fingerprint_files[0].is_file());

    let compiler_artifacts = ["current.air", "current.amir", "current.ameta"].map(|name| {
        let matches = files_named(&project.join("target/dev"), name);
        assert_eq!(matches.len(), 1, "expected exactly one {name}");
        let bytes = fs::read(&matches[0]).unwrap();
        assert!(
            bytes.starts_with(b"ARANCAS\0"),
            "invalid envelope for {name}"
        );
        (matches[0].clone(), bytes)
    });

    // 3. Second build without changes -> must be up-to-date early cutoff!
    let build2 = run_cli_in(&project, &["build"]);
    assert!(
        build2.status.success(),
        "build 2 failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build2.stdout),
        String::from_utf8_lossy(&build2.stderr)
    );
    let stdout2 = String::from_utf8_lossy(&build2.stdout);
    assert!(
        stdout2.contains("incremental: up-to-date"),
        "expected build 2 to be up-to-date, got:\n{stdout2}"
    );
    for (path, expected) in &compiler_artifacts {
        assert_eq!(&fs::read(path).unwrap(), expected);
    }

    // A compiler sidecar is part of the session closure. Corruption must fail
    // closed, rebuild all sidecars, and never be accepted as an early cutoff.
    let mut corrupted_sidecar = compiler_artifacts[1].1.clone();
    *corrupted_sidecar.last_mut().unwrap() ^= 0xff;
    fs::write(&compiler_artifacts[1].0, corrupted_sidecar).unwrap();
    let repaired_sidecar = run_cli_in(&project, &["build", "-v"]);
    assert!(
        repaired_sidecar.status.success(),
        "sidecar repair failed: {}",
        String::from_utf8_lossy(&repaired_sidecar.stderr)
    );
    assert!(!String::from_utf8_lossy(&repaired_sidecar.stdout).contains("incremental: up-to-date"));
    assert_eq!(
        fs::read(&compiler_artifacts[1].0).unwrap(),
        compiler_artifacts[1].1
    );

    // Corrupting a content-addressed executable must invalidate and repair it.
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
    let repaired = run_cli_in(&project, &["build", "-v"]);
    assert!(
        repaired.status.success(),
        "artifact repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    assert!(!String::from_utf8_lossy(&repaired.stdout).contains("incremental: up-to-date"));
    assert!(Command::new(&executable).output().unwrap().status.success());

    // 4. Modifying source content forces rebuild
    let main_src = project.join("src/main.aru");
    let original_content = fs::read_to_string(&main_src).unwrap();
    let modified_content = format!("{original_content}\nfunc helper(): int {{ return 42 }}\n");
    fs::write(&main_src, modified_content).unwrap();

    let build3 = run_cli_in(&project, &["build"]);
    assert!(build3.status.success());
    let stdout3 = String::from_utf8_lossy(&build3.stdout);
    assert!(
        !stdout3.contains("incremental: up-to-date"),
        "modified content should trigger rebuild"
    );

    // 5. Subsequent build after modification is up-to-date again
    let build4 = run_cli_in(&project, &["build"]);
    assert!(build4.status.success());
    let stdout4 = String::from_utf8_lossy(&build4.stdout);
    assert!(
        stdout4.contains("incremental: up-to-date"),
        "subsequent build must be up-to-date"
    );

    // 6. Clean removes target and cache
    let clean = run_cli_in(&project, &["clean"]);
    assert!(clean.status.success());
    assert!(!project.join("target").exists());

    // 7. Build after clean is cold again
    let build5 = run_cli_in(&project, &["build"]);
    assert!(build5.status.success());
    let stdout5 = String::from_utf8_lossy(&build5.stdout);
    assert!(
        !stdout5.contains("incremental: up-to-date"),
        "build after clean must be cold"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn documentation_only_edit_cuts_off_before_compiler_queries() {
    let tmp = common::temp_dir("arandu_inc_docs_test").unwrap();
    let project = tmp.join("docs_proj");
    let init = run_cli_in(&tmp, &["new", "docs_proj", "--bin", "--vcs=none"]);
    assert!(init.status.success());
    let source = project.join("src/main.aru");
    fs::write(&source, "/// first\nfunc main(): int { return 0 }\n").unwrap();
    assert!(run_cli_in(&project, &["build"]).status.success());

    fs::write(
        &source,
        "/// changed documentation\n\nfunc main(): int {  return 0 }\n",
    )
    .unwrap();
    let build = run_cli_in(&project, &["build", "-v"]);
    assert!(build.status.success());
    assert!(String::from_utf8_lossy(&build.stdout).contains("incremental: up-to-date"));
    assert!(!String::from_utf8_lossy(&build.stderr).contains("[rebuilt:"));

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn local_dependency_source_participates_in_the_session_fingerprint() {
    let project = common::temp_dir("arandu_inc_dependency_test").unwrap();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("packages/math/src")).unwrap();
    fs::write(
        project.join("arandu.toml"),
        r#"schema = 1
[package]
name = "dependency_app"
version = "0.1.0"
edition = "2026"
[targets.bin]
name = "dependency_app"
root = "src/main.aru"
[dependencies]
math = { path = "packages/math" }
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/main.aru"),
        "import math.geometry as geometry\nfunc main(): int { return geometry.answer(); }\n",
    )
    .unwrap();
    fs::write(
        project.join("packages/math/arandu.toml"),
        r#"schema = 1
[package]
name = "upstream_math"
version = "1.0.0"
edition = "2026"
[targets.lib]
name = "math"
root = "src/lib.aru"
[targets.lib.exports]
"." = "src/lib.aru"
"geometry" = "src/geometry.aru"
"#,
    )
    .unwrap();
    fs::write(project.join("packages/math/src/lib.aru"), "").unwrap();
    let dependency = project.join("packages/math/src/geometry.aru");
    fs::write(&dependency, "public func answer(): int { return 42; }\n").unwrap();

    let cold = run_cli_in(&project, &["build"]);
    assert!(
        cold.status.success(),
        "dependency cold build failed: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run_cli_in(&project, &["build"]).stdout)
            .contains("incremental: up-to-date")
    );

    fs::write(&dependency, "public func answer(): int { return 43; }\n").unwrap();
    let rebuilt = run_cli_in(&project, &["build", "-v"]);
    assert!(
        rebuilt.status.success(),
        "dependency rebuild failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert!(!String::from_utf8_lossy(&rebuilt.stdout).contains("incremental: up-to-date"));

    let state_path = files_named(&project.join("target/dev"), "build-state.json")[0].clone();
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let executable = state_path
        .parent()
        .unwrap()
        .join(state["artifact"].as_str().unwrap());
    assert_eq!(Command::new(executable).status().unwrap().code(), Some(43));

    let _ = fs::remove_dir_all(project);
}

#[test]
fn touch_without_content_change_skips_rebuild() {
    let tmp = common::temp_dir("arandu_inc_touch_test").unwrap();
    let project = tmp.join("touch_proj");

    let init = run_cli_in(&tmp, &["new", "touch_proj", "--bin", "--vcs=none"]);
    assert!(init.status.success());

    let build1 = run_cli_in(&project, &["build"]);
    assert!(build1.status.success());
    assert!(!String::from_utf8_lossy(&build1.stdout).contains("incremental: up-to-date"));

    // Overwrite with exact same content to update mtime without changing content
    let main_src = project.join("src/main.aru");
    let content = fs::read_to_string(&main_src).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    fs::write(&main_src, content).unwrap();

    let build2 = run_cli_in(&project, &["build"]);
    assert!(build2.status.success());
    let stdout2 = String::from_utf8_lossy(&build2.stdout);
    assert!(
        stdout2.contains("incremental: up-to-date"),
        "touching file without content changes must still perform early cutoff, got:\n{stdout2}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

fn files_named(root: &Path, name: &str) -> Vec<std::path::PathBuf> {
    collect_files(root, &|path| {
        path.file_name().and_then(|value| value.to_str()) == Some(name)
    })
}

fn collect_files(root: &Path, predicate: &dyn Fn(&Path) -> bool) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(collect_files(&path, predicate));
            } else if predicate(&path) {
                result.push(path);
            }
        }
    }
    result
}
