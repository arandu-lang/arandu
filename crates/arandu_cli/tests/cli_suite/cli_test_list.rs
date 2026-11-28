//! SL_T.0 end-to-end discovery through the public project CLI.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

use crate::common;

fn temporary_directory() -> std::path::PathBuf {
    common::temp_dir("arandu-test-list").expect("reserve fresh temporary directory")
}

#[test]
fn test_list_uses_package_qualified_deterministic_ids() {
    let temporary = temporary_directory();
    let project = temporary.join("sample");
    let created = common::cli_command()
        .args(["new", "sample", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    fs::write(
        project.join("src/main.aru"),
        "module sample\n\n@Test\nfunc doctest_0(): void {}\n\n@Test\nfunc sourceCase(): void {}\n\n@Test\nfunc resultCase(): Result<void, Err> { return nil }\n\nfunc main(): int { return 0 }\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/smoke.aru"),
        "module sample_tests\n\n@Test\nfunc smoke(): void {}\n",
    )
    .unwrap();

    let listed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--list"])
        .output()
        .expect("list tests");
    assert!(
        listed.status.success(),
        "test --list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "sample::bin::main::doctest_0\nsample::bin::main::resultCase\nsample::bin::main::sourceCase\nsample::test::smoke::smoke\n"
    );
    let selected = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--list",
            "--exact",
            "sample::test::smoke::smoke",
        ])
        .output()
        .expect("select test");
    assert!(selected.status.success());
    assert_eq!(
        String::from_utf8_lossy(&selected.stdout),
        "sample::test::smoke::smoke\n"
    );
    let executed = common::cli_command()
        .args(["test", project.to_str().unwrap()])
        .output()
        .expect("run tests");
    assert!(
        executed.status.success(),
        "test run failed: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert!(String::from_utf8_lossy(&executed.stderr).contains("passed sample::"));
    let profile_root = project.join("target/dev");
    let harness_pointers = fs::read_dir(&profile_root)
        .expect("read target profile")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("test-harness.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert!(
        harness_pointers.len() == 1,
        "expected one host-specific test harness manifest below {}, found {:?}",
        profile_root.display(),
        harness_pointers
    );
    for policy in ["--locked", "--offline", "--frozen"] {
        let checked = common::cli_command()
            .args([policy, "test", project.to_str().unwrap(), "--list"])
            .output()
            .expect("run policy test");
        assert!(
            checked.status.success(),
            "{policy} test failed: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }
    let json = common::cli_command()
        .args([
            "--release",
            "test",
            project.to_str().unwrap(),
            "--format",
            "json",
            "--jobs",
            "2",
            "--seed",
            "42",
            "--timeout",
            "30",
        ])
        .output()
        .expect("run JSON test reporter");
    assert!(
        json.status.success(),
        "JSON run failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["schema"], "arandu.test/v1");
    assert_eq!(report["seed"], 42);
    assert_eq!(report["backend"], "cranelift-dev");
    assert_eq!(report["cases"].as_array().map(Vec::len), Some(4));
    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn test_child_reuses_project_database_for_source_imports() {
    let temporary = temporary_directory();
    let project = temporary.join("imported_test");
    let created = common::cli_command()
        .args(["new", "imported_test", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    fs::write(
        project.join("src/helper.aru"),
        "module imported_test.helper\n\npublic func answer(): int { return 42 }\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/smoke.aru"),
        "module imported_test_tests\n\nimport std.testing as testing\nimport self.helper as helper\n\n@Test\nfunc importsSourceModule(): void { testing.expectEqualInt(42, helper.answer(), \"source import\") }\n",
    )
    .unwrap();

    let executed = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--exact",
            "imported_test::test::smoke::importsSourceModule",
        ])
        .output()
        .expect("run imported test");
    assert!(
        executed.status.success(),
        "imported test failed: stdout={} stderr={}",
        String::from_utf8_lossy(&executed.stdout),
        String::from_utf8_lossy(&executed.stderr)
    );

    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn test_list_rejects_an_invalid_test_contract() {
    let temporary = temporary_directory();
    let project = temporary.join("invalid_case");
    let created = common::cli_command()
        .args(["new", "invalid_case", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(created.status.success());
    fs::write(
        project.join("tests/smoke.aru"),
        "module invalid_case_tests\n\n@Test\nfunc smoke(value: int): void {}\n",
    )
    .unwrap();

    let listed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--list"])
        .output()
        .expect("list tests");
    assert_eq!(listed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&listed.stderr).contains("T036"));
    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn runner_classifies_result_error_and_timeout() {
    let temporary = temporary_directory();
    let project = temporary.join("runner_failures");
    let created = common::cli_command()
        .args(["new", "runner_failures", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(created.status.success());
    fs::write(
        project.join("tests/smoke.aru"),
        "module runner_failures_tests\n\nimport err\n\n@Test\nfunc fails(): Result<void, Err> { return Result.Err(err.new(\"boom\")) }\n",
    )
    .unwrap();
    let failed = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--format",
            "json",
            "--exact",
            "runner_failures::test::smoke::fails",
        ])
        .output()
        .expect("run failing test");
    assert_eq!(failed.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["cases"][0]["status"], "failed");

    fs::write(
        project.join("tests/smoke.aru"),
        "module runner_failures_tests\n\n@Test\nfunc hangs(): void { while true {} }\n",
    )
    .unwrap();
    let timed_out = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--format",
            "json",
            "--timeout",
            "1",
            "--exact",
            "runner_failures::test::smoke::hangs",
        ])
        .output()
        .expect("run timed out test");
    assert_eq!(timed_out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&timed_out.stdout).unwrap();
    assert_eq!(report["cases"][0]["status"], "timed_out");
    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn test_doc_discovers_and_executes_doctest_blocks() {
    let temporary = temporary_directory();
    let project = temporary.join("doctest_sample");
    let created = common::cli_command()
        .args(["new", "doctest_sample", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(created.status.success());

    fs::write(
        project.join("src/main.aru"),
        concat!(
            "module doctest_sample\n\n",
            "import std.testing as testing\n\n",
            "/// Doubles an integer.\n",
            "///\n",
            "/// # Examples\n",
            "/// ```arandu\n",
            "/// testing.expectEqualInt(10, double(5), \"double five\")\n",
            "/// ```\n",
            "public func double(x: int): int { return x * 2 }\n\n",
            "func main(): int { return 0 }\n",
        ),
    )
    .unwrap();

    let listed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--doc", "--list"])
        .output()
        .expect("list doctests");
    assert!(
        listed.status.success(),
        "test --doc --list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("doctest_sample::bin::main::doctest_0\n"),
        "expected doctest_0 in --list output, got: {}",
        String::from_utf8_lossy(&listed.stdout)
    );

    let executed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--doc"])
        .output()
        .expect("run doctests");
    assert!(
        executed.status.success(),
        "test --doc failed: stdout={} stderr={}",
        String::from_utf8_lossy(&executed.stdout),
        String::from_utf8_lossy(&executed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&executed.stderr)
            .contains("passed doctest_sample::bin::main::doctest_0")
    );

    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn doctests_do_not_replace_source_tests_or_collide_with_ordinary_functions() {
    let temporary = temporary_directory();
    let project = temporary.join("doc_collisions");
    let created = common::cli_command()
        .args(["new", "doc_collisions", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .unwrap();
    assert!(created.status.success());
    fs::remove_file(project.join("tests/smoke.aru")).unwrap();
    fs::write(
        project.join("src/main.aru"),
        concat!(
            "module doc_collisions\n",
            "import std.testing as testing\n",
            "@Test\nfunc doctest_0(): void {}\n",
            "func doctest_0_(): void {}\n",
            "/// # Examples\n",
            "/// ```arandu\n",
            "/// testing.expectEqualInt(42, answer(), \"answer\")\n",
            "/// ```\n",
            "public func answer(): int { return 42 }\n",
            "func main(): int { return 0 }\n",
        ),
    )
    .unwrap();
    let listed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--doc", "--list"])
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "doc_collisions::bin::main::doctest_0\ndoc_collisions::bin::main::doctest_0__\n"
    );
    let executed = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--doc",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["cases"].as_array().unwrap().len(), 2);
    assert!(
        report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case["status"] == "passed")
    );
    let selected = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--doc",
            "--exact",
            "doc_collisions::bin::main::doctest_0__",
        ])
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn human_reporter_streams_progress_shows_failure_output_and_prints_summary_footer() {
    let temporary = temporary_directory();
    let project = temporary.join("human_stream");
    let created = common::cli_command()
        .args(["new", "human_stream", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(created.status.success());
    fs::write(
        project.join("tests/smoke.aru"),
        concat!(
            "module human_stream_tests\n\n",
            "import std.io as io\n",
            "import std.testing as testing\n\n",
            "@Test\n",
            "func ok_case(): void {\n",
            "    io.println(\"hidden-pass-stdout\")\n",
            "    testing.expectEqualInt(2, 1 + 1, \"math works\")\n",
            "}\n\n",
            "@Test\n",
            "func fail_case(): void {\n",
            "    io.println(\"visible-fail-stdout\")\n",
            "    testing.expectEqualInt(10, 99, \"wrong total\")\n",
            "}\n\n",
            "@Test\n",
            "func crash_case(): void {\n",
            "    let none: Option<int> = Option.None\n",
            "    let _ = none.expectOrAbort(\"visible-crash-stderr\")\n",
            "}\n",
        ),
    )
    .unwrap();

    let executed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--jobs", "1"])
        .output()
        .expect("run human tests");
    assert_eq!(executed.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&executed.stderr);
    assert!(
        stderr.contains("running 3 tests"),
        "expected run header in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("[1/3] ") && stderr.contains("[2/3] ") && stderr.contains("[3/3] "),
        "expected [done/total] streaming progress in stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("hidden-pass-stdout"),
        "passing test stdout should stay hidden in human output:\n{stderr}"
    );
    assert!(
        stderr.contains("--- stdout ---") && stderr.contains("visible-fail-stdout"),
        "failing test stdout should be shown in human output:\n{stderr}"
    );
    assert!(
        stderr.contains("--- stderr ---") && stderr.contains("visible-crash-stderr"),
        "crashing test stderr should be shown in human output:\n{stderr}"
    );
    assert!(
        stderr.contains("failures:\n")
            && stderr.contains("failed human_stream::test::smoke::fail_case")
            && stderr.contains("crashed human_stream::test::smoke::crash_case"),
        "expected consolidated failures footer in stderr:\n{stderr}"
    );
    assert!(
        stderr
            .contains("test result: FAILED. 1 passed; 1 failed; 0 skipped; 0 timed out; 1 crashed"),
        "expected final test result summary in stderr:\n{stderr}"
    );

    let _ = fs::remove_dir_all(temporary);
}
