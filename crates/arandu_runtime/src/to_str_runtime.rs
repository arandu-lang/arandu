//! Host helpers for ToStr v0.1, linked into the Cranelift JIT module.
//!
//! Helpers that document "Caller owns the returned buffer" transfer ownership
//! of a `malloc`'d NUL-terminated copy; compiled code pairs it with the
//! language's `str` drop (`Free(str)`). Buffers that must outlive every frame
//! (host-boundary paths such as `fat_str_from_string`) document that policy at
//! their definition instead — do not assume it here.
//!
//! Every `extern "C"` entry point below is wrapped in `crate::ffi::guard`:
//! a panic must never unwind into JIT'd code, so it aborts instead.

use std::fmt::{self, Write};
use std::os::raw::c_void;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
}

/// Allocate `bytes` as a NUL-terminated buffer; write byte length (excluding
/// NUL) to `out_len`. Returns pointer (never null on success; aborts on OOM).
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`.
unsafe fn pack_bytes(bytes: &[u8], out_len: *mut i64) -> *mut u8 {
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

/// [`pack_bytes`] for string literals and other UTF-8 sources.
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`.
unsafe fn pack_string(s: &str, out_len: *mut i64) -> *mut u8 {
    unsafe { pack_bytes(s.as_bytes(), out_len) }
}

/// Longest decimal rendering: `u64::MAX` (20 digits) and `i64::MIN`
/// (`-` plus 19 digits) both fit in exactly 20 bytes.
const INT_BUF_LEN: usize = 20;

/// Write `v` as decimal digits, right-aligned in `buf`; returns the start
/// offset of the first digit.
///
/// Formats on the stack so the ToStr helpers avoid the temporary `String`
/// that `v.to_string()` allocated per call.
fn write_u64_digits(mut v: u64, buf: &mut [u8; INT_BUF_LEN]) -> usize {
    let mut start = INT_BUF_LEN;
    loop {
        start -= 1;
        buf[start] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    start
}

/// Write `v` as decimal (leading `-` when negative) right-aligned in `buf`;
/// returns the start offset of the first character.
///
/// `i64::MIN` goes through `unsigned_abs`, so the magnitude never overflows;
/// every negative magnitude has at most 19 digits, leaving room for the sign.
fn write_i64_digits(v: i64, buf: &mut [u8; INT_BUF_LEN]) -> usize {
    let (magnitude, negative) = if v < 0 {
        (v.unsigned_abs(), true)
    } else {
        (v as u64, false)
    };
    let start = write_u64_digits(magnitude, buf);
    if negative {
        debug_assert!(start >= 1, "sign byte needs room before the digits");
        let sign = start - 1;
        buf[sign] = b'-';
        sign
    } else {
        start
    }
}

/// `int64_t` → decimal string.
///
/// Formats into a stack buffer first: the only heap allocation is the
/// `pack_bytes` copy the caller receives (the previous `v.to_string()`
/// allocated a second, temporary `String` per call).
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_i64_to_str(v: i64, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| {
        let mut buf = [0u8; INT_BUF_LEN];
        let start = write_i64_digits(v, &mut buf);
        unsafe { pack_bytes(&buf[start..], out_len) }
    })
}

/// `uint64_t` → decimal string.
///
/// Formats into a stack buffer first; see `ar_jit_i64_to_str`.
///
/// # Safety
/// `out_len` must be null or a valid writable `*mut i64`. Caller owns the
/// returned buffer (allocated with `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_jit_u64_to_str(v: u64, out_len: *mut i64) -> *mut u8 {
    crate::ffi::guard(|| {
        let mut buf = [0u8; INT_BUF_LEN];
        let start = write_u64_digits(v, &mut buf);
        unsafe { pack_bytes(&buf[start..], out_len) }
    })
}

/// Measure a signed integer or write it directly into interpolation backing.
/// No allocation; returns the byte count, excluding NUL.
///
/// # Safety
/// `destination` must be null or writable for the returned byte count
/// (at most 20 bytes). No NUL is written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_i64_write_digits(v: i64, destination: *mut u8) -> i64 {
    crate::ffi::guard(|| {
        let mut buffer = [0; INT_BUF_LEN];
        let start = write_i64_digits(v, &mut buffer);
        // SAFETY: forwarded nullable destination contract.
        unsafe { copy_integer_digits(&buffer[start..], destination) }
    })
}

/// Measure an unsigned integer or write it directly into interpolation backing.
/// No allocation; returns the byte count, excluding NUL.
///
/// # Safety
/// `destination` must be null or writable for the returned byte count
/// (at most 20 bytes). No NUL is written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_u64_write_digits(v: u64, destination: *mut u8) -> i64 {
    crate::ffi::guard(|| {
        let mut buffer = [0; INT_BUF_LEN];
        let start = write_u64_digits(v, &mut buffer);
        // SAFETY: forwarded nullable destination contract.
        unsafe { copy_integer_digits(&buffer[start..], destination) }
    })
}

unsafe fn copy_integer_digits(bytes: &[u8], destination: *mut u8) -> i64 {
    if !destination.is_null() {
        // SAFETY: private callers forward the public writable destination
        // contract; bytes is a separate stack buffer of at most 20 bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len()) };
    }
    i64::try_from(bytes.len()).unwrap_or(20)
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
    crate::ffi::guard(|| {
        let mut buffer = FloatBuffer::default();
        if write_f64_v01(&mut buffer, v).is_err() {
            // The fixed buffer covers every default Display rendering of f64.
            // Do not return a truncated numeric string if that contract changes.
            std::process::abort();
        }
        // SAFETY: forwarded writable-or-null out_len contract.
        unsafe { pack_bytes(&buffer.bytes[..buffer.len], out_len) }
    })
}

/// Shared ToStr v0.1 float formatting (keep in sync with C `ar_f64_to_str`).
pub fn format_f64_v01(v: f64) -> String {
    let mut output = String::new();
    // String's fmt::Write cannot fail.
    let _ = write_f64_v01(&mut output, v);
    output
}

/// Default f64 Display uses fixed decimal notation: even the smallest
/// subnormal needs fewer than 350 bytes (sign, decimal point, leading zeros,
/// significant digits). Leave headroom without changing the formatting policy.
struct FloatBuffer {
    bytes: [u8; 384],
    len: usize,
}

impl Default for FloatBuffer {
    fn default() -> Self {
        Self {
            bytes: [0; 384],
            len: 0,
        }
    }
}

impl Write for FloatBuffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let Some(end) = self.len.checked_add(text.len()) else {
            return Err(fmt::Error);
        };
        let Some(destination) = self.bytes.get_mut(self.len..end) else {
            return Err(fmt::Error);
        };
        destination.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn write_f64_v01(output: &mut impl Write, v: f64) -> fmt::Result {
    if v.is_nan() {
        output.write_str("nan")
    } else if v.is_infinite() {
        output.write_str(if v.is_sign_negative() { "-inf" } else { "inf" })
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        // Preserve the existing policy, including -0.0 rendering as "0".
        write!(output, "{}", v as i64)
    } else {
        write!(output, "{v}")
    }
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
        let mut buffer = [0; 4];
        let character = char::from_u32(v).unwrap_or('\u{FFFD}');
        let text = character.encode_utf8(&mut buffer);
        // SAFETY: forwarded writable-or-null out_len contract.
        unsafe { pack_bytes(text.as_bytes(), out_len) }
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

/// Prelude `io.print(str)` — write UTF-8 bytes without a trailing newline.
///
/// # Safety
/// `ptr` must be valid for `len` bytes if `len > 0`; `len` is non-negative.
#[unsafe(export_name = "io.print")]
pub unsafe extern "C" fn ar_jit_print(ptr: *const u8, len: i64) {
    crate::ffi::guard(|| {
        use std::io::{self, Write};
        let mut stdout = io::stdout().lock();
        if len > 0 && !ptr.is_null() {
            let Ok(length) = usize::try_from(len) else {
                return;
            };
            // SAFETY: the caller supplies a readable buffer of `length` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, length) };
            let _ = stdout.write_all(bytes);
        }
        let _ = stdout.flush();
    });
}

/// Prelude `io.eprint(str)` — write `len` bytes at `ptr` to stderr.
///
/// Linked as the JIT symbol `eprint` (dual fat-pointer ABI: ptr + i64 len).
///
/// # Safety
/// `ptr` must be valid for `len` bytes if `len > 0`. `len` must be non-negative.
#[unsafe(export_name = "io.eprint")]
pub unsafe extern "C" fn ar_jit_eprint(ptr: *const u8, len: i64) {
    crate::ffi::guard(|| {
        use std::io::{self, Write};
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        if len > 0 && !ptr.is_null() {
            // SAFETY: caller contract: `ptr` is valid for `len` bytes.
            let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
            let _ = handle.write_all(slice);
        }
        let _ = handle.flush();
    });
}

/// Native AOT link name for the prelude `io.eprint(str)` import.
///
/// Cranelift emits this import as the unqualified symbol `eprint`; the JIT
/// binds that name directly, while AOT builds resolve it from this runtime
/// library. Keep the dotted `io.eprint` export above for consumers using the
/// qualified runtime ABI.
///
/// # Safety
/// `ptr` must be valid for `len` bytes if `len > 0`. `len` must be non-negative.
#[unsafe(export_name = "eprint")]
pub unsafe extern "C" fn ar_aot_eprint(ptr: *const u8, len: i64) {
    // SAFETY: this function forwards the caller's pointer/length contract
    // unchanged to the implementation of the same ABI.
    unsafe { ar_jit_eprint(ptr, len) };
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
        #[link_name = "eprint"]
        fn aot_eprint(ptr: *const u8, len: i64);
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
    fn integer_helpers_format_extremes_into_one_stack_buffer() {
        unsafe {
            let mut len = 0i64;

            // Zero on both sides of the signedness split.
            let p = ar_jit_u64_to_str(0, &mut len);
            assert_eq!(owned_str(p, len), "0");
            assert_eq!(len, 1);
            let p = ar_jit_i64_to_str(0, &mut len);
            assert_eq!(owned_str(p, len), "0");
            assert_eq!(len, 1);

            // Widest values: u64::MAX = 20 digits; i64::MIN = '-' + 19 digits.
            let p = ar_jit_u64_to_str(u64::MAX, &mut len);
            assert_eq!(len, 20);
            assert_eq!(owned_str(p, len), "18446744073709551615");
            let p = ar_jit_i64_to_str(i64::MAX, &mut len);
            assert_eq!(len, 19);
            assert_eq!(owned_str(p, len), "9223372036854775807");
            let p = ar_jit_i64_to_str(i64::MIN, &mut len);
            assert_eq!(len, 20);
            assert_eq!(owned_str(p, len), "-9223372036854775808");

            // Length contract: `len` excludes the trailing NUL byte.
            let p = ar_jit_i64_to_str(-42, &mut len);
            assert_eq!(len, 3);
            assert_eq!(*p.add(3), 0);
            assert_eq!(owned_str(p, len), "-42");

            // `out_len` may be null, including on the widest values.
            let p = ar_jit_i64_to_str(i64::MIN, std::ptr::null_mut());
            assert_eq!(*p.add(20), 0);
            free(p.cast());
            let p = ar_jit_u64_to_str(u64::MAX, std::ptr::null_mut());
            assert_eq!(*p.add(20), 0);
            free(p.cast());
        }
    }

    #[test]
    fn integer_helpers_match_to_string_sweep() {
        const SIGNED: [i64; 12] = [
            i64::MIN,
            i64::MIN + 1,
            -1000,
            -42,
            -10,
            -1,
            0,
            1,
            10,
            42,
            i64::MAX - 1,
            i64::MAX,
        ];
        const UNSIGNED: [u64; 8] = [0, 1, 9, 10, 99, 1000, u64::MAX - 1, u64::MAX];
        unsafe {
            let mut len = 0i64;
            for v in SIGNED {
                let expected = v.to_string();
                let p = ar_jit_i64_to_str(v, &mut len);
                assert_eq!(owned_str(p, len), expected, "i64 {v}");
                assert_eq!(len, expected.len() as i64, "i64 len for {v}");
            }
            for v in UNSIGNED {
                let expected = v.to_string();
                let p = ar_jit_u64_to_str(v, &mut len);
                assert_eq!(owned_str(p, len), expected, "u64 {v}");
                assert_eq!(len, expected.len() as i64, "u64 len for {v}");
            }
        }
    }

    #[test]
    fn char_helpers_preserve_utf8_nul_and_invalid_scalars() {
        for scalar in [
            0,
            0x7f,
            0x80,
            0x7ff,
            0x800,
            0xd7ff,
            0xd800,
            0xdfff,
            0xe000,
            0xffff,
            0x10000,
            0x10ffff,
            0x110000,
            u32::MAX,
        ] {
            let expected = char::from_u32(scalar).unwrap_or('\u{FFFD}').to_string();
            // SAFETY: valid out-length and helper-owned buffer, released below.
            unsafe {
                let mut len = -1;
                let pointer = ar_jit_char_to_str(scalar, &mut len);
                assert_eq!(len as usize, expected.len());
                assert_eq!(*pointer.add(expected.len()), 0);
                assert_eq!(owned_str(pointer, len), expected);
                let pointer = ar_jit_char_to_str(scalar, std::ptr::null_mut());
                assert_eq!(owned_str(pointer, expected.len() as i64), expected);
            }
        }
    }

    fn legacy_float_format(value: f64) -> String {
        if value.is_nan() {
            "nan".to_string()
        } else if value.is_infinite() {
            if value.is_sign_negative() {
                "-inf".to_string()
            } else {
                "inf".to_string()
            }
        } else if value.fract() == 0.0 && value.abs() < 1e15 {
            format!("{}", value as i64)
        } else {
            format!("{value}")
        }
    }

    #[test]
    fn float_stack_format_preserves_legacy_output_and_length() {
        let check = |value| {
            let expected = legacy_float_format(value);
            assert_eq!(format_f64_v01(value), expected);
            // SAFETY: valid out-length and helper-owned buffer, released below.
            unsafe {
                let mut len = -1;
                let pointer = ar_jit_f64_to_str(value, &mut len);
                assert_eq!(len as usize, expected.len());
                assert_eq!(*pointer.add(expected.len()), 0);
                assert_eq!(owned_str(pointer, len), expected);
                let pointer = ar_jit_f64_to_str(value, std::ptr::null_mut());
                assert_eq!(owned_str(pointer, expected.len() as i64), expected);
            }
        };
        for value in [
            0.0,
            -0.0,
            1.5,
            -1.5,
            1e15 - 1.0,
            1e15,
            -1e15,
            f64::MIN,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            check(value);
        }
        // Deterministic bit-pattern sweep includes many exponent/precision cases.
        let mut bits = 0x1234_5678_9abc_def0u64;
        for _ in 0..10_000 {
            bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
            check(f64::from_bits(bits));
        }
    }

    #[test]
    fn integer_concat_writers_measure_and_preserve_surrounding_bytes() {
        for value in [i64::MIN, -1, 0, 42, i64::MAX] {
            let expected = value.to_string();
            let mut buffer = [0xa5; 22];
            // SAFETY: null measures only; offset buffer has 20 writable bytes.
            unsafe {
                assert_eq!(
                    ar_rt_i64_write_digits(value, std::ptr::null_mut()),
                    expected.len() as i64
                );
                assert_eq!(
                    ar_rt_i64_write_digits(value, buffer.as_mut_ptr().add(1)),
                    expected.len() as i64
                );
            }
            assert_eq!(&buffer[1..1 + expected.len()], expected.as_bytes());
            assert_eq!(buffer[0], 0xa5);
            assert!(
                buffer[1 + expected.len()..]
                    .iter()
                    .all(|byte| *byte == 0xa5)
            );
        }
        for value in [0, 42, u64::MAX] {
            let expected = value.to_string();
            let mut buffer = [0xa5; 22];
            // SAFETY: null measures only; offset buffer has 20 writable bytes.
            unsafe {
                assert_eq!(
                    ar_rt_u64_write_digits(value, std::ptr::null_mut()),
                    expected.len() as i64
                );
                assert_eq!(
                    ar_rt_u64_write_digits(value, buffer.as_mut_ptr().add(1)),
                    expected.len() as i64
                );
            }
            assert_eq!(&buffer[1..1 + expected.len()], expected.as_bytes());
            assert_eq!(buffer[0], 0xa5);
            assert!(
                buffer[1 + expected.len()..]
                    .iter()
                    .all(|byte| *byte == 0xa5)
            );
        }
    }

    #[test]
    fn aot_eprint_link_symbol_is_exported() {
        // Link through the exact name referenced by native Cranelift objects.
        unsafe { aot_eprint(std::ptr::null(), 0) };
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
