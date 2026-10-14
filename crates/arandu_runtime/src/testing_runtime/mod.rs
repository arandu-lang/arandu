//! Host runtime support for `std.testing` (SL_T.3).
//!
//! Provides process-isolated, deterministic testing context:
//! - Structured expectations (`expect`, `expectEqual`, `fail`, `skip`) without relying on panic text.
//! - Single-evaluation capture of expected/actual values and expressions.
//! - Deterministic LIFO cleanup stack executed on success, failure, or skip.
//! - Bounded log buffer (preventing unbounded allocation).
//! - Sandboxed `temp_dir` with cryptographic/nonce isolation and containment validation.

pub mod assertions;
pub mod benchmark;
pub mod context;
pub mod sandbox;
pub mod types;

pub use assertions::{
    ar_test_expect, ar_test_expect_equal_bool, ar_test_expect_equal_f64, ar_test_expect_equal_i64,
    ar_test_expect_equal_str, ar_test_fail, ar_test_log, ar_test_skip,
};
pub use benchmark::{
    BenchmarkEngine, ar_bench_loop, finish_benchmark_context, init_benchmark_context,
};
pub use context::{
    TestContext, ar_test_set_span, finish_test_context, init_test_context, register_cleanup,
};
pub use sandbox::ar_test_temp_dir;
pub use types::{MAX_BENCH_ITERATIONS, MAX_LOG_ENTRIES, MAX_LOG_TOTAL_BYTES, TestContextResult};

/// Opaque scalar barrier used by benchmark lowering. This is best-effort and
/// must never be used as a correctness or constant-time guarantee.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn ar_bench_black_box_i64(value: i64) -> i64 {
    std::hint::black_box(value)
}

/// Floating-point counterpart of [`ar_bench_black_box_i64`].
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn ar_bench_black_box_f64(value: f64) -> f64 {
    std::hint::black_box(value)
}

/// Pointer counterpart of [`ar_bench_black_box_i64`].
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn ar_bench_black_box_ptr(value: *mut u8) -> *mut u8 {
    std::hint::black_box(value)
}

#[cfg(test)]
mod tests {
    use super::context::with_active_context;
    use super::sandbox::safe_cleanup_temp_dir;
    use super::*;
    use arandu_testing_contract::{TestFailure, TestStatus};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    fn bench_config() -> arandu_testing_contract::BenchmarkConfigV1 {
        arandu_testing_contract::BenchmarkConfigV1 {
            warmup_ns: 10,
            measurement_ns: 60,
            samples: 3,
        }
    }

    #[test]
    fn benchmark_engine_discards_warmup_and_records_complete_batches() {
        let mut engine = BenchmarkEngine::new(bench_config());
        assert!(engine.advance(0));
        assert!(engine.advance(5));
        assert!(engine.advance(10));
        // Warmup estimated 5ns/op, hence 4 iterations per 20ns sample.
        for now in [15, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70] {
            let keep_running = engine.advance(now);
            if now < 70 {
                assert!(keep_running);
            } else {
                assert!(!keep_running);
            }
        }
        assert_eq!(engine.samples.len(), 3);
        assert!(engine.samples.iter().all(|sample| sample.iterations == 4));
        assert_eq!(engine.samples[0].elapsed_ns, 20);
    }

    #[test]
    fn benchmark_engine_rejects_clock_rollback() {
        let mut engine = BenchmarkEngine::new(bench_config());
        assert!(engine.advance(100));
        assert!(!engine.advance(99));
        assert_eq!(
            engine.failure.as_deref(),
            Some("monotonic benchmark clock moved backwards")
        );
        assert!(!engine.advance(101));
    }

    #[test]
    fn benchmark_engine_retries_samples_below_clock_resolution() {
        let mut engine = BenchmarkEngine::new(arandu_testing_contract::BenchmarkConfigV1 {
            warmup_ns: 1,
            measurement_ns: 1,
            samples: 1,
        });
        assert!(engine.advance(0));
        assert!(engine.advance(1));
        // First one-iteration sample reports no clock progress and is retried
        // with a larger batch rather than published as 0 ns/op.
        assert!(engine.advance(1));
        assert!(engine.samples.is_empty());
        assert!(engine.advance(2));
        assert!(!engine.advance(3));
        assert_eq!(engine.samples.len(), 1);
        assert_eq!(engine.samples[0].iterations, 2);
        assert_eq!(engine.samples[0].elapsed_ns, 2);
    }

    #[test]
    fn benchmark_engine_rejects_invalid_internal_configuration() {
        let mut engine = BenchmarkEngine::new(arandu_testing_contract::BenchmarkConfigV1 {
            warmup_ns: 0,
            measurement_ns: 1,
            samples: 1,
        });
        assert!(!engine.advance(0));
        assert_eq!(
            engine.failure.as_deref(),
            Some("benchmark warmup must be greater than zero")
        );
    }

    #[test]
    fn expect_passes_and_fails_structurally() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit1");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test1", 42, Some(temp.clone()));

        unsafe {
            let res = ar_test_expect(1, b"1 == 1".as_ptr(), 6, b"".as_ptr(), 0, b"".as_ptr(), 0);
            assert_eq!(res, 1);
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Passed);
        assert!(out.failure.is_none());

        init_test_context("pkg::mod::test2", 42, Some(temp));
        unsafe {
            let res = ar_test_expect(
                0,
                b"x > 10".as_ptr(),
                6,
                b"x must be > 10".as_ptr(),
                14,
                b"main.aru:10".as_ptr(),
                11,
            );
            assert_eq!(res, 0);
        }
        let out2 = finish_test_context();
        assert_eq!(out2.status, TestStatus::Failed);
        let failure = out2.failure.unwrap();
        assert_eq!(failure.operation.as_deref(), Some("expect"));
        assert_eq!(failure.expression.as_deref(), Some("x > 10"));
        assert_eq!(failure.message, "x must be > 10");
        assert_eq!(failure.location.as_deref(), Some("main.aru:10"));
    }

    #[test]
    fn expect_equal_i64_and_str() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit2");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_eq", 42, Some(temp));

        unsafe {
            assert_eq!(
                ar_test_expect_equal_i64(10, 10, b"".as_ptr(), 0, b"".as_ptr(), 0, b"".as_ptr(), 0),
                1
            );
            assert_eq!(
                ar_test_expect_equal_i64(
                    10,
                    20,
                    b"a == b".as_ptr(),
                    6,
                    b"".as_ptr(),
                    0,
                    b"".as_ptr(),
                    0
                ),
                0
            );
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Failed);
        let f = out.failure.unwrap();
        assert_eq!(f.expected.as_deref(), Some("10"));
        assert_eq!(f.actual.as_deref(), Some("20"));
    }

    #[test]
    fn lifo_cleanup_executes_on_failure() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit3");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_cleanup", 42, Some(temp));

        use std::sync::Arc;
        use std::sync::Mutex;
        let trace = Arc::new(Mutex::new(Vec::new()));

        let t1 = trace.clone();
        register_cleanup(move || {
            t1.lock().unwrap().push(1);
        });

        let t2 = trace.clone();
        register_cleanup(move || {
            t2.lock().unwrap().push(2);
        });

        unsafe {
            ar_test_fail(b"boom".as_ptr(), 4, b"".as_ptr(), 0);
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Failed);
        // LIFO: cleanup 2 was registered second, so it must execute first: [2, 1]
        assert_eq!(*trace.lock().unwrap(), vec![2, 1]);
    }

    #[test]
    fn skip_marks_status_skipped() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit4");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_skip", 42, Some(temp));

        unsafe {
            ar_test_skip(b"not supported on windows".as_ptr(), 24, b"".as_ptr(), 0);
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Skipped);
        assert_eq!(out.failure.unwrap().message, "not supported on windows");
    }

    #[test]
    fn preserves_primary_and_secondary_failures_and_truncates_logs() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit5");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_multiple", 42, Some(temp));
        with_active_context(|ctx| {
            ctx.record_failure(TestFailure::simple("primary"));
            ctx.record_failure(TestFailure::simple("secondary"));
            ctx.log("x".repeat(MAX_LOG_TOTAL_BYTES));
            ctx.log("overflow".to_string());
        });
        let out = finish_test_context();
        assert_eq!(
            out.failure.as_ref().map(|f| f.message.as_str()),
            Some("primary")
        );
        assert_eq!(out.secondary_failures.len(), 1);
        assert_eq!(out.secondary_failures[0].message, "secondary");
        assert!(out.logs_truncated);
    }

    #[test]
    fn temp_dir_returns_a_usable_path_and_cleanup_removes_it() {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ar_test_ctx_temp_{}_{}",
                std::process::id(),
                sandbox::NONCE_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("reserve test directory: {error}"),
            }
        };
        init_test_context("pkg::mod::test_temp", 42, Some(root.clone()));
        let returned = unsafe { ar_test_temp_dir(0) };
        let bytes = unsafe {
            std::slice::from_raw_parts(returned.ptr, usize::try_from(returned.len).unwrap())
        };
        let path = PathBuf::from(std::str::from_utf8(bytes).unwrap());
        assert!(path.is_dir());
        let _ = finish_test_context();
        assert!(!path.exists());
        let _ = std::fs::remove_dir(&root);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_rejects_unix_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join("ar_test_ctx_symlink_root");
        let outside = std::env::temp_dir().join("ar_test_ctx_symlink_outside");
        let link = root.join("escape");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"safe").unwrap();
        symlink(&outside, &link).unwrap();
        safe_cleanup_temp_dir(&root, &link);
        assert!(outside.join("sentinel").is_file());
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_rejects_windows_symlink_escape_when_supported() {
        use std::os::windows::fs::symlink_dir;
        let root = std::env::temp_dir().join("ar_test_ctx_symlink_root");
        let outside = std::env::temp_dir().join("ar_test_ctx_symlink_outside");
        let link = root.join("escape");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"safe").unwrap();
        if symlink_dir(&outside, &link).is_ok() {
            safe_cleanup_temp_dir(&root, &link);
            assert!(outside.join("sentinel").is_file());
            let _ = std::fs::remove_dir(&link);
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
