//! SL_T.5 baseline comparison and portable JUnit reporter contracts.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

mod common;

fn temp_dir() -> std::path::PathBuf {
    common::temp_dir("arandu-slt5").expect("reserve fresh temporary directory")
}

#[test]
fn temporary_directories_are_exclusive_under_parallel_creation() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(32));
    let threads: Vec<_> = (0..32)
        .map(|_| {
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                temp_dir()
            })
        })
        .collect();
    let directories: std::collections::BTreeSet<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(directories.len(), 32);
    for directory in directories {
        fs::remove_dir(directory).unwrap();
    }
}

fn create_project(tmp: &std::path::Path) -> std::path::PathBuf {
    let created = common::cli_command()
        .args(["new", "product_gold", "--vcs=none"])
        .current_dir(tmp)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&created.stdout),
        String::from_utf8_lossy(&created.stderr)
    );
    let project = tmp.join("product_gold");
    fs::write(
        project.join("src/main.aru"),
        r#"module product_gold

import std.testing as testing

@Test
func smoke(): void {
    testing.expect(true, "works")
}

@Benchmark
func integerBarrier(mut bench: testing.Benchmark): void {
    while bench.loop() {
        let value = testing.blackBox<int>(42)
    }
}

func main(): int { return 0 }
"#,
    )
    .unwrap();
    project
}

#[test]
fn junit_report_is_machine_readable_and_keeps_test_identity() {
    let tmp = temp_dir();
    let project = create_project(&tmp);
    let discovered = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--list",
            "--format",
            "json",
        ])
        .output()
        .expect("discover tests");
    assert!(discovered.status.success());
    let discovery: serde_json::Value =
        serde_json::from_slice(&discovered.stdout).expect("discovery JSON");
    assert_eq!(discovery["schema"], "arandu.test-list/v1");
    assert!(
        discovery["cases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|test_case| {
                test_case["id"] == "product_gold::bin::main::smoke"
                    && test_case["path"] == "src/main.aru"
                    && test_case["line"].is_u64()
                    && test_case["column_utf16"].is_u64()
            })
    );
    let report = project.join("target/test-results.xml");
    let output = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--format",
            "junit",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .expect("run tests");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let xml = fs::read_to_string(report).expect("read JUnit report");
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(xml.contains("failures=\"0\" errors=\"0\" skipped=\"0\""));
    assert!(xml.contains("name=\"smoke\""));
    assert!(xml.contains("classname=\"product_gold::bin::main\""));
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn benchmark_baseline_save_compare_and_strict_missing_are_explicit() {
    let tmp = temp_dir();
    let project = create_project(&tmp);
    let id = "product_gold::bin::main::integerBarrier";
    let common_args = [
        "bench",
        project.to_str().unwrap(),
        "--exact",
        id,
        "--warmup",
        "0.001",
        "--measurement-time",
        "0.005",
        "--samples",
        "10",
    ];
    let saved = common::cli_command()
        .args(common_args)
        .args(["--save-baseline", "main"])
        .output()
        .expect("save baseline");
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    let baseline = project.join("target/arandu/benchmarks/main.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&baseline).expect("read baseline"))
            .expect("baseline JSON");
    assert_eq!(document["schema"], "arandu.bench-baseline/v1");
    assert_eq!(document["name"], "main");
    assert!(document["report"]["cpu"].is_string());

    let compared = common::cli_command()
        .args(common_args)
        .args([
            "--compare",
            "main",
            "--max-regression",
            "500",
            "--noise-threshold",
            "500",
            "--format",
            "json",
        ])
        .output()
        .expect("compare baseline");
    assert!(
        compared.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&compared.stdout),
        String::from_utf8_lossy(&compared.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&compared.stdout).expect("comparison JSON");
    assert_eq!(report["comparison"]["baseline"], "main");
    assert_eq!(report["comparison"]["regressions"], 0);

    document["report"]["cases"][0]["median_ns_per_op"] = serde_json::json!(0.000_001);
    document["report"]["cases"][0]["mad_ns_per_op"] = serde_json::json!(0.0);
    fs::write(&baseline, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let dry_run = common::cli_command()
        .args(common_args)
        .args([
            "--compare",
            "main",
            "--max-regression",
            "0",
            "--noise-threshold",
            "0",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("dry-run comparison");
    assert!(dry_run.status.success());
    let dry_report: serde_json::Value =
        serde_json::from_slice(&dry_run.stdout).expect("dry-run JSON");
    assert_eq!(dry_report["comparison"]["regressions"], 1);
    assert_eq!(
        dry_report["comparison"]["cases"][0]["classification"],
        "regressed"
    );

    document["report"]["cpu"] = serde_json::json!("different-machine");
    fs::write(&baseline, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let incompatible = common::cli_command()
        .args(common_args)
        .args(["--compare", "main"])
        .output()
        .expect("incompatible comparison");
    assert_eq!(incompatible.status.code(), Some(4));

    let missing = common::cli_command()
        .args(common_args)
        .args(["--compare", "missing", "--strict"])
        .output()
        .expect("strict missing baseline");
    assert_eq!(missing.status.code(), Some(3));
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn isolated_harness_children_keep_the_coordinator_stdlib_identity() {
    let tmp = temp_dir();
    let project = create_project(&tmp);
    let test = common::cli_command()
        .args(["test", ".", "--format", "json"])
        .current_dir(&project)
        .env_remove("ARANDU_STDLIB")
        .output()
        .expect("run isolated test child outside the repository");
    assert!(
        test.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&test.stdout),
        String::from_utf8_lossy(&test.stderr)
    );

    let benchmark = common::cli_command()
        .args([
            "bench",
            ".",
            "--exact",
            "product_gold::bin::main::integerBarrier",
            "--format",
            "json",
            "--warmup",
            "0.001",
            "--measurement-time",
            "0.005",
            "--samples",
            "10",
        ])
        .current_dir(&project)
        .env_remove("ARANDU_STDLIB")
        .output()
        .expect("run isolated benchmark child outside the repository");
    assert!(
        benchmark.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&benchmark.stdout),
        String::from_utf8_lossy(&benchmark.stderr)
    );
    let _ = fs::remove_dir_all(tmp);
}
