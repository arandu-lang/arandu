//! C standard-library compatibility helpers for transpiled/FFI programs.
//!
//! Provides portable access to C standard stream pointers (`stdin`, `stdout`,
//! `stderr`) and typed fixed-arity `snprintf`/`fprintf` wrappers so callers on
//! any target ABI (including System V x86-64 `%al` register convention) can invoke
//! libc formatting routines without hand-rolling varargs glue.

#![allow(clippy::missing_safety_doc)]

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "freebsd",
        target_os = "dragonfly",
    ))
))]
unsafe fn host_std_stream(index: u8) -> *mut libc::FILE {
    unsafe extern "C" {
        static mut stdin: *mut libc::FILE;
        static mut stdout: *mut libc::FILE;
        static mut stderr: *mut libc::FILE;
    }
    // SAFETY: Standard C streams stay valid static FILE pointers for process lifetime.
    unsafe {
        match index {
            0 => stdin,
            1 => stdout,
            _ => stderr,
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "freebsd",
    target_os = "dragonfly",
))]
unsafe fn host_std_stream(index: u8) -> *mut libc::FILE {
    unsafe extern "C" {
        static mut __stdinp: *mut libc::FILE;
        static mut __stdoutp: *mut libc::FILE;
        static mut __stderrp: *mut libc::FILE;
    }
    // SAFETY: Standard C streams stay valid static FILE pointers for process lifetime.
    unsafe {
        match index {
            0 => __stdinp,
            1 => __stdoutp,
            _ => __stderrp,
        }
    }
}

#[cfg(windows)]
unsafe fn host_std_stream(index: u8) -> *mut libc::FILE {
    unsafe extern "C" {
        fn __acrt_iob_func(idx: libc::c_uint) -> *mut libc::FILE;
    }
    // SAFETY: Index 0..=2 returns the host UCRT standard FILE* entry.
    unsafe { __acrt_iob_func(libc::c_uint::from(index)) }
}

/// Returns the host C `stdin` `FILE*` pointer as an opaque `ptr[u8]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_c_stdin() -> *mut u8 {
    // SAFETY: Returns the C runtime standard input stream pointer.
    unsafe { host_std_stream(0).cast::<u8>() }
}

/// Returns the host C `stdout` `FILE*` pointer as an opaque `ptr[u8]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_c_stdout() -> *mut u8 {
    // SAFETY: Returns the C runtime standard output stream pointer.
    unsafe { host_std_stream(1).cast::<u8>() }
}

/// Returns the host C `stderr` `FILE*` pointer as an opaque `ptr[u8]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_c_stderr() -> *mut u8 {
    // SAFETY: Returns the C runtime standard error stream pointer.
    unsafe { host_std_stream(2).cast::<u8>() }
}

/// Returns the address of thread-local `errno` (or `__errno_location` on Linux).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn errnoLocation() -> *mut libc::c_int {
    #[cfg(target_os = "linux")]
    unsafe extern "C" {
        fn __errno_location() -> *mut libc::c_int;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        __errno_location()
    }
    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn __error() -> *mut libc::c_int;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        __error()
    }
    #[cfg(windows)]
    unsafe extern "C" {
        fn _errno() -> *mut libc::c_int;
    }
    #[cfg(windows)]
    unsafe {
        _errno()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    std::ptr::null_mut()
}

/// Returns the pointer to the ctype lookup table array (`__ctype_b_loc` on Linux).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ctypeBLoc() -> *const *const u16 {
    #[cfg(target_os = "linux")]
    unsafe extern "C" {
        fn __ctype_b_loc() -> *const *const u16;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        __ctype_b_loc()
    }
    #[cfg(not(target_os = "linux"))]
    std::ptr::null()
}

/// Zero-argument wrapper around C `snprintf(buf, size, fmt)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_snprintf_0(buf: *mut u8, size: usize, fmt: *const u8) -> i32 {
    if fmt.is_null() || (size > 0 && buf.is_null()) {
        return -1;
    }
    // SAFETY: Caller guarantees valid NUL-terminated `fmt` and `buf` of `size` bytes.
    unsafe { libc::snprintf(buf.cast::<libc::c_char>(), size, fmt.cast::<libc::c_char>()) }
}

/// Single `i32` argument wrapper around C `snprintf(buf, size, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_snprintf_i32(
    buf: *mut u8,
    size: usize,
    fmt: *const u8,
    val: i32,
) -> i32 {
    if fmt.is_null() || (size > 0 && buf.is_null()) {
        return -1;
    }
    // SAFETY: Caller guarantees `fmt` accepts one 32-bit integer argument.
    unsafe {
        libc::snprintf(
            buf.cast::<libc::c_char>(),
            size,
            fmt.cast::<libc::c_char>(),
            val,
        )
    }
}

/// Single `i64` argument wrapper around C `snprintf(buf, size, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_snprintf_i64(
    buf: *mut u8,
    size: usize,
    fmt: *const u8,
    val: i64,
) -> i32 {
    if fmt.is_null() || (size > 0 && buf.is_null()) {
        return -1;
    }
    // SAFETY: Caller guarantees `fmt` accepts one 64-bit integer argument.
    unsafe {
        libc::snprintf(
            buf.cast::<libc::c_char>(),
            size,
            fmt.cast::<libc::c_char>(),
            val,
        )
    }
}

/// Single `f64` argument wrapper around C `snprintf(buf, size, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_snprintf_f64(
    buf: *mut u8,
    size: usize,
    fmt: *const u8,
    val: f64,
) -> i32 {
    if fmt.is_null() || (size > 0 && buf.is_null()) {
        return -1;
    }
    // SAFETY: Caller guarantees `fmt` accepts one `double` argument.
    unsafe {
        libc::snprintf(
            buf.cast::<libc::c_char>(),
            size,
            fmt.cast::<libc::c_char>(),
            val,
        )
    }
}

/// Single pointer argument wrapper around C `snprintf(buf, size, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_snprintf_ptr(
    buf: *mut u8,
    size: usize,
    fmt: *const u8,
    val: *const u8,
) -> i32 {
    if fmt.is_null() || (size > 0 && buf.is_null()) {
        return -1;
    }
    // SAFETY: Caller guarantees `fmt` accepts one pointer argument.
    unsafe {
        libc::snprintf(
            buf.cast::<libc::c_char>(),
            size,
            fmt.cast::<libc::c_char>(),
            val,
        )
    }
}

/// Zero-argument wrapper around C `fprintf(stream, fmt)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_fprintf_0(stream: *mut u8, fmt: *const u8) -> i32 {
    if stream.is_null() || fmt.is_null() {
        return -1;
    }
    // SAFETY: Caller guarantees `stream` is a valid `FILE*` and `fmt` is NUL-terminated.
    unsafe { libc::fprintf(stream.cast::<libc::FILE>(), fmt.cast::<libc::c_char>()) }
}

/// Single `i64` argument wrapper around C `fprintf(stream, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_fprintf_i64(stream: *mut u8, fmt: *const u8, val: i64) -> i32 {
    if stream.is_null() || fmt.is_null() {
        return -1;
    }
    // SAFETY: Caller guarantees `stream` is a valid `FILE*` and `fmt` accepts `i64`.
    unsafe { libc::fprintf(stream.cast::<libc::FILE>(), fmt.cast::<libc::c_char>(), val) }
}

/// Single `f64` argument wrapper around C `fprintf(stream, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_fprintf_f64(stream: *mut u8, fmt: *const u8, val: f64) -> i32 {
    if stream.is_null() || fmt.is_null() {
        return -1;
    }
    // SAFETY: Caller guarantees `stream` is a valid `FILE*` and `fmt` accepts `f64`.
    unsafe { libc::fprintf(stream.cast::<libc::FILE>(), fmt.cast::<libc::c_char>(), val) }
}

/// Single pointer argument wrapper around C `fprintf(stream, fmt, val)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_fprintf_ptr(stream: *mut u8, fmt: *const u8, val: *const u8) -> i32 {
    if stream.is_null() || fmt.is_null() {
        return -1;
    }
    // SAFETY: Caller guarantees `stream` is a valid `FILE*` and `fmt` accepts a pointer.
    unsafe { libc::fprintf(stream.cast::<libc::FILE>(), fmt.cast::<libc::c_char>(), val) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_streams_are_non_null() {
        // SAFETY: Querying standard process stream pointers.
        unsafe {
            assert!(!ar_rt_c_stdin().is_null());
            assert!(!ar_rt_c_stdout().is_null());
            assert!(!ar_rt_c_stderr().is_null());
        }
    }

    #[test]
    fn snprintf_wrappers_format_all_variants() {
        let mut buf = [0u8; 64];
        // SAFETY: Buffers and NUL-terminated format strings remain valid for each call.
        unsafe {
            let n0 = ar_rt_snprintf_0(buf.as_mut_ptr(), buf.len(), c"ok".as_ptr().cast::<u8>());
            assert_eq!(n0, 2);
            assert_eq!(&buf[..3], b"ok\0");

            let n1 = ar_rt_snprintf_i32(
                buf.as_mut_ptr(),
                buf.len(),
                c"%d".as_ptr().cast::<u8>(),
                -42,
            );
            assert_eq!(n1, 3);
            assert_eq!(&buf[..4], b"-42\0");

            let n2 = ar_rt_snprintf_i64(
                buf.as_mut_ptr(),
                buf.len(),
                c"%lld".as_ptr().cast::<u8>(),
                1_234_567_890_123_i64,
            );
            assert_eq!(n2, 13);
            assert_eq!(&buf[..14], b"1234567890123\0");

            let n3 = ar_rt_snprintf_f64(
                buf.as_mut_ptr(),
                buf.len(),
                c"%.2f".as_ptr().cast::<u8>(),
                3.25,
            );
            assert_eq!(n3, 4);
            assert_eq!(&buf[..5], b"3.25\0");

            let n4 = ar_rt_snprintf_ptr(
                buf.as_mut_ptr(),
                buf.len(),
                c"%s".as_ptr().cast::<u8>(),
                c"lua".as_ptr().cast::<u8>(),
            );
            assert_eq!(n4, 3);
            assert_eq!(&buf[..4], b"lua\0");

            assert_eq!(
                ar_rt_snprintf_0(std::ptr::null_mut(), 8, c"x".as_ptr().cast()),
                -1
            );
            assert_eq!(
                ar_rt_fprintf_0(std::ptr::null_mut(), c"x".as_ptr().cast()),
                -1
            );
        }
    }
}
