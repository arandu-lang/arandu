use crate::rt_runtime::path_from_fat;
use std::path::Path;

/// `fs.fileExists(path)` — returns 1 when `path` exists (any file type),
/// 0 otherwise. Matches Go `os.Stat`-style existence probing; never errors.
///
/// # Safety
/// `path_ptr`/`path_len` must be a valid fat-string pair from the JIT or null/empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn exists(path_ptr: *const u8, path_len: isize) -> isize {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        match path_from_fat(path_ptr, path_len) {
            Some(path) => isize::from(Path::new(&path).exists()),
            None => 0,
        }
    }))
    .unwrap_or(0)
}

/// Reserved for the (out-of-Minimal) `File` open cycle: always yields a
/// deterministic `-1` so Arandu code can never operate on a bogus descriptor.
/// Minimal 0.1 implements whole-file reads (`ar_fs_read_all`), not seekable
/// streams.
///
/// # Safety
/// Never dereferences pointers; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_open(
    _path_ptr: *const u8,
    _path_len: isize,
    _flags: i32,
    _mode: i32,
) -> isize {
    -1
}

/// Reads up to `count` bytes from open descriptor `fd` into `buf`.
///
/// # Safety
/// For nonzero `count`, `buf` must reference `count` writable bytes in a live allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_read(fd: i32, buf: *mut u8, count: usize) -> isize {
    if fd < 0 || (count > 0 && buf.is_null()) || count > isize::MAX as usize {
        return -1;
    }
    if count == 0 {
        return 0;
    }
    #[cfg(unix)]
    {
        loop {
            let res = unsafe { libc::read(fd, buf as *mut libc::c_void, count) };
            if res < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return -1;
            }
            return res as isize;
        }
    }
    #[cfg(windows)]
    {
        // Windows tokens are CRT descriptors owning a HANDLE (RFC 0016); the
        // CRT count is 32-bit, so clamp like the C PAL instead of truncating.
        let count = libc::c_uint::try_from(count.min(i32::MAX as usize)).unwrap_or(0);
        // SAFETY: buf holds at least count writable bytes (see contract).
        let res = unsafe { libc::read(fd, buf.cast(), count) };
        if res < 0 { -1 } else { res as isize }
    }
    #[cfg(not(any(unix, windows)))]
    {
        -1
    }
}

/// Writes up to `count` bytes to open descriptor `fd` from `buf`.
///
/// # Safety
/// For nonzero `count`, `buf` must reference `count` readable bytes in a live allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_write(fd: i32, buf: *const u8, count: usize) -> isize {
    if fd < 0 || (count > 0 && buf.is_null()) || count > isize::MAX as usize {
        return -1;
    }
    if count == 0 {
        return 0;
    }
    #[cfg(unix)]
    {
        loop {
            let res = unsafe { libc::write(fd, buf as *const libc::c_void, count) };
            if res < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return -1;
            }
            return res as isize;
        }
    }
    #[cfg(windows)]
    {
        let count = libc::c_uint::try_from(count.min(i32::MAX as usize)).unwrap_or(0);
        // SAFETY: buf holds at least count readable bytes (see contract).
        let res = unsafe { libc::write(fd, buf.cast(), count) };
        if res < 0 { -1 } else { res as isize }
    }
    #[cfg(not(any(unix, windows)))]
    {
        -1
    }
}

/// Closes an open file or directory descriptor; returns 0.
///
/// # Safety
/// Safe to call with any descriptor; closes valid fds >= 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_close(fd: i32) -> isize {
    #[cfg(unix)]
    {
        if fd >= 0 {
            unsafe { libc::close(fd) };
        }
    }
    #[cfg(windows)]
    {
        if fd >= 0 {
            // SAFETY: `_close` releases the CRT slot and its owned HANDLE once;
            // Arandu invalidates the token before any second close.
            unsafe { libc::close(fd) };
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fd;
    }
    0
}
