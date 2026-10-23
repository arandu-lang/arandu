/// Fills a buffer exclusively from the operating system entropy source.
/// Returns -1 and clears the buffer if the source fails; never substitutes PRNGs.
///
/// # Safety
/// For nonzero `len`, `buf` must reference `len` writable, exclusively borrowed
/// bytes in a single live allocation. The length must fit `isize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_get_entropy(buf: *mut u8, len: usize) -> i32 {
    if len == 0 {
        return 0;
    }
    if buf.is_null() || len > isize::MAX as usize {
        return -1;
    }
    // SAFETY: the caller supplies a writable live allocation; null and slice
    // length limits are checked above. No host reference survives the call.
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf, len) };
    fill_entropy_with(bytes, os_entropy)
}

pub(super) fn fill_entropy_with(
    bytes: &mut [u8],
    source: impl FnOnce(&mut [u8]) -> std::io::Result<()>,
) -> i32 {
    if source(bytes).is_ok() {
        0
    } else {
        bytes.fill(0);
        -1
    }
}

/// Kernel CSPRNG first (getrandom/getentropy, as Rust std and Go do); the
/// device is only a fallback when that interface is unavailable (old kernel,
/// seccomp). Keep in sync with `ar_rt_get_entropy` in the C runtime.
#[cfg(unix)]
fn os_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    match kernel_entropy(bytes) {
        Ok(()) => Ok(()),
        Err(error) if is_unavailable(&error) => {
            std::fs::File::open("/dev/urandom")?.read_exact(bytes)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn is_unavailable(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM))
}

#[cfg(target_os = "linux")]
fn kernel_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < bytes.len() {
        let rest = &mut bytes[filled..];
        // SAFETY: `rest` is exclusively writable for `rest.len()` bytes.
        let got = unsafe { libc::getrandom(rest.as_mut_ptr().cast(), rest.len(), 0) };
        if got < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        filled += got.unsigned_abs();
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn kernel_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    // getentropy accepts at most 256 bytes per call.
    for chunk in bytes.chunks_mut(256) {
        // SAFETY: `chunk` is exclusively writable for `chunk.len()` bytes.
        if unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
))]
fn kernel_entropy(_bytes: &mut [u8]) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
}

#[cfg(windows)]
fn os_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    // BCrypt accepts a DWORD length. Chunking preserves the whole request.
    for chunk in bytes.chunks_mut(u32::MAX as usize) {
        let length = u32::try_from(chunk.len()).map_err(std::io::Error::other)?;
        // SAFETY: chunk is exclusively writable for length bytes. A null
        // algorithm handle is required with SYSTEM_PREFERRED_RNG.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                length,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(std::io::Error::other("BCryptGenRandom failed"));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn os_entropy(_bytes: &mut [u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no operating system entropy backend",
    ))
}
