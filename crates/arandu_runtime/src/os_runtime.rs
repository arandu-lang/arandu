//! Thin OS hosts for Minimal 0.1 optional std surface (`std.process` / `std.time` / `std.env`).
//!
//! Design notes (aligned with Go `os` / Rust `std::{process,env,time}`):
//! - `exit` terminates the **process** (like `os.Exit` / `process::exit`), not just `main`.
//! - `monotonic_ns` is a steady clock (not wall time) — comparable to `Instant` / `CLOCK_MONOTONIC`.
//! - `env` is **query-only** in Minimal: no `setenv` (avoids global-process races).
//! - Strings use the JIT fat ABI (`*const u8` + `i64` len), same as path helpers.

use std::time::{Duration, Instant};

use crate::rt_runtime::ArFatStr;

/// Process start for monotonic offsets (lazy, process-local).
fn mono_origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Terminate the process with `code` (truncated to 8 bits on Unix by the OS).
///
/// # Safety
/// Called only from JIT-linked Arandu code; never returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_process_exit(code: i64) -> ! {
    // Match common shell convention: clamp to u8 range for predictability.
    let status = if code < 0 {
        255
    } else if code > 255 {
        (code & 0xff) as i32
    } else {
        code as i32
    };
    std::process::exit(status)
}

/// End the process after a language-level abort on Windows.
///
/// An inline `ud2` inside a JIT frame has no unwind info. Windows reports
/// that as a stack overflow and writes the message to stderr, so the abort
/// contract (failed status, empty stderr) is lost. `ExitProcess` terminates
/// without the C runtime's `abort` banner. Unix keeps the hardware trap and
/// does not emit this symbol.
///
/// # Safety
/// Called only from JIT-linked Arandu code. This function never returns.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_trap() -> ! {
    // SAFETY: `ExitProcess` ends the process. Status 1 is a failed exit
    // without writing to stderr.
    unsafe { exit_process(1) }
}

#[cfg(windows)]
unsafe extern "system" {
    #[link_name = "ExitProcess"]
    fn exit_process(code: u32) -> !;
}

/// Nanoseconds since an arbitrary process-local epoch (monotonic, not wall clock).
///
/// # Safety
/// No pointer args; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_time_monotonic_ns() -> i64 {
    let elapsed: Duration = mono_origin().elapsed();
    // Saturate instead of wrapping on very long-lived processes.
    i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
}

static PROGRAM_ARGV: std::sync::OnceLock<Box<[String]>> = std::sync::OnceLock::new();
static HOST_ARGV: std::sync::OnceLock<Box<[String]>> = std::sync::OnceLock::new();

/// Installs the argument vector visible to JIT-compiled code.
///
/// A standalone AOT executable reads its own process arguments. A JIT program
/// shares the compiler process, so the CLI must replace that host vector with
/// the program entry path followed by the values after `arandu run ... --`.
pub fn install_program_args(args: Box<[String]>) -> Result<(), Box<[String]>> {
    PROGRAM_ARGV.set(args)
}

/// C ABI for AOT wrappers (LLVM/C smith) that need a synthetic `argv[0]`.
///
/// Returns `0` on success and `-1` when arguments are invalid or already
/// installed. Pointers must remain valid for the duration of this call.
///
/// # Safety
/// `argv` must point to `argc` readable C strings (null entries become empty).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_env_install_args(
    argc: i32,
    argv: *const *const core::ffi::c_char,
) -> i32 {
    if argc < 0 || argv.is_null() {
        return -1;
    }
    let mut args = Vec::with_capacity(argc as usize);
    for index in 0..argc as usize {
        // SAFETY: caller promised `argv` has `argc` readable entries.
        let pointer = unsafe { *argv.add(index) };
        if pointer.is_null() {
            args.push(String::new());
            continue;
        }
        // SAFETY: non-null entries are NUL-terminated C strings for this call.
        let c_str = unsafe { std::ffi::CStr::from_ptr(pointer) };
        args.push(c_str.to_string_lossy().into_owned());
    }
    match install_program_args(args.into_boxed_slice()) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

fn argv_snapshot() -> &'static [String] {
    PROGRAM_ARGV.get().map_or_else(
        || {
            HOST_ARGV.get_or_init(|| {
                std::env::args_os()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
        },
        Box::as_ref,
    )
}

/// Number of process arguments including `argv[0]` (like Go `len(os.Args)` / Rust `env::args().len()`).
///
/// # Safety
/// No pointer args.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_env_args_len() -> i64 {
    argv_snapshot().len() as i64
}

/// `argv[index]` as a UTF-8 fat string (lossy), empty `""` when out of range.
///
/// Reading argv is safe on non-UTF-8 bytes (Rust `env::args()` panics; we use
/// `args_os` + lossy conversion — never a panic on user-provided data).
/// Bounds check mirrors shell `$n` semantics: out-of-range is `""`, not an
/// error; callers use `ar_env_args_len` to pre-check.
fn env_arg_impl(index: isize) -> ArFatStr {
    if index < 0 || index as usize >= argv_snapshot().len() {
        return ArFatStr {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
    }
    let argument = &argv_snapshot()[index as usize];
    ArFatStr {
        ptr: argument.as_ptr().cast_mut(),
        len: argument.len() as isize,
    }
}

#[cfg(not(windows))]
/// Return `argv[index]` empty when out of range.
///
/// # Safety
/// No pointer args; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_env_arg(index: isize) -> ArFatStr {
    env_arg_impl(index)
}

#[cfg(windows)]
/// System V ABI expected by JIT code for fat-string returns.
///
/// # Safety
/// No pointer args; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_env_arg(index: isize) -> ArFatStr {
    env_arg_impl(index)
}

/// `1` if the environment variable `name` is set (even to empty), else `0`.
///
/// Mirrors Rust `env::var_os(name).is_some()` and Go `LookupEnv` presence — not “non-empty”.
///
/// # Safety
/// `ptr`/`len` must form a valid UTF-8 fat string from the Arandu JIT (or null/empty → 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_env_var_is_set(ptr: *const u8, len: i64) -> i64 {
    if len <= 0 || ptr.is_null() {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let Ok(name) = std::str::from_utf8(bytes) else {
        return 0;
    };
    if name.is_empty() || name.contains('\0') || name.contains('=') {
        // Invalid env names — treat as unset (never panic from host).
        return 0;
    }
    i64::from(std::env::var_os(name).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_non_decreasing() {
        unsafe {
            let a = ar_time_monotonic_ns();
            let b = ar_time_monotonic_ns();
            assert!(b >= a, "monotonic_ns went backwards: {a} -> {b}");
            assert!(a >= 0);
        }
    }

    #[test]
    fn args_len_at_least_one() {
        unsafe {
            assert!(ar_env_args_len() >= 1);
        }
    }

    #[test]
    fn install_args_rejects_invalid_pointers() {
        unsafe {
            assert_eq!(ar_env_install_args(-1, std::ptr::null()), -1);
            assert_eq!(ar_env_install_args(1, std::ptr::null()), -1);
        }
    }

    #[test]
    fn env_arg_indexes_match_args_os() {
        let expected: Vec<String> = std::env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(!expected.is_empty(), "test runner must have argv[0]");
        let n = expected.len();
        for (i, want) in expected.iter().enumerate() {
            let got = unsafe { ar_env_arg(i as isize) };
            // The leaked fat string is readable for `len` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(got.ptr, got.len as usize) };
            let text = String::from_utf8_lossy(bytes);
            assert_eq!(*want, text, "argv[{i}] mismatch");
        }
        // Out of range on both sides is empty "", never a panic.
        for bad in [-1, n as isize, n as isize + 1000] {
            let got = unsafe { ar_env_arg(bad) };
            assert_eq!(got.len, 0, "argv[{bad}] must be empty");
        }
    }

    #[test]
    fn var_is_set_path_and_missing() {
        unsafe {
            // PATH is present on essentially all Unix/macOS/Windows CI images.
            let path = b"PATH";
            assert_eq!(ar_env_var_is_set(path.as_ptr(), path.len() as i64), 1);

            let missing = b"ARANDU_P1_ENV_MISSING_XYZ_9f3a";
            assert_eq!(ar_env_var_is_set(missing.as_ptr(), missing.len() as i64), 0);

            // Empty / invalid names never set.
            assert_eq!(ar_env_var_is_set(b"".as_ptr(), 0), 0);
            assert_eq!(ar_env_var_is_set(b"A=B".as_ptr(), 3), 0);
        }
    }
}
