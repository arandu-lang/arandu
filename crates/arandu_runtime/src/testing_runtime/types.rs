//! Types, constants, and outcome DTOs for test runtime execution.

use arandu_testing_contract::{TestFailure, TestStatus};

pub const MAX_LOG_ENTRIES: usize = 1000;
pub const MAX_LOG_TOTAL_BYTES: usize = 64 * 1024; // 64 KB
pub const MAX_BENCH_ITERATIONS: u64 = 1 << 50;

/// Result collected after executing a test and running all cleanups.
#[derive(Debug, Clone)]
pub struct TestContextResult {
    pub id: String,
    pub status: TestStatus,
    pub failure: Option<TestFailure>,
    pub secondary_failures: Vec<TestFailure>,
    pub logs: Vec<String>,
    pub logs_truncated: bool,
}
