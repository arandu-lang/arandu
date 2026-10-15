//! Active test execution context and lifecycle management.

use std::cell::RefCell;
use std::path::PathBuf;

use arandu_testing_contract::{TestFailure, TestStatus};

use super::sandbox::safe_cleanup_temp_dir;
use super::types::{MAX_LOG_ENTRIES, MAX_LOG_TOTAL_BYTES, TestContextResult};

pub(crate) type CleanupFn = Box<dyn FnOnce() + Send>;

/// Active test execution context.
pub struct TestContext {
    pub id: String,
    pub seed: u64,
    pub status: TestStatus,
    pub failure: Option<TestFailure>,
    pub secondary_failures: Vec<TestFailure>,
    pub skip_reason: Option<String>,
    pub logs: Vec<String>,
    pub log_bytes: usize,
    pub logs_truncated: bool,
    pub cleanups: Vec<CleanupFn>,
    pub temp_dirs: Vec<PathBuf>,
    pub temp_root: PathBuf,
    pub current_location: Option<String>,
}

impl TestContext {
    pub fn new(id: String, seed: u64, temp_root: PathBuf) -> Self {
        Self {
            id,
            seed,
            status: TestStatus::Passed,
            failure: None,
            secondary_failures: Vec::new(),
            skip_reason: None,
            logs: Vec::new(),
            log_bytes: 0,
            logs_truncated: false,
            cleanups: Vec::new(),
            temp_dirs: Vec::new(),
            temp_root,
            current_location: None,
        }
    }

    pub fn record_failure(&mut self, failure: TestFailure) {
        if self.status == TestStatus::Skipped {
            return;
        }
        if self.failure.is_none() {
            self.status = TestStatus::Failed;
            self.failure = Some(failure);
        } else {
            self.secondary_failures.push(failure);
        }
    }

    pub fn record_skip(&mut self, reason: String, location: Option<String>) {
        if self.status == TestStatus::Passed {
            self.status = TestStatus::Skipped;
            self.skip_reason = Some(reason.clone());
            self.failure = Some(TestFailure {
                operation: Some("skip".to_string()),
                message: reason,
                location,
                expression: None,
                expected: None,
                actual: None,
                type_name: None,
                cause: None,
            });
        }
    }

    pub fn log(&mut self, msg: String) {
        if self.logs.len() >= MAX_LOG_ENTRIES || self.log_bytes + msg.len() > MAX_LOG_TOTAL_BYTES {
            self.logs_truncated = true;
            return;
        }
        self.log_bytes += msg.len();
        self.logs.push(msg);
    }

    pub fn add_cleanup(&mut self, cleanup: impl FnOnce() + Send + 'static) {
        self.cleanups.push(Box::new(cleanup));
    }
}

thread_local! {
    static ACTIVE_CONTEXT: RefCell<Option<TestContext>> = const { RefCell::new(None) };
}

pub(crate) fn with_active_context<R>(f: impl FnOnce(&mut TestContext) -> R) -> Option<R> {
    ACTIVE_CONTEXT.with(|cell| cell.borrow_mut().as_mut().map(f))
}

/// Records the source span of the next `std.testing` operation. The compiler
/// emits this call immediately before the public testing helper call.
#[unsafe(no_mangle)]
pub extern "C" fn ar_test_set_span(file_id: i64, start: i64, end: i64) {
    with_active_context(|ctx| {
        ctx.current_location = Some(format!("{file_id}:{start}:{end}"));
    });
}

pub(crate) fn effective_location(location: Option<String>) -> Option<String> {
    if location.is_some() {
        return location;
    }
    with_active_context(|ctx| ctx.current_location.take()).flatten()
}

/// Initializes the test context for a given test case before execution.
pub fn init_test_context(id: &str, seed: u64, temp_root: Option<PathBuf>) {
    let root = temp_root.unwrap_or_else(|| {
        let default_dir =
            std::env::temp_dir().join(format!("arandu-test-ctx-{}", std::process::id()));
        if let Err(err) = std::fs::create_dir_all(&default_dir) {
            eprintln!(
                "failed to create test context dir {}: {err}",
                default_dir.display()
            );
        }
        default_dir
    });
    let ctx = TestContext::new(id.to_string(), seed, root);
    ACTIVE_CONTEXT.with(|cell| {
        *cell.borrow_mut() = Some(ctx);
    });
}

/// Finishes the active test context: runs LIFO cleanups, cleans up contained temp dirs,
/// and returns the final outcome.
pub fn finish_test_context() -> TestContextResult {
    let ctx = ACTIVE_CONTEXT.with(|cell| cell.borrow_mut().take());

    let Some(mut ctx) = ctx else {
        return TestContextResult {
            id: String::new(),
            status: TestStatus::Passed,
            failure: None,
            secondary_failures: Vec::new(),
            logs: Vec::new(),
            logs_truncated: false,
        };
    };

    // 1. Run all cleanups in LIFO order (reverse of registration).
    while let Some(cleanup) = ctx.cleanups.pop() {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
        if res.is_err() {
            ctx.record_failure(TestFailure::simple("test cleanup handler panicked"));
        }
    }

    // 2. Safely remove registered temporary directories with containment validation.
    for dir in &ctx.temp_dirs {
        safe_cleanup_temp_dir(&ctx.temp_root, dir);
    }

    TestContextResult {
        id: ctx.id,
        status: ctx.status,
        failure: ctx.failure,
        secondary_failures: ctx.secondary_failures,
        logs: ctx.logs,
        logs_truncated: ctx.logs_truncated,
    }
}

/// Registers a cleanup closure for the active test case.
pub fn register_cleanup(f: impl FnOnce() + Send + 'static) {
    with_active_context(|ctx| {
        ctx.add_cleanup(f);
    });
}
