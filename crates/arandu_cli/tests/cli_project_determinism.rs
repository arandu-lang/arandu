//! P7-C: semantic package outputs must not depend on paths, time or TOML order.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

mod common;

#[test]
fn equivalent_projects_produce_identical_graph_lock_and_build_metadata() {
    let root = temp_dir("arandu-p7-determinism");
    let first_parent = root.join("first-location");
    let second_parent = root.join("a-much-longer-second-location");
    fs::create_dir_all(&first_parent).unwrap();
    fs::create_dir_all(&second_parent).unwrap();
    let first = first_parent.join("deterministic_gold");
    let second = second_parent.join("deterministic_gold");
    assert_success(
        &cli(&first_parent, &["new", "deterministic_gold"]),
        "first new",
    );
    assert_success(
        &cli(&second_parent, &["new", "deterministic_gold"]),
        "second new",
    );

    // Same typed manifest, deliberately different comments, whitespace and
    // table/key declaration order.
    fs::write(
        second.join("arandu.toml"),
        r#"schema = 1

[policy.effects]
deny = ["UnknownCapability"]
warn_new_resources = true
deny_new_authority = true

[targets.bin]
root = "src/main.aru"
name = "deterministic_gold"

[capabilities]
foreign = false
process = []
environment = []
filesystem_write = []
filesystem_read = []
network = []

[toolchain]
arandu = ">=0.2.0, <0.3.0"

[package]
edition = "2026"
version = "0.0.1"
name = "deterministic_gold"

[dependencies]
"#,
    )
    .unwrap();

    let first_tree = cli_with_variance(&first, &["tree"], "111", "UTC");
    let second_tree = cli_with_variance(&second, &["tree"], "999", "Pacific/Auckland");
    assert_success(&first_tree, "first tree");
    assert_success(&second_tree, "second tree");
    assert_eq!(
        first_tree.stdout, second_tree.stdout,
        "graph output drifted"
    );

    let first_lock = fs::read(first.join("arandu.lock")).unwrap();
    let second_lock = fs::read(second.join("arandu.lock")).unwrap();
    assert_eq!(first_lock, second_lock, "canonical lockfile drifted");
    assert!(
        !first_lock.contains(&b'\r'),
        "lockfile must use LF on every OS"
    );

    let first_build = cli_with_variance(&first, &["build", "--locked"], "111", "UTC");
    let second_build =
        cli_with_variance(&second, &["build", "--locked"], "999", "Pacific/Auckland");
    assert_success(&first_build, "first build");
    assert_success(&second_build, "second build");

    let first_state = only_named(&first.join("target"), "build-state.json");
    let second_state = only_named(&second.join("target"), "build-state.json");
    assert_eq!(
        fs::read(first_state).unwrap(),
        fs::read(second_state).unwrap(),
        "build metadata must exclude absolute paths, clocks and ambient locale"
    );

    fs::remove_dir_all(root).unwrap();
}

fn cli(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run Arandu CLI")
}

fn cli_with_variance(
    dir: &std::path::Path,
    args: &[&str],
    epoch: &str,
    timezone: &str,
) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .env("SOURCE_DATE_EPOCH", epoch)
        .env("TZ", timezone)
        .env("LANG", if epoch == "111" { "C" } else { "pt_BR.UTF-8" })
        .output()
        .expect("run Arandu CLI with varied environment")
}

fn assert_success(output: &std::process::Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn only_named(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|candidate| candidate == name) {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "expected one {name}, found {found:?}");
    found.pop().unwrap()
}

fn temp_dir(prefix: &str) -> std::path::PathBuf {
    common::temp_dir(prefix).expect("reserve fresh temporary directory")
}
