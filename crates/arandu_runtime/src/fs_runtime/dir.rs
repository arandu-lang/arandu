use super::{
    DIR_BLOB_HEADER_SIZE, DIR_ENTRY_SIZE, ERR_INVALID_INPUT, ERR_OK, ERR_OTHER, MAX_BUFFER_SIZE,
    io_error_to_portable,
};
use crate::rt_runtime::path_from_fat;
use crate::vec_runtime::ar_vec_malloc;

/// Host-side entry: raw name bytes + cheap directory flag from `file_type`
/// (no full stat — the Go `os.ReadDir`/`DirEntry` lesson).
struct HostEntry {
    name: Vec<u8>,
    is_dir: bool,
}

pub(super) fn directory_blob_len(count: usize, names_len: usize) -> Option<usize> {
    let entry_table = count.checked_mul(DIR_ENTRY_SIZE)?;
    let descriptor_size = std::mem::size_of::<usize>().checked_mul(2)?;
    let descriptor_table = count.checked_mul(descriptor_size)?;
    let blob_len = DIR_BLOB_HEADER_SIZE
        .checked_add(entry_table)?
        .checked_add(descriptor_table)?
        .checked_add(names_len)?;
    (blob_len <= MAX_BUFFER_SIZE && blob_len <= u32::MAX as usize).then_some(blob_len)
}

/// Reads a directory into a single blob:
/// `[u32 count][u32 blob_len][entry × count][descriptor × count][names]`.
///
/// Each entry is 16 bytes: `u32 name_off` + `u32 name_len` + `u8 is_dir` +
/// `u8 kind` (reserved, 0) + `u16 pad`. Each descriptor is a native pointer
/// followed by a native `usize` byte length, matching a borrowed `str` view.
/// `name_off` is the byte offset from the start of the names region (no
/// ordering, no eager stat — Go `os.ReadDir` lesson). `blob_len` is the total
/// allocation size for `ar_vec_buf_free`.
///
/// Returns `(data, count, capacity)`; `data` is `NULL` for an empty directory.
fn read_dir_impl(ptr: *const u8, len: isize) -> Result<(*mut u8, usize, usize), isize> {
    let path = path_from_fat(ptr, len).ok_or(ERR_INVALID_INPUT)?;
    let iterator = std::fs::read_dir(&path).map_err(|e| io_error_to_portable(&e))?;
    let mut entries: Vec<HostEntry> = Vec::new();
    let mut names_len = 0usize;
    for item in iterator {
        let dir_entry = item.map_err(|e| io_error_to_portable(&e))?;
        let file_type = dir_entry
            .file_type()
            .map_err(|e| io_error_to_portable(&e))?;
        let name = dir_entry
            .file_name()
            .to_string_lossy()
            .into_owned()
            .into_bytes();
        names_len = names_len.checked_add(name.len()).ok_or(ERR_OTHER)?;
        let next_count = entries.len().checked_add(1).ok_or(ERR_OTHER)?;
        if directory_blob_len(next_count, names_len).is_none() {
            return Err(ERR_OTHER);
        }
        entries.push(HostEntry {
            // Lossy on purpose: never panic on non-UTF-8 names (Rust env::args lesson).
            name,
            is_dir: file_type.is_dir(),
        });
    }

    let count = entries.len();
    if count == 0 {
        // Contract: empty dir ⇒ NULL/0/0, nothing to free.
        return Ok((std::ptr::null_mut(), 0, 0));
    }

    let entry_table = count.checked_mul(DIR_ENTRY_SIZE).ok_or(ERR_OTHER)?;
    let descriptor_size = std::mem::size_of::<usize>() * 2;
    let descriptor_table = count.checked_mul(descriptor_size).ok_or(ERR_OTHER)?;
    let blob_len = directory_blob_len(count, names_len).ok_or(ERR_OTHER)?;

    // SAFETY: `blob_len` bytes, writable, 16-aligned by `ar_vec_malloc`.
    let data = unsafe { ar_vec_malloc(blob_len) };
    if data.is_null() {
        return Err(ERR_OTHER);
    }
    // SAFETY: `data` holds `blob_len` writable bytes.
    let bytes = unsafe { std::slice::from_raw_parts_mut(data, blob_len) };
    bytes[0..4].copy_from_slice(&(count as u32).to_le_bytes());
    bytes[4..8].copy_from_slice(&(blob_len as u32).to_le_bytes());
    let descriptors_base = DIR_BLOB_HEADER_SIZE + entry_table;
    let names_base = descriptors_base + descriptor_table;
    let mut cursor = names_base;
    for (i, entry) in entries.iter().enumerate() {
        let base = DIR_BLOB_HEADER_SIZE + i * DIR_ENTRY_SIZE;
        let name_off = cursor - names_base;
        bytes[base..base + 4].copy_from_slice(&(name_off as u32).to_le_bytes());
        bytes[base + 4..base + 8].copy_from_slice(&(entry.name.len() as u32).to_le_bytes());
        bytes[base + 8] = u8::from(entry.is_dir);
        bytes[base + 9..base + 16].fill(0);
        let descriptor = descriptors_base + i * descriptor_size;
        let name_ptr = data.wrapping_add(cursor) as usize;
        bytes[descriptor..descriptor + std::mem::size_of::<usize>()]
            .copy_from_slice(&name_ptr.to_ne_bytes());
        bytes[descriptor + std::mem::size_of::<usize>()..descriptor + descriptor_size]
            .copy_from_slice(&entry.name.len().to_ne_bytes());
        bytes[cursor..cursor + entry.name.len()].copy_from_slice(&entry.name);
        cursor += entry.name.len();
    }

    Ok((data, count, blob_len))
}

/// Host extern entry point for `read_dir_impl` (see module docs for ABI).
///
/// # Safety
/// Fat-string ABI for `path`; all out-pointers must be writable for one word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_readdir(
    path_ptr: *const u8,
    path_len: isize,
    out_buf: *mut *mut u8,
    out_count: *mut usize,
    out_cap: *mut usize,
    err: *mut isize,
) {
    if out_buf.is_null() || out_count.is_null() || out_cap.is_null() || err.is_null() {
        return;
    }
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_dir_impl(path_ptr, path_len)
    }));
    match res {
        Ok(Ok((data, count, capacity))) => {
            // SAFETY: contract mandates writable out-pointers.
            unsafe {
                *out_buf = data;
                *out_count = count;
                *out_cap = capacity;
                *err = ERR_OK;
            }
        }
        Ok(Err(code)) => {
            // Contract: any error ⇒ NULL/0/0, nothing to free.
            unsafe {
                *out_buf = std::ptr::null_mut();
                *out_count = 0;
                *out_cap = 0;
                *err = code;
            }
        }
        Err(_) => unsafe {
            *out_buf = std::ptr::null_mut();
            *out_count = 0;
            *out_cap = 0;
            *err = ERR_OTHER;
        },
    }
}
