//! Types, options, and structured report models for test and benchmark runners.

use arandu_codegen::testing::{BenchmarkConfigV1, TestFailure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub jobs: usize,
    pub timeout: Duration,
    pub fail_fast: bool,
    pub seed: u64,
    pub format: TestOutputFormat,
    pub output: Option<PathBuf>,
    pub target: Option<String>,
    pub backend: Option<String>,
    pub doc_tests: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutputFormat {
    Human,
    Json,
    Junit,
}

#[derive(Debug, Clone)]
pub struct BenchmarkRunnerOptions {
    pub timeout: Duration,
    pub config: BenchmarkConfigV1,
    pub format_json: bool,
    pub output: Option<PathBuf>,
    pub target: Option<String>,
    pub backend: Option<String>,
    pub baseline: Option<BenchmarkBaselineMode>,
}

#[derive(Debug, Clone)]
pub enum BenchmarkBaselineMode {
    Save {
        name: String,
    },
    Compare {
        name: String,
        strict: bool,
        dry_run: bool,
        max_regression_percent: f64,
        noise_threshold_percent: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkRunOutcome {
    Passed,
    BenchmarkFailed,
    Regression,
    MissingBaseline,
    IncompatibleBaseline,
}

impl BenchmarkRunOutcome {
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Passed => 0,
            Self::BenchmarkFailed | Self::Regression => 1,
            Self::MissingBaseline => 3,
            Self::IncompatibleBaseline => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkJsonReport {
    pub schema: String,
    pub arandu_version: String,
    pub target: String,
    pub backend: String,
    pub profile: String,
    pub clock: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub cases: Vec<BenchmarkJsonCase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<BenchmarkComparisonReport>,
}

impl BenchmarkJsonReport {
    #[must_use]
    pub fn environment(&self) -> BenchmarkEnvironment {
        BenchmarkEnvironment {
            target: self.target.clone(),
            backend: self.backend.clone(),
            profile: self.profile.clone(),
            clock: self.clock.clone(),
            os: self.os.clone(),
            arch: self.arch.clone(),
            cpu: self.cpu.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkEnvironment {
    pub target: String,
    pub backend: String,
    pub profile: String,
    pub clock: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkJsonCase {
    pub id: String,
    pub config: BenchmarkConfigV1,
    pub samples: Vec<arandu_codegen::testing::BenchmarkSampleV1>,
    pub median_ns_per_op: Option<f64>,
    pub mad_ns_per_op: Option<f64>,
    pub p50_ns_per_op: Option<f64>,
    pub p95_ns_per_op: Option<f64>,
    pub min_ns_per_op: Option<f64>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkBaselineFile {
    pub schema: String,
    pub name: String,
    pub report: BenchmarkJsonReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkComparisonReport {
    pub baseline: String,
    pub status: BenchmarkComparisonStatus,
    pub strict: bool,
    pub dry_run: bool,
    pub max_regression_percent: f64,
    pub noise_threshold_percent: f64,
    pub cases: Vec<BenchmarkComparisonCase>,
    pub regressions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkComparisonStatus {
    Compared,
    MissingBaseline,
    IncompatibleEnvironment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkComparisonCase {
    pub id: String,
    pub baseline_median_ns_per_op: f64,
    pub current_median_ns_per_op: f64,
    pub delta_percent: f64,
    pub uncertainty_percent: f64,
    pub effective_threshold_percent: f64,
    pub classification: BenchmarkClassification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkClassification {
    Improved,
    Unchanged,
    Regressed,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonReport<'a> {
    pub schema: &'static str,
    pub target: String,
    pub backend: String,
    pub seed: u64,
    pub jobs: usize,
    pub timeout_ms: u64,
    pub fail_fast: bool,
    pub summary: JsonSummary,
    pub cases: Vec<JsonCase<'a>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct JsonSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub timed_out: usize,
    pub crashed: usize,
    pub duration_ms: u128,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonCase<'a> {
    pub id: &'a str,
    pub status: &'static str,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub failure: Option<TestFailure>,
    pub secondary_failures: Vec<TestFailure>,
    pub logs: Vec<String>,
    pub logs_truncated: bool,
}
