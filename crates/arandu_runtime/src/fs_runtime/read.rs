use super::{
    ERR_INVALID_INPUT, ERR_OK, ERR_OTHER, MAX_BUFFER_SIZE, has_more_data, io_error_to_portable,
    next_read_capacity, read_retry,
};
use crate::rt_runtime::path_from_fat;
use crate::vec_runtime::{ar_vec_buf_free, ar_vec_malloc, ar_vec_realloc};

/// Reads the whole file into an owned, `ar_vec_malloc`-compatible buffer.
///
/// Returns `(data, len, capacity)`; `data` is `NULL` when the content is empty.
fn read_all_impl(ptr: *const u8, len: isize) -> Result<(*mut u8, usize, usize), isize> {
    let path = path_from_fat(ptr, len).ok_or(ERR_INVALID_INPUT)?;
    let mut file = std::fs::File::open(&path).map_err(|e| io_error_to_portable(&e))?;
    let initial = file.metadata().ok().map_or(0, |meta| {
        usize::try_from(meta.len()).unwrap_or(MAX_BUFFER_SIZE.saturating_add(1))
    });
    if initial > MAX_BUFFER_SIZE {
        return Err(ERR_OTHER);
    }

    let mut capacity = initial;
    // SAFETY: `ar_vec_malloc` returns an owned, 16-aligned buffer or `NULL`.
    let mut data = if capacity == 0 {
        std::ptr::null_mut()
    } else {
        let p = unsafe { ar_vec_malloc(capacity) };
        if p.is_null() {
            return Err(ERR_OTHER);
        }
        p
    };
    let mut filled = 0usize;

    loop {
        if filled == capacity {
            // Tolerates special files whose reported size is 0 or smaller than
            // the actual content (Zig readFileAlloc lesson: grow, don't truncate).
            if capacity >= MAX_BUFFER_SIZE {
                match has_more_data(&mut file) {
                    Ok(false) => break,
                    Ok(true) => {
                        unsafe { ar_vec_buf_free(data, capacity) };
                        return Err(ERR_OTHER);
                    }
                    Err(error) => {
                        unsafe { ar_vec_buf_free(data, capacity) };
                        return Err(io_error_to_portable(&error));
                    }
                }
            }
            let next_capacity = next_read_capacity(capacity);
            let grown = unsafe { ar_vec_realloc(data, capacity, next_capacity) };
            if grown.is_null() {
                unsafe { ar_vec_buf_free(data, capacity) };
                return Err(ERR_OTHER);
            }
            data = grown;
            capacity = next_capacity;
        }
        let read_bytes = {
            // SAFETY: `filled <= capacity` and `data` holds `capacity` bytes.
            let buf =
                unsafe { std::slice::from_raw_parts_mut(data.add(filled), capacity - filled) };
            match read_retry(&mut file, buf) {
                Ok(n) => n,
                Err(error) => {
                    unsafe { ar_vec_buf_free(data, capacity) };
                    return Err(io_error_to_portable(&error));
                }
            }
        };
        if read_bytes == 0 {
            break; // EOF
        }
        filled += read_bytes;
    }

    // Shrink to the exact size so `String{data, len, capacity}` frees correctly
    // and empty content yields `NULL`/`0`/`0` (nothing to free).
    let shrunk = unsafe { ar_vec_realloc(data, capacity, filled) };
    if filled > 0 && shrunk.is_null() {
        // Shrink failed: keep the over-allocated buffer (still valid to free).
        Ok((data, filled, capacity))
    } else {
        Ok((shrunk, filled, filled))
    }
}

/// Host extern entry point for `read_all_impl` (see module docs for ABI).
///
/// # Safety
/// `path` must be a valid fat-string pair from the JIT. All out-pointers must
/// be writable for one machine word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_read_all(
    path_ptr: *const u8,
    path_len: isize,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
    out_cap: *mut usize,
    err: *mut isize,
) {
    if out_buf.is_null() || out_len.is_null() || out_cap.is_null() || err.is_null() {
        return;
    }
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_all_impl(path_ptr, path_len)
    }));
    match res {
        Ok(Ok((data, len, capacity))) => {
            // SAFETY: contract mandates writable out-pointers.
            unsafe {
                *out_buf = data;
                *out_len = len;
                *out_cap = capacity;
                *err = ERR_OK;
            }
        }
        Ok(Err(code)) => {
            // Contract: any error ⇒ NULL/0/0, nothing to free.
            unsafe {
                *out_buf = std::ptr::null_mut();
                *out_len = 0;
                *out_cap = 0;
                *err = code;
            }
        }
        Err(_) => unsafe {
            *out_buf = std::ptr::null_mut();
            *out_len = 0;
            *out_cap = 0;
            *err = ERR_OTHER;
        },
    }
}
