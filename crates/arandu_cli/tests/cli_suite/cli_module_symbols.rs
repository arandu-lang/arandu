//! SYM.1: equal function names in package modules without a `module`
//! declaration are distinct symbols on the native AOT and JIT paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common;

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
fn same_named_functions_in_undeclared_modules_link_and_run() {
    let tmp = common::temp_dir("arandu_module_symbols").unwrap();
    let init = common::cli_command()
        .args(["new", "app", "--bin", "--vcs=none"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let project = tmp.join("app");
    fs::write(
        project.join("src/main.aru"),
        "import app.left as left\nimport app.right as right\n\nfunc helper(value: int): int {\n    return value + 100\n}\n\nfunc main(): int {\n    return left.api(1) + right.api(2) + helper(0)\n}\n",
    )
    .unwrap();
    // Different signatures under one name used to fail Cranelift with
    // `IncompatibleSignature`; equal ones failed the link with duplicates.
    fs::write(
        project.join("src/left.aru"),
        "func helper(value: int): int {\n    return value + 10\n}\n\npublic func api(value: int): int {\n    return helper(value)\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/right.aru"),
        "func helper(value: int, scale: int): int {\n    return value * scale\n}\n\npublic func api(value: int): int {\n    return helper(value, 3)\n}\n",
    )
    .unwrap();

    let build = common::cli_command()
        .args(["build"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let states = files_named(&project.join("target/dev"), "build-state.json");
    assert_eq!(states.len(), 1);
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&states[0]).unwrap()).unwrap();
    let executable = states[0]
        .parent()
        .unwrap()
        .join(state["artifact"].as_str().unwrap());
    assert_eq!(
        Command::new(&executable).status().unwrap().code(),
        Some(117)
    );

    let run = common::cli_command()
        .args(["run"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert_eq!(
        run.status.code(),
        Some(117),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
