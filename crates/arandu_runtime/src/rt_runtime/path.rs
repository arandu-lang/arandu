/// Path absolute check for SL_S / Minimal path helpers.
///
/// Uses the host [`std::path::Path::is_absolute`] semantics (Unix `/…`, Windows
/// drive/UNC). Empty and invalid UTF-8 are never absolute.
///
/// # Safety
/// `ptr`/`len` fat string from Arandu JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_is_absolute(ptr: *const u8, len: isize) -> isize {
    if len <= 0 || ptr.is_null() {
        return 0;
    }
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let Ok(text) = std::str::from_utf8(s) else {
        return 0;
    };
    isize::from(std::path::Path::new(text).is_absolute())
}

/// Path empty check.
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_is_empty(_ptr: *const u8, len: isize) -> isize {
    isize::from(len <= 0)
}

/// Fat `str` return for path hosts (matches LayoutEngine / Cranelift multi-value).
///
/// On System V x86_64, two pointer-width fields return in the same registers as
/// Cranelift multi-value `(ptr, len)`.
#[repr(C)]
pub struct ArFatStr {
    pub ptr: *mut u8,
    pub len: isize,
}

pub(crate) fn fat_str_from_string(s: String) -> ArFatStr {
    let len = s.len() as isize;
    // Boxed and never freed *here*: the pointer crosses a host boundary with
    // no language-visible owner at this frame, so its lifetime must be
    // analyzed on its own. Different policy from ToStr / interpolation
    // buffers, which the caller owns and drop elaboration pairs with
    // `Free(str)`.
    let boxed = s.into_boxed_str();
    let ptr = Box::into_raw(boxed) as *mut u8;
    ArFatStr { ptr, len }
}

pub(crate) fn path_from_fat(ptr: *const u8, len: isize) -> Option<std::path::PathBuf> {
    if len < 0 || (len > 0 && ptr.is_null()) {
        return None;
    }
    if len == 0 {
        return Some(std::path::PathBuf::new());
    }
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let text = std::str::from_utf8(s).ok()?;
    Some(std::path::PathBuf::from(text))
}

/// Join two path segments (`std::path::Path::join`).
///
/// # Safety
/// Fat string ABI for both inputs; returns malloc-style owned buffer.
unsafe fn ar_path_join_impl(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    let a = path_from_fat(a_ptr, a_len).unwrap_or_default();
    let b = path_from_fat(b_ptr, b_len).unwrap_or_default();
    let joined = a.join(b);
    fat_str_from_string(joined.to_string_lossy().into_owned())
}

/// Join two paths into an exact-size buffer owned by the caller.
///
/// # Safety
/// Input pairs must satisfy the fat-string ABI and all out-pointers must be
/// writable. A successful non-empty buffer must be released with
/// `ar_vec_buf_free(ptr, capacity)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_join_owned(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
    out_cap: *mut usize,
) -> i8 {
    if out_buf.is_null() || out_len.is_null() || out_cap.is_null() {
        return 0;
    }
    unsafe {
        *out_buf = std::ptr::null_mut();
        *out_len = 0;
        *out_cap = 0;
    }
    let (Some(a), Some(b)) = (path_from_fat(a_ptr, a_len), path_from_fat(b_ptr, b_len)) else {
        return 0;
    };
    let joined = a.join(b);
    let Some(bytes) = joined.to_str().map(str::as_bytes) else {
        return 0;
    };
    if bytes.is_empty() {
        return 1;
    }
    let data = unsafe { crate::vec_runtime::ar_vec_malloc(bytes.len()) };
    if data.is_null() {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        *out_buf = data;
        *out_len = bytes.len();
        *out_cap = bytes.len();
    }
    1
}

/// File name component (`Path::file_name`); empty string when none.
///
/// # Safety
/// Fat string ABI.
unsafe fn ar_path_file_name_impl(ptr: *const u8, len: isize) -> ArFatStr {
    let Some(path) = path_from_fat(ptr, len) else {
        return fat_str_from_string(String::new());
    };
    match path.file_name() {
        Some(name) => fat_str_from_string(name.to_string_lossy().into_owned()),
        None => fat_str_from_string(String::new()),
    }
}

fn slice_from_fat(ptr: *const u8, len: isize) -> &'static [u8] {
    if len <= 0 || ptr.is_null() {
        return b"";
    }
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }
}

/// Concatenate two fat strings (malloc-style process-lifetime buffer).
///
/// # Safety
/// Fat string ABI for both inputs.
unsafe fn ar_str_concat_impl(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    let a = slice_from_fat(a_ptr, a_len);
    let b = slice_from_fat(b_ptr, b_len);
    let total = a.len() + b.len();
    let len = isize::try_from(total).unwrap_or(isize::MAX);
    // The documented contract is a malloc-style buffer, because Arandu frees
    // runtime strings through libc `free`. Allocating through the Rust global
    // allocator here would hand a pointer to `free` that it cannot release.
    let ptr = unsafe { crate::vec_runtime::ar_rt_raw_malloc(total.max(1)) };
    if ptr.is_null() {
        std::process::abort();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(a.as_ptr(), ptr, a.len());
        std::ptr::copy_nonoverlapping(b.as_ptr(), ptr.add(a.len()), b.len());
    }
    ArFatStr { ptr, len }
}

/// Bytes after the last occurrence of `sep` (byte-wise). Empty sep → full `s`.
///
/// # Safety
/// Fat string ABI.
unsafe fn ar_str_split_last_impl(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    let s = slice_from_fat(s_ptr, s_len);
    let sep = slice_from_fat(sep_ptr, sep_len);
    if sep.is_empty() {
        return fat_str_from_string(String::from_utf8_lossy(s).into_owned());
    }
    if let Some(pos) = s.windows(sep.len()).rposition(|w| w == sep) {
        let after = &s[pos + sep.len()..];
        return fat_str_from_string(String::from_utf8_lossy(after).into_owned());
    }
    fat_str_from_string(String::from_utf8_lossy(s).into_owned())
}

// Cranelift represents `str` as two return registers. Windows x64's C ABI
// returns this 16-byte struct indirectly, whereas SysV returns it in RAX/RDX.
// Keep the exported JIT boundary on SysV there; native Rust callers use the
// platform ABI only through the private implementations above.
/// Join two valid fat-string paths.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[cfg(not(windows))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_join(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_path_join_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(windows)]
/// Join two valid fat-string paths using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_path_join(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_path_join_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(not(windows))]
/// Return the final component of a valid fat-string path.
///
/// # Safety
/// `ptr` and `len` must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_file_name(ptr: *const u8, len: isize) -> ArFatStr {
    unsafe { ar_path_file_name_impl(ptr, len) }
}

#[cfg(windows)]
/// Return the final path component using the System V ABI expected by JIT code.
///
/// # Safety
/// `ptr` and `len` must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_path_file_name(ptr: *const u8, len: isize) -> ArFatStr {
    unsafe { ar_path_file_name_impl(ptr, len) }
}

#[cfg(not(windows))]
/// Concatenate two valid fat strings.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_concat(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_str_concat_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(windows)]
/// Concatenate two fat strings using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_str_concat(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_str_concat_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(not(windows))]
/// Return the suffix after the final occurrence of `sep`.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_split_last(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    unsafe { ar_str_split_last_impl(s_ptr, s_len, sep_ptr, sep_len) }
}

#[cfg(windows)]
/// Split a fat string using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_str_split_last(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    unsafe { ar_str_split_last_impl(s_ptr, s_len, sep_ptr, sep_len) }
}
