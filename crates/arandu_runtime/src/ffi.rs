//! Panic containment for `extern "C"` entry points.
//!
//! A Rust panic must never unwind across an FFI boundary: unwinding into
//! code that does not understand Rust (the Cranelift JIT, generated C) is
//! undefined behavior. Host helpers therefore catch panics at the boundary
//! and abort the process — the same policy the string helpers already apply
//! to `malloc` failure.
//!
//! Use [`guard`] only where the ABI has no failure channel. Routines that
//! encode errors as return values (see `fs_runtime`) map the panic to their
//! portable error code instead of aborting, provided no ownership has been
//! transferred before the failure.

/// Runs `f`, aborting the process if it panics.
///
/// Only call this directly inside `extern "C"` entry points: the closure
/// must own every pointer it touches for the duration of the call.
pub(crate) fn guard<T>(f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            // The ABI has no way to report the failure and invariants may be
            // half-transferred (ownership crosses these boundaries by move);
            // continuing could corrupt the host or the JIT. Fail loudly
            // instead of turning the panic into undefined behavior.
            std::process::abort();
        }
    }
}
