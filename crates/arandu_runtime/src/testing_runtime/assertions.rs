//! C-ABI expectation and assertion evaluation hooks.

use arandu_testing_contract::TestFailure;

use super::context::{effective_location, with_active_context};

pub(crate) unsafe fn parse_str_arg(ptr: *const u8, len: i64) -> Option<String> {
    if len <= 0 || ptr.is_null() {
        return None;
    }
    let len = usize::try_from(len).ok()?;
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(slice).ok().map(|s| s.to_string())
}

/// Evaluates a generic boolean expectation.
/// Returns 1 on success, 0 on failure.
///
/// # Safety
/// All pointer args must be valid UTF-8 fat-string slices (ptr + len) from JIT-compiled Arandu
/// code. Caller must not pass dangling or misaligned pointers. Zero-length strings (`len == 0`)
/// are accepted and treated as absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect(
    condition: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    if condition != 0 {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect",
            expr,
            Some("true".to_string()),
            Some("false".to_string()),
            Some("bool".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two integer values for equality.
/// Returns 1 if equal, 0 if not equal.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_i64(
    expected: i64,
    actual: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    if expected == actual {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(expected.to_string()),
            Some(actual.to_string()),
            Some("int".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two floating point values for equality.
/// Returns 1 if equal (or bitwise equal for NaN), 0 if not equal.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_f64(
    expected: f64,
    actual: f64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    if (expected == actual) || (expected.is_nan() && actual.is_nan()) {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(expected.to_string()),
            Some(actual.to_string()),
            Some("float".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two boolean values for equality.
/// Returns 1 if equal, 0 if not equal.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_bool(
    expected: i64,
    actual: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    let exp_bool = expected != 0;
    let act_bool = actual != 0;
    if exp_bool == act_bool {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(exp_bool.to_string()),
            Some(act_bool.to_string()),
            Some("bool".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two fat strings for equality.
/// Returns 1 if equal, 0 if not equal.
///
/// # Safety
/// All pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_str(
    exp_ptr: *const u8,
    exp_len: i64,
    act_ptr: *const u8,
    act_len: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    let exp_str = unsafe { parse_str_arg(exp_ptr, exp_len) }.unwrap_or_default();
    let act_str = unsafe { parse_str_arg(act_ptr, act_len) }.unwrap_or_default();

    if exp_str == act_str {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(exp_str),
            Some(act_str),
            Some("str".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Explicitly fails the active test with a message.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_fail(
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) {
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) }
        .unwrap_or_else(|| "test failed explicitly".to_string());
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure {
            operation: Some("fail".to_string()),
            message: msg,
            location: loc,
            expression: None,
            expected: None,
            actual: None,
            type_name: None,
            cause: None,
        };
        ctx.record_failure(failure);
    });
}

/// Explicitly skips the active test with a reason.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_skip(
    reason_ptr: *const u8,
    reason_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) {
    let reason = unsafe { parse_str_arg(reason_ptr, reason_len) }
        .unwrap_or_else(|| "test skipped explicitly".to_string());
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        ctx.record_skip(reason, loc);
    });
}

/// Appends a message to the test context log buffer.
///
/// # Safety
/// `msg_ptr` must be a valid UTF-8 fat-string slice from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_log(msg_ptr: *const u8, msg_len: i64) {
    if let Some(msg) = unsafe { parse_str_arg(msg_ptr, msg_len) } {
        with_active_context(|ctx| {
            ctx.log(msg);
        });
    }
}
