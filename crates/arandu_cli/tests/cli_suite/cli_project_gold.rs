//! P2 gold bars: project CLI (`new` / `doctor` / package check|run|build),
//! manifest errors, rebuild status line, backend convention.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

use crate::common;

fn run_cli(args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .output()
        .expect("cli should run")
}

fn run_cli_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cli should run")
}

#[test]
fn size_inspection_rejects_debug_before_reading_the_source() {
    for command in ["emit-c", "emit-wasm", "emit-component"] {
        let out = run_cli(&[command, "missing.aru", "--size", "--debug"]);
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--size") && stderr.contains("--debug"),
            "{stderr}"
        );
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn native_build_does_not_treat_i686_layout_as_wasm() {
    let tmp = tempfile_dir("arandu_native_i686_layout");
    let project = tmp.join("native_i686_layout");
    let init = run_cli_in(&tmp, &["new", "native_i686_layout", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new project: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let build = run_cli_in(&project, &["build", "--layout=i686"]);
    assert!(
        !build.status.success(),
        "i686 layout unexpectedly built as a native artifact"
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("native builds require the host data layout"),
        "expected explicit layout rejection, got: {stderr}"
    );
    assert!(
        !project.join("target/dev/wasm32-wasip1").exists(),
        "32-bit layout must not create an implicit Wasm artifact"
    );
}

#[test]
fn cache_dir_reports_the_absolute_override_without_creating_it() {
    let tmp = tempfile_dir("arandu_cache_dir");
    let cache = tmp.join("global-cache");
    let out = run_cli(&["cache", "dir", &format!("--cache-dir={}", cache.display())]);
    assert!(
        out.status.success(),
        "cache dir failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        cache.to_string_lossy()
    );
    assert!(
        !cache.exists(),
        "cache discovery must not mutate the filesystem"
    );
}

#[test]
fn doctor_reports_binary_and_stdlib() {
    let out = run_cli(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "doctor should pass in monorepo: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Doctor summary"),
        "expected Flutter-style header, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Arandu toolchain"),
        "expected toolchain category:\n{stdout}"
    );
    assert!(
        stdout.contains("Stdlib"),
        "expected Stdlib category:\n{stdout}"
    );
    assert!(
        stdout.contains("Cranelift") || stdout.contains("cranelift"),
        "expected Cranelift category:\n{stdout}"
    );
    assert!(
        stdout.contains("No issues found"),
        "expected clean summary:\n{stdout}"
    );
}

#[test]
fn doctor_verbose_expands_details() {
    let out = run_cli(&["doctor", "-v"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "doctor -v failed: {stdout}");
    assert!(
        stdout.contains("binary at") || stdout.contains("stdlib at"),
        "verbose should expand detail bullets:\n{stdout}"
    );
    assert!(
        stdout.contains("cascade:") || stdout.contains("relative to binary"),
        "verbose should mention resolution cascade:\n{stdout}"
    );
}

#[test]
fn new_scaffolds_package_and_check_run() {
    let tmp = tempfile_dir("arandu_new_gold");
    let name = "hello_gold";
    let project = tmp.join(name);

    let new_out = run_cli_in(&tmp, &["new", name]);
    assert!(
        new_out.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    assert!(project.join("arandu.toml").is_file());
    assert!(project.join("src/main.aru").is_file());
    assert!(project.join("README.md").is_file());
    assert!(project.join(".gitignore").is_file());
    assert!(project.join("tests/smoke.aru").is_file());

    let check = run_cli_in(&project, &["check"]);
    assert!(
        check.status.success(),
        "project check failed: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("ok"));
    let lock_bytes = fs::read(project.join("arandu.lock")).unwrap();
    assert!(!lock_bytes.contains(&b'\r'));
    assert!(String::from_utf8_lossy(&lock_bytes).contains("version = 2"));

    let run = run_cli_in(&project, &["run", "--verbose"]);
    assert!(
        run.status.success(),
        "project run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("[rebuilt:") || stderr.contains("[cached]"),
        "run must print DX.5 status line, got stderr={stderr}"
    );

    let build = run_cli_in(&project, &["build"]);
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(String::from_utf8_lossy(&build.stdout).contains("cranelift"));
    let build_states = files_named(&project.join("target"), "build-state.json");
    assert_eq!(build_states.len(), 1);
    let state = fs::read_to_string(&build_states[0]).unwrap();
    assert!(state.contains("\"schema\": 2"));
    assert!(state.contains("\"backend\": \"cranelift-aot\""));
    assert!(state.contains("\"compiler_version\""));
    assert!(state.contains("\"linker\""));
    let first_state: serde_json::Value = serde_json::from_str(&state).unwrap();
    let first_artifact = build_states[0]
        .parent()
        .unwrap()
        .join(first_state["artifact"].as_str().unwrap());
    assert!(first_artifact.is_file());
    assert_eq!(
        Command::new(&first_artifact)
            .output()
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    let object_extension = if cfg!(windows) { "obj" } else { "o" };
    assert_eq!(
        files_with_extension(&project.join("target"), object_extension).len(),
        1
    );
    let repeated = run_cli_in(&project, &["build"]);
    assert!(repeated.status.success());
    let repeated_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states[0]).unwrap()).unwrap();
    assert_eq!(
        repeated_state["artifact"], first_state["artifact"],
        "identical native inputs must select a byte-identical executable"
    );

    fs::write(
        project.join("src/main.aru"),
        "module hello_gold\nfunc main(): int { return 1 }\n",
    )
    .unwrap();
    let rebuild = run_cli_in(&project, &["build"]);
    assert!(rebuild.status.success());
    assert_eq!(
        files_with_extension(&project.join("target"), object_extension).len(),
        2,
        "a successful rebuild retains the previous immutable artifact"
    );
    let second_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states[0]).unwrap()).unwrap();
    let second_artifact = build_states[0]
        .parent()
        .unwrap()
        .join(second_state["artifact"].as_str().unwrap());
    assert_ne!(first_artifact, second_artifact);
    assert!(
        first_artifact.is_file(),
        "last valid artifact was discarded"
    );
    assert_eq!(
        Command::new(&second_artifact)
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );

    fs::write(
        project.join("src/main.aru"),
        "module hello_gold\nfunc main(): int { return missing_name }\n",
    )
    .unwrap();
    let failed = run_cli_in(&project, &["build"]);
    assert_eq!(failed.status.code(), Some(1));
    let state_after_failure = fs::read_to_string(&build_states[0]).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&state_after_failure).unwrap()["artifact"],
        second_state["artifact"],
        "failed build must leave the last valid publication selected"
    );
    assert_eq!(
        Command::new(&second_artifact)
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );

    // Release builds use the speed-oriented Cranelift AOT and O2 middle-end
    // pipeline while preserving the same transactional publication contract.
    fs::write(
        project.join("src/main.aru"),
        "module hello_gold\nfunc main(): int { return 2 }\n",
    )
    .unwrap();
    let rel = run_cli_in(&project, &["build", "--release"]);
    assert!(
        rel.status.success(),
        "release build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&rel.stdout),
        String::from_utf8_lossy(&rel.stderr)
    );
    assert!(String::from_utf8_lossy(&rel.stdout).contains("backend=cranelift-release"));
    let release_states = files_named(&project.join("target/release"), "build-state.json");
    assert_eq!(release_states.len(), 1);
    let release_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&release_states[0]).unwrap()).unwrap();
    assert_eq!(release_state["profile"], "release");
    assert_eq!(release_state["backend"], "cranelift-aot-speed");
    let release_bytes = fs::read(&release_states[0]).unwrap();

    let size = run_cli_in(&project, &["build", "--size"]);
    assert!(
        size.status.success(),
        "size build failed: {}",
        String::from_utf8_lossy(&size.stderr)
    );
    assert!(String::from_utf8_lossy(&size.stdout).contains("backend=cranelift-size"));
    let size_states = files_named(&project.join("target/size"), "build-state.json");
    assert_eq!(size_states.len(), 1);
    let size_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&size_states[0]).unwrap()).unwrap();
    assert_eq!(size_state["profile"], "size");
    assert_eq!(size_state["backend"], "cranelift-aot-size");
    let size_artifact = size_states[0]
        .parent()
        .unwrap()
        .join(size_state["artifact"].as_str().unwrap());
    assert_eq!(
        Command::new(&size_artifact).status().unwrap().code(),
        Some(2)
    );
    let size_bytes = fs::read(&size_artifact).unwrap();
    let repeated_size = run_cli_in(&project, &["build", "-Oz"]);
    assert!(repeated_size.status.success());
    assert!(String::from_utf8_lossy(&repeated_size.stdout).contains("incremental: up-to-date"));
    assert_eq!(fs::read(&size_artifact).unwrap(), size_bytes);
    assert_eq!(fs::read(&release_states[0]).unwrap(), release_bytes);

    for args in [
        &["build", "--size", "--release"][..],
        &["build", "--size", "--debug"][..],
        &["run", "--size"][..],
    ] {
        let invalid = run_cli_in(&project, args);
        assert!(
            !invalid.status.success(),
            "accepted incompatible flags: {args:?}"
        );
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("--size"));
    }

    let clean = run_cli_in(&project, &["clean"]);
    assert!(clean.status.success());
    assert!(!project.join("target").exists());
    let clean_again = run_cli_in(&project, &["clean"]);
    assert!(clean_again.status.success());
    assert!(String::from_utf8_lossy(&clean_again.stdout).contains("already clean"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn locked_offline_and_frozen_have_independent_policy() {
    let tmp = tempfile_dir("arandu_lock_policy");
    let project = tmp.join("lock_policy");
    assert!(
        run_cli_in(&tmp, &["new", "lock_policy", "--vcs=none"])
            .status
            .success()
    );

    let missing_locked = run_cli_in(&project, &["check", "--locked"]);
    assert!(!missing_locked.status.success());
    assert!(String::from_utf8_lossy(&missing_locked.stderr).contains("--locked"));
    assert!(!project.join("arandu.lock").exists());

    let offline = run_cli_in(&project, &["check", "--offline"]);
    assert!(
        offline.status.success(),
        "offline root resolution failed: {}",
        String::from_utf8_lossy(&offline.stderr)
    );
    let original = fs::read(project.join("arandu.lock")).unwrap();

    let frozen = run_cli_in(&project, &["check", "--frozen"]);
    assert!(frozen.status.success());
    assert_eq!(fs::read(project.join("arandu.lock")).unwrap(), original);
}

#[test]
fn tree_is_canonical_and_verify_is_locked_offline() {
    let tmp = tempfile_dir("arandu_graph_inspect");
    let project = tmp.join("graph_inspect");
    assert!(
        run_cli_in(&tmp, &["new", "graph_inspect", "--vcs=none"])
            .status
            .success()
    );

    let tree = run_cli_in(&project, &["tree"]);
    assert!(tree.status.success());
    let first = String::from_utf8_lossy(&tree.stdout).into_owned();
    assert!(first.starts_with("graph blake3:"));
    assert!(first.contains(" root local"), "{first}");
    assert!(project.join("arandu.lock").is_file());
    let second = run_cli_in(&project, &["tree", "--locked"]);
    assert!(second.status.success());
    assert_eq!(first, String::from_utf8_lossy(&second.stdout));

    let verified = run_cli_in(&project, &["verify"]);
    assert!(verified.status.success());
    assert!(String::from_utf8_lossy(&verified.stdout).contains("verified locked offline graph"));

    let manifest = project.join("arandu.toml");
    let changed = fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.0.1\"", "version = \"0.0.2\"");
    fs::write(&manifest, changed).unwrap();
    assert!(!run_cli_in(&project, &["verify"]).status.success());

    fs::remove_dir_all(tmp).unwrap();
}

#[test]
fn audit_and_vendor_are_locked_and_deterministic() {
    let tmp = tempfile_dir("arandu_audit_vendor");
    let project = tmp.join("audit_vendor");
    assert!(
        run_cli_in(&tmp, &["new", "audit_vendor", "--vcs=none"])
            .status
            .success()
    );
    assert!(run_cli_in(&project, &["check"]).status.success());
    let misplaced_accept = run_cli_in(&project, &["check", "--accept"]);
    assert_eq!(misplaced_accept.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&misplaced_accept.stderr).contains("only"));
    let audit = run_cli_in(&project, &["audit"]);
    assert!(
        audit.status.success(),
        "{}",
        String::from_utf8_lossy(&audit.stderr)
    );
    let text = String::from_utf8_lossy(&audit.stdout);
    assert!(text.contains("integrity=verified"));
    assert!(text.contains("advisories=not-configured"));
    let vendor = run_cli_in(&project, &["vendor"]);
    assert!(
        vendor.status.success(),
        "{}",
        String::from_utf8_lossy(&vendor.stderr)
    );
    assert!(project.join("vendor/arandu/arandu-vendor.toml").is_file());
    let first = fs::read(project.join("vendor/arandu/arandu-vendor.toml")).unwrap();
    assert!(run_cli_in(&project, &["vendor"]).status.success());
    assert_eq!(
        first,
        fs::read(project.join("vendor/arandu/arandu-vendor.toml")).unwrap()
    );
    fs::remove_dir_all(tmp).unwrap();
}

#[test]
fn stale_or_corrupt_lock_never_builds_under_locked_policy() {
    let tmp = tempfile_dir("arandu_stale_lock");
    let project = tmp.join("stale_lock");
    assert!(
        run_cli_in(&tmp, &["new", "stale_lock", "--vcs=none"])
            .status
            .success()
    );
    assert!(run_cli_in(&project, &["check"]).status.success());
    let lock_path = project.join("arandu.lock");
    let original = fs::read(&lock_path).unwrap();

    let manifest_path = project.join("arandu.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(&manifest_path, manifest.replace("0.1.0", "0.1.1")).unwrap();
    let stale = run_cli_in(&project, &["check", "--locked"]);
    assert!(!stale.status.success());
    assert_eq!(fs::read(&lock_path).unwrap(), original);

    fs::write(
        &lock_path,
        b"version = 1\nmanifest_fingerprint = 'broken'\n",
    )
    .unwrap();
    let corrupt = run_cli_in(&project, &["check"]);
    assert!(!corrupt.status.success());
    assert_eq!(
        fs::read(&lock_path).unwrap(),
        b"version = 1\nmanifest_fingerprint = 'broken'\n"
    );
}

#[test]
fn local_workspace_dependency_resolves_only_declared_exports() {
    let project = tempfile_dir("arandu_p4_workspace");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("packages/math/src/internal")).unwrap();
    fs::write(
        project.join("arandu.toml"),
        r#"schema = 1
[package]
name = "calculator"
version = "0.1.0"
edition = "2026"
[targets.bin]
name = "calculator"
root = "src/main.aru"
[dependencies]
math = { path = "packages/math" }
[workspace]
members = ["packages/math"]
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/main.aru"),
        "import math.geometry as geometry\nfunc main(): int { return geometry.answer() }\n",
    )
    .unwrap();
    fs::write(
        project.join("packages/math/arandu.toml"),
        r#"schema = 1
[package]
name = "upstream_math"
version = "1.2.3"
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
    fs::write(
        project.join("packages/math/src/lib.aru"),
        "public func root_answer(): int { return 7 }\n",
    )
    .unwrap();
    fs::write(
        project.join("packages/math/src/geometry.aru"),
        "public func answer(): int { return 42 }\n",
    )
    .unwrap();
    fs::write(
        project.join("packages/math/src/internal/secret.aru"),
        "public func secret(): int { return 99 }\n",
    )
    .unwrap();

    let check = run_cli_in(&project, &["check"]);
    assert!(
        check.status.success(),
        "declared dependency export failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let lock = fs::read_to_string(project.join("arandu.lock")).unwrap();
    assert!(lock.contains("source = \"path+packages/math\""));
    assert!(lock.contains("name = \"upstream_math\""));
    assert!(!lock.contains(&project.to_string_lossy().to_string()));

    fs::write(
        project.join("src/main.aru"),
        "import math.internal.secret as secret\nfunc main(): int { return secret.secret() }\n",
    )
    .unwrap();
    let deep_import = run_cli_in(&project, &["check", "--locked"]);
    assert!(!deep_import.status.success());
    assert!(String::from_utf8_lossy(&deep_import.stderr).contains("M001"));
}

#[test]
fn workspace_rejects_cycles_undeclared_members_and_duplicate_identity_aliases() {
    let project = tempfile_dir("arandu_p4_graph_rejection");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("packages/a/src")).unwrap();
    fs::write(
        project.join("src/main.aru"),
        "func main(): int { return 0 }\n",
    )
    .unwrap();
    fs::write(
        project.join("arandu.toml"),
        r#"schema=1
[package]
name="root_app"
version="0.1.0"
edition="2026"
[targets.bin]
name="root_app"
root="src/main.aru"
[dependencies]
a = { path = "packages/a" }
a_again = { path = "packages/a" }
[workspace]
members=["packages/a"]
"#,
    )
    .unwrap();
    fs::write(
        project.join("packages/a/arandu.toml"),
        r#"schema=1
[package]
name="a"
version="1.0.0"
edition="2026"
[targets.lib]
name="a"
root="src/lib.aru"
[targets.lib.exports]
"."="src/lib.aru"
"#,
    )
    .unwrap();
    fs::write(
        project.join("packages/a/src/lib.aru"),
        "public func value(): int { return 1 }\n",
    )
    .unwrap();
    let duplicate = run_cli_in(&project, &["check"]);
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("more than once"));

    let manifest = fs::read_to_string(project.join("arandu.toml"))
        .unwrap()
        .replace("a_again = { path = \"packages/a\" }\n", "");
    fs::write(project.join("arandu.toml"), manifest).unwrap();
    let member_manifest = fs::read_to_string(project.join("packages/a/arandu.toml"))
        .unwrap()
        .replace(
            "[targets.lib.exports]",
            "[dependencies]\nroot_app = { path = \"../..\" }\n[targets.lib.exports]",
        );
    fs::write(project.join("packages/a/arandu.toml"), member_manifest).unwrap();
    let cycle = run_cli_in(&project, &["check"]);
    assert!(!cycle.status.success());
    assert!(String::from_utf8_lossy(&cycle.stderr).contains("cyclic package dependency"));
}

#[test]
fn new_lib_and_init_create_the_requested_targets_without_git() {
    let tmp = tempfile_dir("arandu_scaffold_kinds");
    let new_lib = run_cli_in(&tmp, &["new", "math_lib", "--lib", "--vcs=none"]);
    assert!(
        new_lib.status.success(),
        "{}",
        String::from_utf8_lossy(&new_lib.stderr)
    );
    let library = tmp.join("math_lib");
    assert!(library.join("src/lib.aru").is_file());
    assert!(!library.join("src/main.aru").exists());
    assert!(!library.join(".git").exists());
    let manifest = fs::read_to_string(library.join("arandu.toml")).unwrap();
    assert!(manifest.contains("[targets.lib]"));

    let existing = tmp.join("existing_app");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep.txt"), "untouched\n").unwrap();
    let init = run_cli_in(&existing, &["init", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(existing.join("arandu.toml").is_file());
    assert!(existing.join("src/main.aru").is_file());
    assert_eq!(
        fs::read_to_string(existing.join("keep.txt")).unwrap(),
        "untouched\n"
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn new_classifies_usage_and_operational_failures() {
    let tmp = tempfile_dir("arandu_new_failure_classes");

    let created = run_cli_in(&tmp, &["new", "hello"]);
    assert!(created.status.success());
    let created_stdout = String::from_utf8_lossy(&created.stdout);
    assert!(created_stdout.contains("arandu check"));
    assert!(created_stdout.contains("arandu run"));
    assert!(!created_stdout.contains("arandu_cli"));

    let invalid = run_cli_in(&tmp, &["new", "../escape"]);
    assert_eq!(invalid.status.code(), Some(2));
    let invalid_stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(invalid_stderr.contains("invalid project name"));

    fs::create_dir(tmp.join("occupied")).unwrap();
    let occupied = run_cli_in(&tmp, &["new", "occupied"]);
    assert_eq!(occupied.status.code(), Some(1));
    let occupied_stderr = String::from_utf8_lossy(&occupied.stderr);
    assert!(occupied_stderr.contains("create project"));
    assert!(occupied_stderr.contains("path already exists"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn new_rejects_nonportable_names_and_case_collisions() {
    let tmp = tempfile_dir("arandu_new_portable_names");
    fs::create_dir(tmp.join("Hello_App")).unwrap();

    for name in ["std", "olá", "hello world"] {
        let output = run_cli_in(&tmp, &["new", name, "--vcs=none"]);
        assert_eq!(output.status.code(), Some(1), "name `{name}` should fail");
        assert!(!tmp.join(name).exists());
    }

    let collision = run_cli_in(&tmp, &["new", "hello_app", "--vcs=none"]);
    assert_eq!(collision.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&collision.stderr).contains("only by case"),
        "{}",
        String::from_utf8_lossy(&collision.stderr)
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn init_is_transactional_when_a_generated_file_exists() {
    let tmp = tempfile_dir("arandu_init_transaction");
    fs::write(tmp.join("README.md"), "keep me\n").unwrap();

    let output = run_cli_in(&tmp, &["init", "--vcs=none"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(tmp.join("README.md")).unwrap(),
        "keep me\n"
    );
    assert!(!tmp.join("arandu.toml").exists());
    assert!(!tmp.join("src").exists());
    assert!(!tmp.join("tests").exists());
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn stale_interrupted_staging_does_not_block_new() {
    let tmp = tempfile_dir("arandu_new_interrupted");
    let stale = tmp.join(format!(".recoverable.arandu-new-{}", std::process::id()));
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("partial"), "incomplete\n").unwrap();

    let output = run_cli_in(&tmp, &["new", "recoverable", "--vcs=none"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.join("recoverable/arandu.toml").is_file());
    assert_eq!(
        fs::read_to_string(stale.join("partial")).unwrap(),
        "incomplete\n"
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn vcs_auto_reuses_parent_and_explicit_git_creates_repository() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let tmp = tempfile_dir("arandu_new_vcs");
    assert!(
        Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&tmp)
            .status()
            .unwrap()
            .success()
    );

    let nested = run_cli_in(&tmp, &["new", "nested", "--vcs=auto"]);
    assert!(
        nested.status.success(),
        "{}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(!tmp.join("nested/.git").exists());

    let explicit = run_cli_in(&tmp, &["new", "standalone", "--vcs=git"]);
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(tmp.join("standalone/.git").is_dir());
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn clean_refuses_an_unowned_target_directory() {
    let tmp = tempfile_dir("arandu_clean_unowned");
    assert!(
        run_cli_in(&tmp, &["new", "guarded", "--vcs=none"])
            .status
            .success()
    );
    let project = tmp.join("guarded");
    fs::create_dir(project.join("target")).unwrap();
    fs::write(project.join("target/keep.txt"), "user data\n").unwrap();

    let clean = run_cli_in(&project, &["clean"]);
    assert_eq!(clean.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&clean.stderr).contains("refusing"));
    assert_eq!(
        fs::read_to_string(project.join("target/keep.txt")).unwrap(),
        "user data\n"
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn package_local_multi_file_check_and_run() {
    let tmp = tempfile_dir("arandu_l2_pkg");
    let name = "pkg_l2";
    let project = tmp.join(name);

    assert!(run_cli_in(&tmp, &["new", name]).status.success());

    // Add util module next to main (package src root).
    fs::write(
        project.join("src/util.aru"),
        r#"
public func answer(): int {
    return 42
}
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/main.aru"),
        format!(
            r#"
module {name}
import {name}.util as util
func main(): int {{
    return util.answer()
}}
"#
        ),
    )
    .unwrap();

    let check = run_cli_in(&project, &["check"]);
    assert!(
        check.status.success(),
        "L2 package check failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    let run = run_cli_in(&project, &["run"]);
    // main returns 42 — process exit code is 42 (not OS "success").
    assert_eq!(
        run.status.code(),
        Some(42),
        "L2 package run exit: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn reserved_package_name_std_rejected() {
    let tmp = tempfile_dir("arandu_reserved_std");
    fs::write(
        tmp.join("Arandu.toml"),
        r#"
name = "std"
version = "0.0.1"
entry = "src/main.aru"
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.join("src")).unwrap();
    fs::write(tmp.join("src/main.aru"), "func main(): int { return 0 }\n").unwrap();
    let out = run_cli_in(&tmp, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("reserved") || err.contains("std"),
        "expected reserved name error, got: {err}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn malformed_manifest_is_hard_error() {
    let tmp = tempfile_dir("arandu_bad_manifest");
    fs::write(tmp.join("Arandu.toml"), "this is not valid toml {{{\n").unwrap();
    let out = run_cli_in(&tmp, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("malformed") || err.contains("failed") || err.contains("error"),
        "expected parse error, got: {err}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn legacy_manifest_loads_with_migration_warning() {
    let tmp = tempfile_dir("arandu_legacy_manifest");
    fs::create_dir_all(tmp.join("src")).unwrap();
    fs::write(
        tmp.join("Arandu.toml"),
        "name = \"legacy_app\"\nversion = \"0.1.0\"\nentry = \"src/main.aru\"\n",
    )
    .unwrap();
    fs::write(tmp.join("src/main.aru"), "func main(): int { return 0 }\n").unwrap();

    let out = run_cli_in(&tmp, &["check"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("deprecated"));
    assert!(stderr.contains("arandu.toml"));
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn missing_entry_field_is_hard_error() {
    let tmp = tempfile_dir("arandu_missing_entry");
    fs::write(
        tmp.join("Arandu.toml"),
        "name = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let out = run_cli_in(&tmp, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("entry") || err.contains("missing"),
        "expected missing entry, got: {err}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn single_file_run_prints_status_line() {
    let tmp = tempfile_dir("arandu_run_status");
    let file = tmp.join("main.aru");
    fs::write(
        &file,
        r#"module t
func main(): int {
    return 0
}
"#,
    )
    .unwrap();
    let out = run_cli(&["run", &file.to_string_lossy(), "--verbose"]);
    assert!(
        out.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[rebuilt:") || stderr.contains("[cached]"),
        "expected status line in stderr, got: {stderr}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
    common::temp_dir(prefix).expect("reserve fresh temporary directory")
}

fn files_named(root: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    collect_files(root, &|path| {
        path.file_name().and_then(|value| value.to_str()) == Some(name)
    })
}

fn files_with_extension(root: &std::path::Path, extension: &str) -> Vec<std::path::PathBuf> {
    collect_files(root, &|path| {
        path.extension().and_then(|value| value.to_str()) == Some(extension)
    })
}

fn collect_files(
    root: &std::path::Path,
    predicate: &dyn Fn(&std::path::Path) -> bool,
) -> Vec<std::path::PathBuf> {
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

#[test]
fn package_check_covers_modules_the_entry_never_calls() {
    // `check` used to lower only what main reached, so type and ownership
    // errors in an unused package module passed silently.
    let tmp = tempfile_dir("arandu_check_all_modules");
    let project = tmp.join("unreached");
    let new_out = run_cli_in(&tmp, &["new", "unreached"]);
    assert!(
        new_out.status.success(),
        "{}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    for (body, code) in [
        (
            "public func typo(): usize {\n    let x: bool = 3\n    return 0\n}\n",
            "T",
        ),
        (
            "import std.alloc.vec as vec\npublic func moved(): usize {\n    let a = vec.Vec.new<int>()\n    let b = a\n    return a.len()\n}\n",
            "O001",
        ),
    ] {
        fs::write(
            project.join("src/helper.aru"),
            format!("module helper\n\n{body}"),
        )
        .unwrap();
        let check = run_cli_in(&project, &["check"]);
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            !check.status.success(),
            "unreached module error was not reported:\n{body}"
        );
        assert!(stderr.contains(code), "expected {code}, got:\n{stderr}");
    }
}
