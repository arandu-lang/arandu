//! Host helpers for ToStr v0.1, linked into the Cranelift JIT module.
//!
//! These allocate with `malloc` (same lifetime policy as StringInterp concat:
//! process-lifetime leak is acceptable for debug JIT).
//!
//! Every `extern "C"` entry point below is wrapped in `crate::ffi::guard`:
//! a panic must never unwind into JIT'd code, so it aborts instead.

use std::os::raw::c_void;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
}

/// Allocate `s` as a NUL-terminated buffer; write byte length (excluding NUL)
/// to `out_len`. Returns pointer (never null on success; aborts on OOM).
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`.
unsafe fn pack_string(s: &str, out_len: *mut i64) -> *mut u8 {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if !out_len.is_null() {
        // Saturate instead of wrapping: `usize` → `i64` is lossy only above
        // `i64::MAX`, which no live allocation can reach, but a silent
        // negative length would be an out-of-bounds contract for the caller.
        unsafe {
            *out_len = i64::try_from(len).unwrap_or(i64::MAX);
        }
    }
    let ptr = unsafe { malloc(len + 1) as *mut u8 };
    if ptr.is_null() {
        // Match C runtime abort-on-OOM policy for debug helpers.
        std::process::abort();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        *ptr.add(len) = 0;
    }
    ptr
}

/// `int64_t` → decimal string.
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_i64_to_str(v: i64, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| unsafe { pack_string(&v.to_string(), out_len) })
}

/// `uint64_t` → decimal string.
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_u64_to_str(v: u64, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| unsafe { pack_string(&v.to_string(), out_len) })
}

/// `f64` → decimal string aligned with C emit `%.15g` for common finite values.
///
/// Specials: `nan`, `inf`, `-inf` (lowercase, matching typical C `%g` style).
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_f64_to_str(v: f64, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| unsafe { pack_string(&format_f64_v01(v), out_len) })
}

/// Shared ToStr v0.1 float formatting (keep in sync with C `ar_f64_to_str`).
pub fn format_f64_v01(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    // Prefer a compact decimal; match C `%.15g` for ordinary magnitudes.
    // Rust's default Display is close; for whole numbers prefer no trailing `.0`
    // when the value is an integer in range (mirrors common `%g` output).
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v}");
    s
}

/// bool → `"true"` / `"false"`.
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_bool_to_str(v: i8, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| {
        let s = if v != 0 { "true" } else { "false" };
        unsafe { pack_string(s, out_len) }
    })
}

/// Unicode scalar value (u32) → UTF-8 string.
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_char_to_str(v: u32, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| {
        let s = char::from_u32(v)
            .map(|c| c.to_string())
            .unwrap_or_else(|| "\u{FFFD}".to_string());
        unsafe { pack_string(&s, out_len) }
    })
}

/// Prelude `io.println(str)` — write `len` bytes at `ptr` plus a newline.
///
/// Linked as the JIT symbol `io.println` (dual fat-pointer ABI: ptr + i64 len).
///
/// # Safety
/// `ptr` must be valid for `len` bytes if `len > 0`. `len` must be non-negative.
#[unsafe(export_name = "io.println")]
pub unsafe extern "C" fn ar_jit_println(ptr: *const u8, len: i64) {
    crate::ffi::guard(|| {
        use std::io::{self, Write};
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        if len > 0 && !ptr.is_null() {
            // SAFETY: caller contract: `ptr` is valid for `len` bytes.
            let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
            let _ = handle.write_all(slice);
        }
        let _ = handle.write_all(b"\n");
        let _ = handle.flush();
    });
}

/// Prelude `err.new(str) -> Err`.
///
/// `Err` is a non-null message handle: a `malloc`'d NUL-terminated copy of the
/// input bytes (same lifetime policy as ToStr helpers). Callers compare handles
/// against `nil` and may treat the pointer as a C string for debug printing.
///
/// Linked as the JIT symbol `err.new`.
///
/// # Safety
/// `ptr` must be valid for `len` bytes if `len > 0`. `len` must be non-negative.
#[unsafe(export_name = "err.new")]
pub unsafe extern "C" fn ar_jit_err_new(ptr: *const u8, len: i64) -> *mut u8 {
    crate::ffi::guard(|| {
        let slice = if len > 0 && !ptr.is_null() {
            // SAFETY: caller contract: `ptr` is valid for `len` bytes.
            unsafe { std::slice::from_raw_parts(ptr, len as usize) }
        } else {
            b""
        };
        // Lossy only if input is not valid UTF-8; messages are language string literals.
        let s = std::str::from_utf8(slice).unwrap_or("");
        unsafe { pack_string(s, std::ptr::null_mut()) }
    })
}

/// ToStr for `Err`: the handle *is* a NUL-terminated message buffer.
///
/// Returns `(ptr, len)` via the usual out-len slot. Does not allocate.
///
/// # Safety
/// `err` must be null or a valid NUL-terminated buffer from `err.new`.
/// `out_len` must be null or a valid writable `*mut i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_err_to_str(err: *const u8, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| {
        if err.is_null() {
            if !out_len.is_null() {
                unsafe {
                    *out_len = 0;
                }
            }
            return std::ptr::null_mut();
        }
        let mut len = 0usize;
        // SAFETY: err is NUL-terminated (pack_string / err.new contract).
        unsafe {
            while *err.add(len) != 0 {
                len += 1;
            }
        }
        if !out_len.is_null() {
            // Saturating cast: see the note in `pack_string`.
            unsafe {
                *out_len = i64::try_from(len).unwrap_or(i64::MAX);
            }
        }
        err as *mut u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn free(ptr: *mut c_void);
    }

    /// # Safety
    /// `ptr` must come from one of this module's `pack_string` callers.
    unsafe fn owned_str(ptr: *mut u8, len: i64) -> String {
        // SAFETY: caller guarantees the malloc'd buffer contract; the copy
        // does not extend the buffer's lifetime beyond this call.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        // Materialize before freeing: the slice would dangle after `free`
        // (the allocator may already reuse or scribble over the chunk).
        let s = String::from_utf8_lossy(bytes).into_owned();
        // SAFETY: paired with the `malloc` inside `pack_string`.
        unsafe { free(ptr.cast()) };
        s
    }

    #[test]
    fn integer_bool_char_and_float_helpers_roundtrip() {
        unsafe {
            let mut len = 0i64;
            let p = ar_jit_i64_to_str(-42, &mut len);
            assert_eq!(owned_str(p, len), "-42");

            let p = ar_jit_u64_to_str(u64::MAX, &mut len);
            assert_eq!(owned_str(p, len), "18446744073709551615");

            let p = ar_jit_bool_to_str(1, &mut len);
            assert_eq!(owned_str(p, len), "true");
            let p = ar_jit_bool_to_str(0, &mut len);
            assert_eq!(owned_str(p, len), "false");

            // Invalid Unicode scalar values fall back to U+FFFD.
            let p = ar_jit_char_to_str(0xD800, &mut len);
            assert_eq!(owned_str(p, len), "\u{FFFD}");

            let p = ar_jit_f64_to_str(2.5, &mut len);
            assert_eq!(owned_str(p, len), "2.5");
            let p = ar_jit_f64_to_str(f64::INFINITY, &mut len);
            assert_eq!(owned_str(p, len), "inf");
        }
    }

    #[test]
    fn out_len_may_be_null() {
        unsafe {
            let p = ar_jit_i64_to_str(7, std::ptr::null_mut());
            assert!(!p.is_null());
            let bytes = std::slice::from_raw_parts(p, 1);
            assert_eq!(bytes, b"7");
            free(p.cast());
        }
    }

    #[test]
    fn err_new_and_err_to_str_share_one_buffer() {
        unsafe {
            let message = b"boom";
            let handle = ar_jit_err_new(message.as_ptr(), message.len() as i64);
            assert!(!handle.is_null());

            let mut len = 0i64;
            let p = ar_jit_err_to_str(handle, &mut len);
            assert_eq!(p, handle, "Err handle is its own message buffer");
            assert_eq!(owned_str(p, len), "boom");

            // nil Err → null handle, zero length.
            let p = ar_jit_err_to_str(std::ptr::null(), &mut len);
            assert!(p.is_null());
            assert_eq!(len, 0);
        }
    }
}
