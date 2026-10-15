//! SL_T.3 — `std.testing` expectation and diagnostics integration tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

use crate::common;

fn temp_dir(label: &str) -> std::path::PathBuf {
    common::temp_dir(&format!("arandu-slt3-{label}")).expect("reserve fresh temporary directory")
}

fn create_project(root: &std::path::Path, name: &str, main_src: &str) {
    let created = common::cli_command()
        .args(["new", name, "--vcs=none"])
        .current_dir(root)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let _ = fs::remove_dir_all(root.join(name).join("tests"));
    fs::write(root.join(name).join("src/main.aru"), main_src).unwrap();
}

#[test]
fn expect_passes_and_fails_with_structured_failure() {
    let tmp = temp_dir("expect_basic");
    let proj = tmp.join("expect_basic");
    // Note: `import std.testing as testing` — explicit alias is required.
    // Test functions must be void and discard expect's result.
    let src = "module expect_basic\n\nimport std.testing as testing\n\n\
        @Test\nfunc passes(): void {\n\
            testing.expect(true, \"must be true\")\n\
        }\n\n\
        @Test\nfunc fails(): void {\n\
            testing.expect(false, \"must fail with structured failure\")\n\
        }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "expect_basic", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    // fails test => suite fails => non-zero exit code
    assert_ne!(out.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "expected JSON output on stdout, got: {stdout}"
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["summary"]["failed"], 1);

    let cases = report["cases"].as_array().expect("cases array");
    let fail_case = cases
        .iter()
        .find(|c| c["status"] == "failed")
        .expect("failed case");
    let failure = &fail_case["failure"];
    assert!(!failure.is_null(), "failure must be set for failed test");
    assert_eq!(failure["operation"], "expect");
    assert_eq!(failure["message"], "must fail with structured failure");
    assert_eq!(failure["expected"], "true");
    assert_eq!(failure["actual"], "false");
    assert!(
        failure["location"]
            .as_str()
            .is_some_and(|location| location.split(':').count() == 3),
        "compiler must attach the real call span: {failure}"
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn expect_equal_int_and_str() {
    let tmp = temp_dir("expect_eq");
    let proj = tmp.join("expect_eq");
    let src = "module expect_eq\n\nimport std.testing as testing\n\n\
        @Test\nfunc eq_int_pass(): void {\n\
            testing.expectEqualInt(42, 42, \"integers match\")\n\
        }\n\n\
        @Test\nfunc eq_int_fail(): void {\n\
            testing.expectEqualInt(10, 20, \"integers mismatch\")\n\
        }\n\n\
        @Test\nfunc eq_str_pass(): void {\n\
            testing.expectEqualStr(\"hello\", \"hello\", \"strings match\")\n\
        }\n\n\
        @Test\nfunc eq_str_fail(): void {\n\
            testing.expectEqualStr(\"foo\", \"bar\", \"strings mismatch\")\n\
        }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "expect_eq", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    assert_ne!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "expected JSON output on stdout, got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    assert_eq!(report["summary"]["passed"], 2);
    assert_eq!(report["summary"]["failed"], 2);

    let cases = report["cases"].as_array().expect("cases array");
    let int_fail = cases
        .iter()
        .find(|c| c["id"].as_str().unwrap().contains("eq_int_fail"))
        .unwrap();
    assert_eq!(int_fail["failure"]["expected"], "10");
    assert_eq!(int_fail["failure"]["actual"], "20");

    let str_fail = cases
        .iter()
        .find(|c| c["id"].as_str().unwrap().contains("eq_str_fail"))
        .unwrap();
    assert_eq!(str_fail["failure"]["expected"], "foo");
    assert_eq!(str_fail["failure"]["actual"], "bar");

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn skip_marks_test_as_skipped() {
    let tmp = temp_dir("skip_test");
    let proj = tmp.join("skip_test");
    let src = "module skip_test\n\nimport std.testing as testing\n\n\
        @Test\nfunc skipped_case(): void {\n\
            testing.skip(\"feature not implemented yet on this OS\")\n\
            testing.fail(\"must not replace the skip terminal state\")\n\
        }\n\n\
        @Test\nfunc normal_case(): void {\n\
            testing.expect(true, \"passes\")\n\
        }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "skip_test", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "expected JSON on stdout, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // skipped + passed should NOT fail the overall suite
    let failed = report["summary"]["failed"].as_u64().unwrap_or(0);
    let crashed = report["summary"]["crashed"].as_u64().unwrap_or(0);
    assert_eq!(
        failed + crashed,
        0,
        "no failures expected; report: {report}"
    );
    assert_eq!(report["summary"]["passed"], 1);

    let cases = report["cases"].as_array().expect("cases array");
    let skipped = cases
        .iter()
        .find(|c| c["status"] == "skipped")
        .expect("skip must produce the distinct skipped status");
    assert_eq!(skipped["failure"]["operation"], "skip");
    assert_eq!(
        skipped["failure"]["message"],
        "feature not implemented yet on this OS"
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn explicit_fail_marks_test_failed() {
    let tmp = temp_dir("fail_test");
    let proj = tmp.join("fail_test");
    let src = "module fail_test\n\nimport std.testing as testing\n\n\
        @Test\nfunc explicit_fail(): void {\n\
            testing.fail(\"custom failure reason\")\n\
        }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "fail_test", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    assert_ne!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "expected JSON on stdout, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    assert_eq!(report["summary"]["failed"], 1);
    let cases = report["cases"].as_array().expect("cases array");
    let fail_case = cases
        .iter()
        .find(|c| c["status"] == "failed")
        .expect("failed case");
    assert_eq!(fail_case["failure"]["operation"], "fail");
    assert_eq!(fail_case["failure"]["message"], "custom failure reason");

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn parallel_jobs_isolate_test_expectations() {
    let tmp = temp_dir("parallel_expect");
    let proj = tmp.join("parallel_expect");
    let src = "module parallel_expect\n\nimport std.testing as testing\n\n\
        @Test\nfunc c1(): void { testing.expect(true, \"ok\") }\n\n\
        @Test\nfunc c2(): void { testing.expect(true, \"ok\") }\n\n\
        @Test\nfunc c3(): void { testing.expect(true, \"ok\") }\n\n\
        @Test\nfunc c4(): void { testing.expect(true, \"ok\") }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "parallel_expect", src);

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--jobs",
            "4",
        ])
        .output()
        .expect("run test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "expected JSON on stdout, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        out.status.success(),
        "all expects(true) should pass; report: {report}"
    );
    assert_eq!(report["summary"]["failed"], 0);

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn logs_multiple_failures_and_temp_dir_reach_the_structured_report() {
    let tmp = temp_dir("context_report");
    let proj = tmp.join("context_report");
    let src = "module context_report\n\nimport std.testing as testing\nimport std.path as path\n\n\
        @Test\nfunc context_case(): void {\n\
            testing.log(\"before failures\")\n\
            let temporary = path.Path.from(testing.tempDir(0))\n\
            testing.expect(!temporary.isEmpty(), \"temporary path must be usable\")\n\
            testing.fail(\"primary\")\n\
            testing.fail(\"secondary\")\n\
        }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "context_report", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");
    assert_eq!(out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let case = &report["cases"][0];
    assert_eq!(case["failure"]["message"], "primary");
    assert_eq!(case["secondary_failures"][0]["message"], "secondary");
    assert_eq!(case["logs"][0], "before failures");
    assert_eq!(case["logs_truncated"], false);

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn c_backend_preserves_testing_calls_and_compiler_span_hook() {
    let tmp = temp_dir("c_backend");
    let proj = tmp.join("c_backend");
    let src = "module c_backend\n\nimport std.testing as testing\n\n\
        @Test\nfunc portable_case(): void { testing.expect(false, \"portable\") }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "c_backend", src);
    let out = common::cli_command()
        .args(["emit-c", proj.to_str().unwrap(), "--layout=host"])
        .output()
        .expect("emit C");
    assert!(
        out.status.success(),
        "emit-c failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let emitted = String::from_utf8_lossy(&out.stdout);
    assert!(emitted.contains("ar_test_set_span"));
    assert!(emitted.contains("ar_test_expect"));

    let _ = fs::remove_dir_all(tmp);
}
