//! Typed holes (RFC 0036, H1): a program that still contains `todo` is
//! checked and reported, and never built, run, tested or evaluated at
//! compile time.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

use crate::common;

fn refuses(output: &std::process::Output, what: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "{what} must refuse a hole: {stderr}"
    );
    assert!(stderr.contains("T102"), "{what} must report T102: {stderr}");
    assert!(
        stderr.contains("typed hole: expected 'int'"),
        "{what} must show the goal: {stderr}"
    );
}

#[test]
fn a_hole_stops_check_run_emit_and_comptime() {
    let directory = common::temp_dir("arandu-typed-holes").expect("temporary directory");
    let file = directory.join("hole.aru");
    fs::write(
        &file,
        "func area(width: int): int {\n    return todo(\"multiply\")\n}\n\nfunc main(): int { return area(2) }\n",
    )
    .unwrap();
    let path = file.to_str().unwrap();
    for args in [
        vec!["check", path],
        vec!["run", path],
        vec!["emit-c", path],
        vec!["emit-wasm", path],
    ] {
        let output = common::cli_command().args(&args).output().expect("run cli");
        refuses(&output, &args[0..1].join(" "));
    }

    let comptime = directory.join("comptime_hole.aru");
    fs::write(
        &comptime,
        "func main(): int {\n    let n: int = comptime { todo }\n    return n\n}\n",
    )
    .unwrap();
    let output = common::cli_command()
        .args(["run", comptime.to_str().unwrap()])
        .output()
        .expect("run cli");
    refuses(&output, "comptime");
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn a_hole_stops_build_and_the_test_runner() {
    let temporary = common::temp_dir("arandu-typed-hole-tests").expect("temporary directory");
    let created = common::cli_command()
        .args(["new", "holes", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let project = temporary.join("holes");
    fs::write(
        project.join("src/main.aru"),
        "module holes\n\nfunc answer(): int { return todo }\n\n@Test\nfunc answersFortyTwo(): void {\n    let value = answer()\n}\n\nfunc main(): int { return 0 }\n",
    )
    .unwrap();
    for command in ["build", "test"] {
        let output = common::cli_command()
            .args([command, project.to_str().unwrap()])
            .output()
            .expect("run cli");
        refuses(&output, command);
    }
    let _ = fs::remove_dir_all(temporary);
}
