//! SL_T.6 black-box verification for an installed Arandu SDK.

use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TEST_LIST_SCHEMA: &str = "arandu.test-list/v1";
const TEST_SCHEMA: &str = "arandu.test/v1";
const BENCH_LIST_SCHEMA: &str = "arandu.bench-list/v1";
const BENCH_SCHEMA: &str = "arandu.bench/v1";
const BASELINE_SCHEMA: &str = "arandu.bench-baseline/v1";

pub fn check(root: &Path, args: impl Iterator<Item = String>) -> i32 {
    match Config::parse(args).and_then(|config| run(root, &config)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("check-slt6-sdk: error: {error}");
            1
        }
    }
}

struct Config {
    arandu: PathBuf,
    work_dir: PathBuf,
    evidence_dir: PathBuf,
    expected_version: Option<String>,
}

impl Config {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut arandu = None;
        let mut work_dir = None;
        let mut evidence_dir = None;
        let mut expected_version = None;
        while let Some(argument) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("{argument} requires a value"))?;
            match argument.as_str() {
                "--arandu" => arandu = Some(PathBuf::from(value)),
                "--work-dir" => work_dir = Some(PathBuf::from(value)),
                "--evidence-dir" => evidence_dir = Some(PathBuf::from(value)),
                "--expected-version" => expected_version = Some(value),
                _ => return Err(format!("unknown argument `{argument}`")),
            }
        }
        Ok(Self {
            arandu: arandu.ok_or("missing --arandu")?,
            work_dir: work_dir.ok_or("missing --work-dir")?,
            evidence_dir: evidence_dir.ok_or("missing --evidence-dir")?,
            expected_version,
        })
    }
}

fn run(root: &Path, config: &Config) -> Result<(), String> {
    let arandu = fs::canonicalize(&config.arandu).map_err(|error| {
        format!(
            "cannot resolve installed CLI {}: {error}",
            config.arandu.display()
        )
    })?;
    if !arandu.is_file() {
        return Err(format!("installed CLI is not a file: {}", arandu.display()));
    }

    replace_directory(&config.work_dir)?;
    fs::create_dir_all(&config.evidence_dir).map_err(|error| {
        format!(
            "create evidence directory {}: {error}",
            config.evidence_dir.display()
        )
    })?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("canonicalize workspace {}: {error}", root.display()))?;
    let canonical_work = fs::canonicalize(&config.work_dir).map_err(|error| {
        format!(
            "canonicalize work directory {}: {error}",
            config.work_dir.display()
        )
    })?;
    if canonical_work.starts_with(&canonical_root) {
        return Err("--work-dir must be outside the repository".into());
    }

    let version = invoke(&arandu, &config.work_dir, ["--version"])?;
    let version_text = stdout_text(&version, "read version")?;
    if let Some(expected) = &config.expected_version {
        if !version_text.contains(expected) {
            return Err(format!(
                "installed CLI version `{}` does not contain expected `{expected}`",
                version_text.trim()
            ));
        }
    }

    successful(
        invoke(
            &arandu,
            &config.work_dir,
            ["new", "slt6_gold", "--vcs=none"],
        )?,
        "create project",
    )?;
    let project = config.work_dir.join("slt6_gold");
    fs::write(project.join("tests/smoke.aru"), gold_source())
        .map_err(|error| format!("write Gold fixture in {}: {error}", project.display()))?;

    for arguments in [vec!["check"], vec!["build"], vec!["run"]] {
        successful(invoke(&arandu, &project, arguments)?, "project lifecycle")?;
    }

    let test_list = json_output(
        invoke(
            &arandu,
            &project,
            ["test", ".", "--list", "--format", "json"],
        )?,
        TEST_LIST_SCHEMA,
        "test discovery",
    )?;
    require_case(&test_list, "slt6_gold::test::smoke::smoke", "test")?;
    write_json(config, "test-list.json", &test_list)?;

    let serial = test_run(&arandu, &project, 1)?;
    let parallel = test_run(&arandu, &project, 4)?;
    if normalized_test_report(serial.clone()) != normalized_test_report(parallel.clone()) {
        return Err("test results differ between --jobs 1 and --jobs 4".into());
    }
    write_json(config, "test-serial.json", &serial)?;
    write_json(config, "test-parallel.json", &parallel)?;

    let junit = config.evidence_dir.join("test-results.xml");
    successful(
        invoke_os(
            &arandu,
            &project,
            [
                OsString::from("test"),
                OsString::from("."),
                OsString::from("--format"),
                OsString::from("junit"),
                OsString::from("--output"),
                junit.as_os_str().to_owned(),
            ],
        )?,
        "JUnit reporter",
    )?;
    let xml =
        fs::read_to_string(&junit).map_err(|error| format!("read {}: {error}", junit.display()))?;
    if !xml.contains("testsuite") || !xml.contains("failures=\"0\"") {
        return Err("JUnit evidence is missing a successful testsuite".into());
    }

    let bench_list = json_output(
        invoke(
            &arandu,
            &project,
            ["bench", ".", "--list", "--format", "json"],
        )?,
        BENCH_LIST_SCHEMA,
        "benchmark discovery",
    )?;
    let benchmark_id = "slt6_gold::test::smoke::integerBarrier";
    require_case(&bench_list, benchmark_id, "benchmark")?;
    write_json(config, "bench-list.json", &bench_list)?;

    let mut benchmark_arguments = benchmark_args(benchmark_id);
    benchmark_arguments.extend(["--save-baseline", "gold"]);
    let saved = json_output(
        invoke(&arandu, &project, benchmark_arguments)?,
        BENCH_SCHEMA,
        "save benchmark baseline",
    )?;
    write_json(config, "bench-save.json", &saved)?;
    let baseline_path = project.join("target/arandu/benchmarks/gold.json");
    let baseline: Value = serde_json::from_slice(
        &fs::read(&baseline_path)
            .map_err(|error| format!("read {}: {error}", baseline_path.display()))?,
    )
    .map_err(|error| format!("parse benchmark baseline: {error}"))?;
    require_schema(&baseline, BASELINE_SCHEMA, "benchmark baseline")?;
    write_json(config, "bench-baseline.json", &baseline)?;

    let mut compare_arguments = benchmark_args(benchmark_id);
    compare_arguments.extend(["--compare", "gold", "--max-regression", "100", "--dry-run"]);
    let compared = json_output(
        invoke(&arandu, &project, compare_arguments)?,
        BENCH_SCHEMA,
        "compare benchmark baseline",
    )?;
    // Preserve the failing comparison too; it is essential for separating
    // benchmark/environment noise from protocol and SDK failures.
    write_json(config, "bench-compare.json", &compared)?;
    if compared
        .pointer("/comparison/regressions")
        .and_then(Value::as_u64)
        != Some(0)
    {
        return Err("A/A benchmark exceeded the 100% Gold smoke threshold".into());
    }

    let manifest = json!({
        "schema": "arandu.slt6-evidence/v1",
        "version": version_text.trim(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "sdk_outside_repository": true,
        "test_protocol": TEST_SCHEMA,
        "benchmark_protocol": BENCH_SCHEMA,
        "checks": [
            "new", "check", "build", "run", "test-list", "test-jobs-1-vs-4",
            "junit", "bench-list", "bench-save", "bench-compare-aa"
        ]
    });
    write_json(config, "manifest.json", &manifest)?;
    println!(
        "check-slt6-sdk: ok ({} {}, evidence={})",
        std::env::consts::OS,
        std::env::consts::ARCH,
        config.evidence_dir.display()
    );
    Ok(())
}

fn test_run(arandu: &Path, project: &Path, jobs: usize) -> Result<Value, String> {
    let jobs = jobs.to_string();
    json_output(
        invoke(
            arandu,
            project,
            [
                "test", ".", "--format", "json", "--jobs", &jobs, "--seed", "1787",
            ],
        )?,
        TEST_SCHEMA,
        "test execution",
    )
}

fn benchmark_args(id: &str) -> Vec<&str> {
    vec![
        "bench",
        ".",
        "--exact",
        id,
        "--format",
        "json",
        "--warmup",
        // Millisecond-scale samples were dominated by hosted-runner noise.
        // Give both identical-binary runs the same substantial warmup/window.
        "0.05",
        "--measurement-time",
        "0.1",
        "--samples",
        "10",
    ]
}

fn gold_source() -> &'static str {
    r#"module slt6_gold_tests

import std.testing as testing

@Test
func smoke(): void {
    testing.expect(true, "SL_T.6 installed SDK")
}

@Benchmark
func integerBarrier(mut bench: testing.Benchmark): void {
    while bench.loop() {
        let value = testing.blackBox<int>(42)
    }
}
"#
}

fn invoke<'a>(
    arandu: &Path,
    current_dir: &Path,
    arguments: impl IntoIterator<Item = &'a str>,
) -> Result<Output, String> {
    invoke_os(
        arandu,
        current_dir,
        arguments.into_iter().map(OsString::from),
    )
}

fn invoke_os(
    arandu: &Path,
    current_dir: &Path,
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Output, String> {
    let mut command = Command::new(arandu);
    command
        .args(arguments)
        .current_dir(current_dir)
        .env("ARANDU_DAEMON", "0")
        .env_remove("ARANDU_STDLIB")
        .env_remove("ARANDU_RUNTIME_LIB")
        .env_remove("ARANDU_LINKER")
        .env_remove("ARANDU_BENCH_MACHINE")
        .env("NO_COLOR", "1");
    command
        .output()
        .map_err(|error| format!("launch {}: {error}", arandu.display()))
}

fn successful(output: Output, operation: &str) -> Result<Output, String> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{operation} failed (status={}): stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn stdout_text(output: &Output, operation: &str) -> Result<String, String> {
    if !output.status.success() {
        return Err(format!(
            "{operation} failed (status={}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{operation} returned non-UTF-8 stdout: {error}"))
}

fn json_output(output: Output, schema: &str, operation: &str) -> Result<Value, String> {
    let output = successful(output, operation)?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{operation} returned invalid JSON: {error}"))?;
    require_schema(&value, schema, operation)?;
    Ok(value)
}

fn require_schema(value: &Value, schema: &str, operation: &str) -> Result<(), String> {
    match value.get("schema").and_then(Value::as_str) {
        Some(actual) if actual == schema => Ok(()),
        actual => Err(format!(
            "{operation} schema mismatch: expected `{schema}`, got {actual:?}"
        )),
    }
}

fn require_case(value: &Value, id: &str, kind: &str) -> Result<(), String> {
    let present = value
        .get("cases")
        .and_then(Value::as_array)
        .is_some_and(|cases| {
            cases
                .iter()
                .any(|case| case.get("id") == Some(&Value::String(id.into())))
        });
    if present {
        Ok(())
    } else {
        Err(format!("{kind} discovery did not contain `{id}`"))
    }
}

fn normalized_test_report(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("jobs");
        if let Some(summary) = object.get_mut("summary").and_then(Value::as_object_mut) {
            summary.remove("duration_ms");
        }
        if let Some(cases) = object.get_mut("cases").and_then(Value::as_array_mut) {
            for case in cases {
                if let Some(case) = case.as_object_mut() {
                    case.remove("duration_ms");
                }
            }
        }
    }
    value
}

fn write_json(config: &Config, name: &str, value: &Value) -> Result<(), String> {
    let path = config.evidence_dir.join(name);
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

fn replace_directory(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| format!("clear {}: {error}", path.display()))?;
    }
    fs::create_dir_all(path).map_err(|error| format!("create {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_removes_only_declared_volatile_test_fields() {
        let left = json!({
            "schema": TEST_SCHEMA,
            "jobs": 1,
            "seed": 7,
            "summary": { "passed": 1, "duration_ms": 2 },
            "cases": [{ "id": "p::smoke", "status": "passed", "duration_ms": 2 }]
        });
        let right = json!({
            "schema": TEST_SCHEMA,
            "jobs": 8,
            "seed": 7,
            "summary": { "passed": 1, "duration_ms": 20 },
            "cases": [{ "id": "p::smoke", "status": "passed", "duration_ms": 20 }]
        });
        assert_eq!(normalized_test_report(left), normalized_test_report(right));
    }

    #[test]
    fn normalization_keeps_semantic_differences() {
        let passed = json!({
            "schema": TEST_SCHEMA,
            "cases": [{ "id": "p::smoke", "status": "passed", "duration_ms": 1 }]
        });
        let failed = json!({
            "schema": TEST_SCHEMA,
            "cases": [{ "id": "p::smoke", "status": "failed", "duration_ms": 1 }]
        });
        assert_ne!(
            normalized_test_report(passed),
            normalized_test_report(failed)
        );
    }
}
