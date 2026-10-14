//! Deterministic test-harness registry shared by code-generation frontends.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::time::Duration;

pub const TEST_PROTOCOL_V1: &str = "arandu.test/v1";
pub const BENCH_PROTOCOL_V1: &str = "arandu.bench/v1";
pub const TEST_LIST_PROTOCOL_V1: &str = "arandu.test-list/v1";
pub const BENCH_LIST_PROTOCOL_V1: &str = "arandu.bench-list/v1";
pub const BENCH_BASELINE_PROTOCOL_V1: &str = "arandu.bench-baseline/v1";
pub const FRAME_MAGIC: &[u8; 4] = b"ARND";
pub const MAX_FRAME_PAYLOAD_SIZE: usize = 2 * 1024 * 1024; // 2MB

pub use arandu_testing_contract::{
    BenchmarkConfigV1, BenchmarkEventV1, BenchmarkSampleV1, CapturedOutput, TestFailure, TestStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEventV1 {
    pub sequence: u64,
    pub id: String,
    pub status: TestStatus,
    pub duration: Duration,
    #[serde(default)]
    pub stdout: CapturedOutput,
    #[serde(default)]
    pub stderr: CapturedOutput,
    pub failure: Option<TestFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_failures: Vec<TestFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    #[serde(default)]
    pub logs_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFramePayload {
    pub schema: String,
    pub sequence: u64,
    pub event: TestEventV1,
}

#[derive(Serialize)]
pub struct TestFramePayloadRef<'a> {
    pub schema: &'a str,
    pub sequence: u64,
    pub event: &'a TestEventV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BenchmarkFramePayload {
    schema: String,
    sequence: u64,
    event: BenchmarkEventV1,
}

#[derive(Serialize)]
struct BenchmarkFramePayloadRef<'a> {
    schema: &'a str,
    sequence: u64,
    event: &'a BenchmarkEventV1,
}

/// Write one versioned benchmark event using the SL_T framing contract.
pub fn write_benchmark_frame<W: Write>(
    writer: &mut W,
    sequence: u64,
    event: &BenchmarkEventV1,
) -> Result<(), String> {
    if event.sequence != sequence {
        return Err("benchmark frame sequence mismatch".to_string());
    }
    let bytes = serde_json::to_vec(&BenchmarkFramePayloadRef {
        schema: BENCH_PROTOCOL_V1,
        sequence,
        event,
    })
    .map_err(|error| format!("serialize benchmark frame: {error}"))?;
    if bytes.len() > MAX_FRAME_PAYLOAD_SIZE {
        return Err("benchmark frame exceeds maximum payload size".to_string());
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| "benchmark frame payload size overflow".to_string())?;
    writer
        .write_all(FRAME_MAGIC)
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

/// Read and validate one versioned benchmark event.
pub fn read_benchmark_frame<R: Read>(
    reader: &mut R,
    expected_sequence: u64,
    expected_id: &str,
) -> Result<BenchmarkEventV1, String> {
    let mut magic = [0_u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|error| error.to_string())?;
    if &magic != FRAME_MAGIC {
        return Err("invalid benchmark frame magic".to_string());
    }
    let mut length = [0_u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(|error| error.to_string())?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_| "invalid benchmark frame length".to_string())?;
    if length > MAX_FRAME_PAYLOAD_SIZE {
        return Err("benchmark frame exceeds maximum payload size".to_string());
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    let payload: BenchmarkFramePayload =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if payload.schema != BENCH_PROTOCOL_V1 {
        return Err(format!("unsupported benchmark schema `{}`", payload.schema));
    }
    if payload.sequence != expected_sequence || payload.event.sequence != expected_sequence {
        return Err("benchmark frame sequence mismatch".to_string());
    }
    if payload.event.id != expected_id {
        return Err("benchmark frame id mismatch".to_string());
    }
    Ok(payload.event)
}

/// Encodes a framed protocol event to an IO writer.
///
/// Frame format:
/// - 4 bytes magic: `ARND`
/// - 4 bytes payload length (u32 BE)
/// - JSON payload: `TestFramePayload`
///
/// # Errors
/// Returns error if payload serialization fails or frame size exceeds `MAX_FRAME_PAYLOAD_SIZE`.
pub fn write_frame<W: Write>(
    writer: &mut W,
    sequence: u64,
    event: &TestEventV1,
) -> Result<(), String> {
    if event.sequence != sequence {
        return Err(format!(
            "sequence mismatch: event sequence {} does not match expected frame sequence {sequence}",
            event.sequence
        ));
    }
    let payload = TestFramePayloadRef {
        schema: TEST_PROTOCOL_V1,
        sequence,
        event,
    };
    let json_bytes =
        serde_json::to_vec(&payload).map_err(|err| format!("serialize frame error: {err}"))?;
    if json_bytes.len() > MAX_FRAME_PAYLOAD_SIZE {
        return Err(format!(
            "frame payload size {} exceeds maximum permitted limit {}",
            json_bytes.len(),
            MAX_FRAME_PAYLOAD_SIZE
        ));
    }
    let len_u32 =
        u32::try_from(json_bytes.len()).map_err(|_| "frame payload size overflow".to_string())?;
    writer
        .write_all(FRAME_MAGIC)
        .map_err(|err| format!("write magic error: {err}"))?;
    writer
        .write_all(&len_u32.to_be_bytes())
        .map_err(|err| format!("write length error: {err}"))?;
    writer
        .write_all(&json_bytes)
        .map_err(|err| format!("write payload error: {err}"))?;
    writer
        .flush()
        .map_err(|err| format!("flush frame error: {err}"))?;
    Ok(())
}

/// Decodes and validates a framed protocol event from an IO reader.
///
/// # Errors
/// Returns error on invalid magic, size limit violation, sequence mismatch, or schema mismatch.
pub fn read_frame<R: Read>(
    reader: &mut R,
    expected_sequence: Option<u64>,
    expected_id: Option<&str>,
) -> Result<TestEventV1, String> {
    let mut magic = [0_u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|err| format!("read frame magic error: {err}"))?;
    if &magic != FRAME_MAGIC {
        return Err(format!(
            "invalid frame magic: expected {FRAME_MAGIC:?}, got {magic:?}"
        ));
    }

    let mut len_bytes = [0_u8; 4];
    reader
        .read_exact(&mut len_bytes)
        .map_err(|err| format!("read frame length error: {err}"))?;
    let payload_len = usize::try_from(u32::from_be_bytes(len_bytes))
        .map_err(|_| "invalid payload length".to_string())?;

    if payload_len > MAX_FRAME_PAYLOAD_SIZE {
        return Err(format!(
            "frame payload size {payload_len} exceeds maximum permitted limit {MAX_FRAME_PAYLOAD_SIZE}"
        ));
    }

    let mut payload_bytes = vec![0_u8; payload_len];
    reader
        .read_exact(&mut payload_bytes)
        .map_err(|err| format!("read frame payload error: {err}"))?;

    let frame: TestFramePayload = serde_json::from_slice(&payload_bytes)
        .map_err(|err| format!("invalid frame json payload: {err}"))?;

    if frame.schema != TEST_PROTOCOL_V1 {
        return Err(format!(
            "protocol schema mismatch: expected `{TEST_PROTOCOL_V1}`, got `{}`",
            frame.schema
        ));
    }

    if let Some(exp_seq) = expected_sequence
        && (frame.sequence != exp_seq || frame.event.sequence != exp_seq)
    {
        return Err(format!(
            "sequence mismatch: expected {exp_seq}, got frame sequence {} / event sequence {}",
            frame.sequence, frame.event.sequence
        ));
    }

    if let Some(exp_id) = expected_id
        && frame.event.id != exp_id
    {
        return Err(format!(
            "id mismatch: expected `{exp_id}`, got `{}`",
            frame.event.id
        ));
    }

    Ok(frame.event)
}

/// A compiler-validated test entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEntry {
    pub id: String,
    pub function: String,
}

/// Ordered registry used by harness generators and reporters.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestRegistry {
    entries: BTreeMap<String, TestEntry>,
}

/// Compiler-validated benchmark entry. Separate from [`TestEntry`] because its
/// callable ABI requires a mutable benchmark context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkEntry {
    pub id: String,
    pub function: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BenchmarkRegistry {
    entries: BTreeMap<String, BenchmarkEntry>,
}

impl BenchmarkRegistry {
    pub fn insert(&mut self, entry: BenchmarkEntry) {
        self.entries.insert(entry.id.clone(), entry);
    }

    pub fn iter(&self) -> impl Iterator<Item = &BenchmarkEntry> {
        self.entries.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Emit a portable launcher ABI. The engine initializes the process-local
    /// handle before invoking this launcher in a complete AOT harness.
    #[must_use]
    pub fn emit_c_entrypoint(&self) -> String {
        let mut output = String::from(
            "/* generated by arandu benchmark harness */\n#include <stdint.h>\n\ntypedef struct { int64_t handle; } ArBenchmarkContext;\n",
        );
        for entry in self.iter() {
            output.push_str("extern void ");
            output.push_str(&sanitize_identifier(&entry.function));
            output.push_str("(void *);\n");
        }
        output.push_str("\nint main(void) {\n    ArBenchmarkContext context = { 1 };\n");
        for entry in self.iter() {
            output.push_str("    ");
            output.push_str(&sanitize_identifier(&entry.function));
            output.push_str("(&context);\n");
        }
        output.push_str("    return 0;\n}\n");
        output
    }
}

impl TestRegistry {
    pub fn insert(&mut self, entry: TestEntry) {
        self.entries.insert(entry.id.clone(), entry);
    }

    pub fn get(&self, id: &str) -> Option<&TestEntry> {
        self.entries.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TestEntry> {
        self.entries.values()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Emits the deterministic C shim used by the portable backend.
    #[must_use]
    pub fn emit_c_entrypoint(&self) -> String {
        let mut output =
            String::from("/* generated by arandu test harness */\n#include <stddef.h>\n\n");
        for entry in self.iter() {
            output.push_str("extern void ");
            output.push_str(&sanitize_identifier(&entry.function));
            output.push_str("(void);\n");
        }
        output.push_str("\nint main(void) {\n");
        for entry in self.iter() {
            output.push_str("    ");
            output.push_str(&sanitize_identifier(&entry.function));
            output.push_str("();\n");
        }
        output.push_str("    return 0;\n}\n");
        output
    }
}

fn sanitize_identifier(id: &str) -> String {
    let mut result = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            result.push(byte as char);
        } else {
            result.push('_');
        }
    }
    if result.is_empty() || result.as_bytes()[0].is_ascii_digit() {
        result.insert(0, '_');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn registry_is_deterministic_and_replaces_duplicate_ids() {
        let mut registry = TestRegistry::default();
        registry.insert(TestEntry {
            id: "z".into(),
            function: "zeta".into(),
        });
        registry.insert(TestEntry {
            id: "a".into(),
            function: "alpha".into(),
        });
        registry.insert(TestEntry {
            id: "a".into(),
            function: "updated".into(),
        });
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            registry.get("a").map(|entry| entry.function.as_str()),
            Some("updated")
        );
        assert!(registry.emit_c_entrypoint().contains("updated"));
    }

    #[test]
    fn benchmark_registry_emits_context_aware_c_abi() {
        let mut registry = BenchmarkRegistry::default();
        registry.insert(BenchmarkEntry {
            id: "pkg::bench::work".to_string(),
            function: "work".to_string(),
        });
        let c = registry.emit_c_entrypoint();
        assert!(c.contains("ArBenchmarkContext context = { 1 }"));
        assert!(c.contains("work(&context);"));
        assert!(!c.contains("work();"));
    }

    #[test]
    fn frame_round_trip_validates_sequence_and_magic() {
        let event = TestEventV1 {
            sequence: 42,
            id: "sample::test_foo".into(),
            status: TestStatus::Passed,
            duration: Duration::from_millis(15),
            stdout: CapturedOutput {
                bytes: b"hello stdout".to_vec(),
                truncated: false,
            },
            stderr: CapturedOutput {
                bytes: Vec::new(),
                truncated: false,
            },
            failure: None,
            secondary_failures: Vec::new(),
            logs: Vec::new(),
            logs_truncated: false,
        };

        let mut buffer = Vec::new();
        write_frame(&mut buffer, 42, &event).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let decoded = read_frame(&mut cursor, Some(42), Some("sample::test_foo")).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn read_frame_rejects_invalid_magic() {
        let mut bad_magic = Vec::new();
        bad_magic.extend_from_slice(b"FAIL");
        bad_magic.extend_from_slice(&10_u32.to_be_bytes());
        bad_magic.extend_from_slice(&[0_u8; 10]);

        let mut cursor = Cursor::new(&bad_magic);
        let err = read_frame(&mut cursor, None, None).unwrap_err();
        assert!(err.contains("invalid frame magic"));
    }

    #[test]
    fn read_frame_rejects_excessive_payload_size() {
        let mut overflow_frame = Vec::new();
        overflow_frame.extend_from_slice(FRAME_MAGIC);
        let huge_len = u32::try_from(MAX_FRAME_PAYLOAD_SIZE + 1).unwrap();
        overflow_frame.extend_from_slice(&huge_len.to_be_bytes());

        let mut cursor = Cursor::new(&overflow_frame);
        let err = read_frame(&mut cursor, None, None).unwrap_err();
        assert!(err.contains("exceeds maximum permitted limit"));
    }

    #[test]
    fn framed_protocols_reject_truncated_headers_and_payloads() {
        let test_header = FRAME_MAGIC.to_vec();
        let error = read_frame(&mut Cursor::new(test_header), None, None).unwrap_err();
        assert!(error.contains("read frame length"));

        let mut test_payload = FRAME_MAGIC.to_vec();
        test_payload.extend_from_slice(&16_u32.to_be_bytes());
        test_payload.extend_from_slice(b"short");
        let error = read_frame(&mut Cursor::new(test_payload), None, None).unwrap_err();
        assert!(error.contains("read frame payload"));

        let benchmark_header = FRAME_MAGIC.to_vec();
        assert!(read_benchmark_frame(&mut Cursor::new(benchmark_header), 0, "id").is_err());
        let mut benchmark_payload = FRAME_MAGIC.to_vec();
        benchmark_payload.extend_from_slice(&16_u32.to_be_bytes());
        benchmark_payload.extend_from_slice(b"short");
        assert!(read_benchmark_frame(&mut Cursor::new(benchmark_payload), 0, "id").is_err());
    }

    #[test]
    fn read_frame_rejects_sequence_mismatch_and_unknown_status() {
        let event = TestEventV1 {
            sequence: 10,
            id: "sample::test".into(),
            status: TestStatus::Failed,
            duration: Duration::from_millis(5),
            stdout: CapturedOutput {
                bytes: Vec::new(),
                truncated: false,
            },
            stderr: CapturedOutput {
                bytes: Vec::new(),
                truncated: false,
            },
            failure: Some(TestFailure::simple("failed assertion")),
            secondary_failures: Vec::new(),
            logs: Vec::new(),
            logs_truncated: false,
        };

        let mut buffer = Vec::new();
        write_frame(&mut buffer, 10, &event).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let err = read_frame(&mut cursor, Some(99), None).unwrap_err();
        assert!(err.contains("sequence mismatch"));

        // Unknown status string test:
        let invalid_status_json = r#"{"schema":"arandu.test/v1","sequence":1,"event":{"sequence":1,"id":"a","status":"unknown_status_kind","duration":{"secs":0,"nanos":0},"stdout":{"bytes":[],"truncated":false},"stderr":{"bytes":[],"truncated":false},"failure":null}}"#;
        let mut raw_bad_status = Vec::new();
        raw_bad_status.extend_from_slice(FRAME_MAGIC);
        let len = u32::try_from(invalid_status_json.len()).unwrap();
        raw_bad_status.extend_from_slice(&len.to_be_bytes());
        raw_bad_status.extend_from_slice(invalid_status_json.as_bytes());

        let mut cursor = Cursor::new(&raw_bad_status);
        let err = read_frame(&mut cursor, None, None).unwrap_err();
        assert!(err.contains("invalid frame json payload") || err.contains("unknown variant"));
    }

    #[test]
    fn benchmark_frame_round_trip_preserves_raw_samples() {
        let event = BenchmarkEventV1 {
            sequence: 7,
            id: "pkg::bin::bench".to_string(),
            config: BenchmarkConfigV1 {
                warmup_ns: 1_000,
                measurement_ns: 10_000,
                samples: 2,
            },
            samples: vec![
                BenchmarkSampleV1 {
                    iterations: 100,
                    elapsed_ns: 2_000,
                },
                BenchmarkSampleV1 {
                    iterations: 100,
                    elapsed_ns: 2_100,
                },
            ],
            stdout: CapturedOutput::default(),
            stderr: CapturedOutput::default(),
            failure: None,
        };
        let mut bytes = Vec::new();
        write_benchmark_frame(&mut bytes, 7, &event).unwrap();
        let decoded = read_benchmark_frame(&mut Cursor::new(bytes), 7, "pkg::bin::bench").unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn benchmark_frame_rejects_size_schema_sequence_and_id_confusion() {
        let mut oversized = FRAME_MAGIC.to_vec();
        let length = u32::try_from(MAX_FRAME_PAYLOAD_SIZE + 1).unwrap();
        oversized.extend_from_slice(&length.to_be_bytes());
        let error = read_benchmark_frame(&mut Cursor::new(oversized), 0, "id").unwrap_err();
        assert!(error.contains("maximum payload size"));

        let event = BenchmarkEventV1 {
            sequence: 7,
            id: "pkg::bench".to_string(),
            config: BenchmarkConfigV1 {
                warmup_ns: 1,
                measurement_ns: 2,
                samples: 10,
            },
            samples: Vec::new(),
            stdout: CapturedOutput::default(),
            stderr: CapturedOutput::default(),
            failure: None,
        };
        let mut valid = Vec::new();
        write_benchmark_frame(&mut valid, 7, &event).unwrap();
        assert!(
            read_benchmark_frame(&mut Cursor::new(&valid), 8, "pkg::bench")
                .unwrap_err()
                .contains("sequence")
        );
        assert!(
            read_benchmark_frame(&mut Cursor::new(&valid), 7, "pkg::other")
                .unwrap_err()
                .contains("id mismatch")
        );

        let payload = BenchmarkFramePayload {
            schema: "arandu.bench/v2".to_string(),
            sequence: 7,
            event,
        };
        let encoded = serde_json::to_vec(&payload).unwrap();
        let mut unknown = FRAME_MAGIC.to_vec();
        unknown.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
        unknown.extend_from_slice(&encoded);
        assert!(
            read_benchmark_frame(&mut Cursor::new(unknown), 7, "pkg::bench")
                .unwrap_err()
                .contains("unsupported benchmark schema")
        );
    }
}
