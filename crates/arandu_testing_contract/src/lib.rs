//! Shared test and benchmark result values.
//!
//! This leaf contract depends only on Serde and the standard library so the
//! host runtime never links compiler IR, Salsa registrations, or compiler
//! initializers merely to report a test result. Framing and harness generation
//! remain in `arandu_codegen::testing`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
    TimedOut,
    Crashed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFailure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl TestFailure {
    #[must_use]
    pub fn simple(message: impl Into<String>) -> Self {
        Self {
            operation: None,
            message: message.into(),
            location: None,
            expression: None,
            expected: None,
            actual: None,
            type_name: None,
            cause: None,
        }
    }

    #[must_use]
    pub fn expectation(
        operation: impl Into<String>,
        expression: Option<String>,
        expected: Option<String>,
        actual: Option<String>,
        type_name: Option<String>,
        message: Option<String>,
        location: Option<String>,
    ) -> Self {
        let op_str = operation.into();
        let default_msg = match (&expected, &actual) {
            (Some(exp), Some(act)) => {
                format!("expectation `{op_str}` failed: expected `{exp}`, got `{act}`")
            }
            _ => format!("expectation `{op_str}` failed"),
        };
        Self {
            operation: Some(op_str),
            message: message.unwrap_or(default_msg),
            location,
            expression,
            expected,
            actual,
            type_name,
            cause: None,
        }
    }
}

/// Effective benchmark configuration recorded with every result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkConfigV1 {
    pub warmup_ns: u64,
    pub measurement_ns: u64,
    pub samples: u32,
}

/// One measured batch. Raw integer values are preserved for auditing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkSampleV1 {
    pub iterations: u64,
    pub elapsed_ns: u64,
}

/// Terminal benchmark result emitted by a harness child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkEventV1 {
    pub sequence: u64,
    pub id: String,
    pub config: BenchmarkConfigV1,
    pub samples: Vec<BenchmarkSampleV1>,
    #[serde(default)]
    pub stdout: CapturedOutput,
    #[serde(default)]
    pub stderr: CapturedOutput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_wire_contract_preserves_optional_fields() {
        let failure = TestFailure::simple("mismatch");
        let encoded = serde_json::to_value(&failure).expect("serialize failure");
        assert_eq!(encoded, serde_json::json!({ "message": "mismatch" }));
        let decoded: TestFailure = serde_json::from_value(encoded).expect("deserialize failure");
        assert_eq!(decoded, failure);

        let failure = TestFailure::expectation(
            "expectEqual",
            Some("x".into()),
            Some("1".into()),
            Some("2".into()),
            Some("uint".into()),
            None,
            Some("source.aru:3".into()),
        );
        assert_eq!(
            serde_json::to_value(&failure).expect("serialize expectation"),
            serde_json::json!({
                "operation": "expectEqual",
                "message": "expectation `expectEqual` failed: expected `1`, got `2`",
                "location": "source.aru:3",
                "expression": "x",
                "expected": "1",
                "actual": "2",
                "type_name": "uint"
            })
        );
    }

    #[test]
    fn benchmark_wire_contract_preserves_defaults_and_integer_values() {
        let encoded = serde_json::json!({
            "sequence": 7,
            "id": "pkg::benchmark",
            "config": { "warmup_ns": 100, "measurement_ns": 200, "samples": 1 },
            "samples": [{ "iterations": u64::MAX, "elapsed_ns": 300 }]
        });
        let event: BenchmarkEventV1 =
            serde_json::from_value(encoded).expect("deserialize benchmark");
        assert_eq!(event.stdout, CapturedOutput::default());
        assert_eq!(event.stderr, CapturedOutput::default());
        assert_eq!(event.failure, None);
        assert_eq!(
            serde_json::to_value(event).expect("serialize benchmark"),
            serde_json::json!({
                "sequence": 7,
                "id": "pkg::benchmark",
                "config": { "warmup_ns": 100, "measurement_ns": 200, "samples": 1 },
                "samples": [{ "iterations": u64::MAX, "elapsed_ns": 300 }],
                "stdout": { "bytes": [], "truncated": false },
                "stderr": { "bytes": [], "truncated": false }
            })
        );
    }

    #[test]
    fn test_status_wire_names_remain_stable() {
        for (status, name) in [
            (TestStatus::Passed, "passed"),
            (TestStatus::Failed, "failed"),
            (TestStatus::Skipped, "skipped"),
            (TestStatus::TimedOut, "timed_out"),
            (TestStatus::Crashed, "crashed"),
        ] {
            let value = serde_json::json!(name);
            assert_eq!(
                serde_json::to_value(status).expect("serialize status"),
                value
            );
            assert_eq!(
                serde_json::from_value::<TestStatus>(value).expect("deserialize status"),
                status
            );
        }
    }
}
