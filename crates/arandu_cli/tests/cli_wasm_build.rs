//! Integration tests for WebAssembly builds via the Arandu CLI.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

mod common;

fn run_cli_in(dir: &Path, args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cli should run")
}

#[test]
fn wasm_target_build_and_incremental_session() {
    let tmp = common::temp_dir("arandu_wasm_cli_test").unwrap();
    let project = tmp.join("wasm_app");

    // 1. Create a new package
    let init = run_cli_in(&tmp, &["new", "wasm_app", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // 2. Cold build with --target wasm32-wasi
    let build1 = run_cli_in(&project, &["build", "--target=wasm32-wasi"]);
    let stdout1 = String::from_utf8_lossy(&build1.stdout);
    let stderr1 = String::from_utf8_lossy(&build1.stderr);
    assert!(
        build1.status.success(),
        "build 1 failed:\nstdout={stdout1}\nstderr={stderr1}"
    );
    assert!(
        stdout1.contains("backend=wasm-component"),
        "expected backend=wasm-component, got: {stdout1}"
    );
    assert!(
        !stdout1.contains("incremental: up-to-date"),
        "first build should not be up-to-date"
    );

    // Verify artifact is produced in target/dev/wasm32-wasi/bin/wasm_app.wasm
    let artifact_path = project.join("target/dev/wasm32-wasi/bin/wasm_app.wasm");
    assert!(
        artifact_path.is_file(),
        "expected artifact at {}",
        artifact_path.display()
    );
    let convenience_path = project.join("target/dev/wasm_app.wasm");
    assert!(
        convenience_path.is_file(),
        "expected convenience copy at {}",
        convenience_path.display()
    );

    let bytes = fs::read(&artifact_path).expect("read artifact");
    assert!(
        arandu_backend_wasm::is_component(&bytes) || arandu_backend_wasm::is_core_module(&bytes),
        "artifact must have valid wasm magic"
    );

    // 3. Second build without changes -> must report incremental up-to-date early cutoff
    let build2 = run_cli_in(&project, &["build", "--target=wasm32-wasi"]);
    let stdout2 = String::from_utf8_lossy(&build2.stdout);
    let stderr2 = String::from_utf8_lossy(&build2.stderr);
    assert!(
        build2.status.success(),
        "build 2 failed:\nstdout={stdout2}\nstderr={stderr2}"
    );
    assert!(
        stdout2.contains("incremental: up-to-date"),
        "expected incremental: up-to-date, got: {stdout2}"
    );
}

#[test]
fn component_manifest_declares_wasm_target() {
    let tmp = common::temp_dir("arandu_wasm_manifest_test").unwrap();
    let project = tmp.join("wasm_comp");

    // 1. Create a new package
    let init = run_cli_in(&tmp, &["new", "wasm_comp", "--bin", "--vcs=none"]);
    assert!(init.status.success());

    // 2. Set target-type = "component" in arandu.toml
    let manifest_path = project.join("arandu.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("read arandu.toml");
    manifest = manifest.replace("[package]\n", "[package]\ntarget-type = \"component\"\n");
    manifest.push_str("\n[wasm]\nmemory-initial-pages = 4\nenable-threads = false\n");
    fs::write(&manifest_path, manifest).expect("write arandu.toml");

    // 3. Run build without --target flag -> should automatically detect component target
    let build = run_cli_in(&project, &["build"]);
    let stdout = String::from_utf8_lossy(&build.stdout);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build.status.success(),
        "build failed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("backend=wasm-component"),
        "expected backend=wasm-component, got: {stdout}"
    );

    let artifact_path = project.join("target/dev/wasm_comp.wasm");
    assert!(
        artifact_path.is_file(),
        "expected artifact at {}",
        artifact_path.display()
    );
}
