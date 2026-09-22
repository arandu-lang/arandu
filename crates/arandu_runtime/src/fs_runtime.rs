//! FS hosts (Minimal std `std.fs`): owned, zero-copy read of a whole file and
//! of a directory listing.
//!
//! Contract (aligned with the campaign doc `docs/campaigns/fs-env-host-abi-v0.1.md`):
//! - Buffers are allocated with the `ar_vec_malloc` semantics (JIT:
//!   `Layout::from_size_align(n, 8)`; C: `malloc`) and freed by
//!   `ar_vec_buf_free(data, capacity)` with the **exact** capacity returned.
//! - `capacity` may exceed the useful length: special files (e.g. `/proc`) lie
//!   about their size, so the host grows the buffer on the fly (Zig
//!   `readFileAlloc` lesson: stat → exact alloc → read loop; no grow+shrink
//!   copies in Arandu code).
//! - Any error ⇒ `*out_buf == NULL`, `*out_len/*out_count == 0`, `*out_cap == 0`.
//! - Empty file / empty dir ⇒ `NULL`/`0`/`0`, nothing allocated.
//! - Errors are **portable codes** (never raw errno), mapped to `IoErrorKind`
//!   on the Arandu side; `IoError.os_code` carries that same portable code.

use crate::rt_runtime::path_from_fat;
use crate::vec_runtime::{ar_vec_buf_free, ar_vec_malloc, ar_vec_realloc};
use std::io::Read;
use std::path::Path;

/// Max buffer, matching `std.alloc.string.maxBufferCapacity()` (i32::MAX).
const MAX_BUFFER_SIZE: usize = i32::MAX as usize;

/// Header size in bytes for directory serialization: `[u32 count][u32 blob_len]`.
const DIR_BLOB_HEADER_SIZE: usize = 8;
/// Size in bytes of one serialized directory entry:
/// `u32 name_off` + `u32 name_len` + `u8 is_dir` + `u8 kind` + `u16 pad` + 4 padding.
const DIR_ENTRY_SIZE: usize = 16;
/// Initial growth capacity when reading dynamic/special files whose metadata length is 0 (e.g. `/proc`).
const INITIAL_GROWTH_CAPACITY: usize = 8192;

const ERR_OK: isize = 0;
/// `IoErrorKind::NotFound`.
const ERR_NOT_FOUND: isize = 1;
/// `IoErrorKind::PermissionDenied`.
const ERR_PERMISSION_DENIED: isize = 2;
/// `IoErrorKind::AlreadyExists`.
const ERR_ALREADY_EXISTS: isize = 3;
/// `IoErrorKind::WouldBlock`.
const ERR_WOULD_BLOCK: isize = 4;
/// `IoErrorKind::InvalidInput`.
const ERR_INVALID_INPUT: isize = 5;
/// `IoErrorKind::BrokenPipe`.
const ERR_BROKEN_PIPE: isize = 6;
/// `IoErrorKind::UnexpectedEof`.
const ERR_UNEXPECTED_EOF: isize = 7;
/// `IoErrorKind::Other`.
const ERR_OTHER: isize = 8;

/// Maps a `std::io::ErrorKind` to the portable error code contract.
fn io_error_to_portable(error: &std::io::Error) -> isize {
    match error.kind() {
        std::io::ErrorKind::NotFound => ERR_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => ERR_PERMISSION_DENIED,
        std::io::ErrorKind::AlreadyExists => ERR_ALREADY_EXISTS,
        std::io::ErrorKind::WouldBlock => ERR_WOULD_BLOCK,
        std::io::ErrorKind::InvalidInput => ERR_INVALID_INPUT,
        std::io::ErrorKind::BrokenPipe => ERR_BROKEN_PIPE,
        std::io::ErrorKind::UnexpectedEof => ERR_UNEXPECTED_EOF,
        _ => ERR_OTHER,
    }
}

/// Single read; retries `Interrupted` (Rust `fs::read_to_string` semantics).
fn read_retry(file: &mut std::fs::File, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    loop {
        match file.read(buf) {
            Ok(n) => return Ok(n),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Reads the whole file into an owned, `ar_vec_malloc`-compatible buffer.
///
/// Returns `(data, len, capacity)`; `data` is `NULL` when the content is empty.
fn read_all_impl(ptr: *const u8, len: isize) -> Result<(*mut u8, usize, usize), isize> {
    let path = path_from_fat(ptr, len).ok_or(ERR_INVALID_INPUT)?;
    let mut file = std::fs::File::open(&path).map_err(|e| io_error_to_portable(&e))?;
    let initial = file
        .metadata()
        .map(|meta| meta.len() as usize)
        .ok()
        .unwrap_or(0);
    if initial > MAX_BUFFER_SIZE {
        return Err(ERR_OTHER);
    }

    let mut capacity = initial;
    // SAFETY: `ar_vec_malloc` returns an owned, 8-aligned buffer or `NULL`.
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
                break;
            }
            let next_capacity = if capacity == 0 {
                INITIAL_GROWTH_CAPACITY.min(MAX_BUFFER_SIZE)
            } else {
                (capacity * 2).min(MAX_BUFFER_SIZE)
            };
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
    match read_all_impl(path_ptr, path_len) {
        Ok((data, len, capacity)) => {
            // SAFETY: contract mandates writable out-pointers.
            unsafe {
                *out_buf = data;
                *out_len = len;
                *out_cap = capacity;
                *err = ERR_OK;
            }
        }
        Err(code) => {
            // Contract: any error ⇒ NULL/0/0, nothing to free.
            unsafe {
                *out_buf = std::ptr::null_mut();
                *out_len = 0;
                *out_cap = 0;
                *err = code;
            }
        }
    }
}

/// Host-side entry: raw name bytes + cheap directory flag from `file_type`
/// (no full stat — the Go `os.ReadDir`/`DirEntry` lesson).
struct HostEntry {
    name: Vec<u8>,
    is_dir: bool,
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
    for item in iterator {
        let dir_entry = item.map_err(|e| io_error_to_portable(&e))?;
        let file_type = dir_entry
            .file_type()
            .map_err(|e| io_error_to_portable(&e))?;
        entries.push(HostEntry {
            // Lossy on purpose: never panic on non-UTF-8 names (Rust env::args lesson).
            name: dir_entry
                .file_name()
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
            is_dir: file_type.is_dir(),
        });
    }

    let count = entries.len();
    if count == 0 {
        // Contract: empty dir ⇒ NULL/0/0, nothing to free.
        return Ok((std::ptr::null_mut(), 0, 0));
    }

    let names_len: usize = entries.iter().map(|e| e.name.len()).sum();
    let entry_table = count.checked_mul(DIR_ENTRY_SIZE).ok_or(ERR_OTHER)?;
    let descriptor_size = std::mem::size_of::<usize>() * 2;
    let descriptor_table = count.checked_mul(descriptor_size).ok_or(ERR_OTHER)?;
    let blob_len = DIR_BLOB_HEADER_SIZE
        .checked_add(entry_table)
        .and_then(|v| v.checked_add(descriptor_table))
        .and_then(|v| v.checked_add(names_len))
        .ok_or(ERR_OTHER)?;
    if blob_len > MAX_BUFFER_SIZE || blob_len > u32::MAX as usize {
        return Err(ERR_OTHER);
    }

    // SAFETY: `blob_len` bytes, writable, 8-aligned by `ar_vec_malloc`.
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
    match read_dir_impl(path_ptr, path_len) {
        Ok((data, count, capacity)) => {
            // SAFETY: contract mandates writable out-pointers.
            unsafe {
                *out_buf = data;
                *out_count = count;
                *out_cap = capacity;
                *err = ERR_OK;
            }
        }
        Err(code) => {
            // Contract: any error ⇒ NULL/0/0, nothing to free.
            unsafe {
                *out_buf = std::ptr::null_mut();
                *out_count = 0;
                *out_cap = 0;
                *err = code;
            }
        }
    }
}

/// `fs.fileExists(path)` — returns 1 when `path` exists (any file type),
/// 0 otherwise. Matches Go `os.Stat`-style existence probing; never errors.
///
/// # Safety
/// `path_ptr`/`path_len` must be a valid fat-string pair from the JIT or null/empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn exists(path_ptr: *const u8, path_len: isize) -> isize {
    match path_from_fat(path_ptr, path_len) {
        Some(path) => isize::from(Path::new(&path).exists()),
        None => 0,
    }
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

/// Reserved for the (out-of-Minimal) `File` read cycle: deterministic `-1`.
///
/// # Safety
/// Never dereferences pointers; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_read(_fd: i32, _buf: *mut u8, _count: usize) -> isize {
    -1
}

/// Reserved for the (out-of-Minimal) `File` write cycle: deterministic `-1`.
///
/// # Safety
/// Never dereferences pointers; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_write(_fd: i32, _buf: *const u8, _count: usize) -> isize {
    -1
}

/// Frees nothing (fds are never handed out in Minimal 0.1); returns 0.
///
/// # Safety
/// No pointer args; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_close(_fd: i32) -> isize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("arandu_fs_runtime_{}_{id}", std::process::id()))
    }

    /// Keeps the lossy string alive while calling a host with a fat-ptr pair.
    struct FatStr {
        _storage: String,
        ptr: *const u8,
        len: isize,
    }

    impl FatStr {
        fn new(path: &std::path::Path) -> Self {
            let storage = path.to_string_lossy().into_owned();
            let ptr = storage.as_ptr();
            let len = storage.len() as isize;
            Self {
                _storage: storage,
                ptr,
                len,
            }
        }
    }

    #[test]
    fn read_all_roundtrip_and_empty() {
        let text = b"arandu fs read_all \xf0\x9f\x98\x80";
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), text).unwrap();
        std::fs::write(dir.join("empty.bin"), []).unwrap();

        let fat = FatStr::new(&dir.join("a.txt"));
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        let mut cap = 0usize;
        let mut err = ERR_OTHER;
        unsafe {
            ar_fs_read_all(fat.ptr, fat.len, &mut buf, &mut len, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_OK);
        assert!(!buf.is_null());
        assert_eq!(len, text.len());
        assert_eq!(cap, len, "capacity must equal len for regular files");
        let bytes = unsafe { std::slice::from_raw_parts(buf, len) };
        assert_eq!(bytes, text);
        unsafe { ar_vec_buf_free(buf, cap) };

        // Empty file ⇒ NULL/0/0 (validates the adopt/leak-free path).
        let fat = FatStr::new(&dir.join("empty.bin"));
        buf = std::ptr::null_mut();
        len = 0;
        cap = 0;
        unsafe {
            ar_fs_read_all(fat.ptr, fat.len, &mut buf, &mut len, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_OK);
        assert!(buf.is_null());
        assert_eq!((len, cap), (0, 0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_all_not_found_and_invalid() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("nope.txt");
        let fat = FatStr::new(&missing);
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        let mut cap = 0usize;
        let mut err = ERR_OK;
        unsafe {
            ar_fs_read_all(fat.ptr, fat.len, &mut buf, &mut len, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_NOT_FOUND);
        assert!(buf.is_null());
        assert_eq!((len, cap), (0, 0));

        // Garbage fat string ⇒ InvalidInput, no panic.
        unsafe {
            ar_fs_read_all(std::ptr::null(), -1, &mut buf, &mut len, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_INVALID_INPUT);
        assert!(buf.is_null());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_dir_lists_files_and_subdirs() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("f1.txt"), b"x").unwrap();
        std::fs::write(dir.join("f2.txt"), b"yy").unwrap();

        let fat = FatStr::new(&dir);
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut count = 0usize;
        let mut cap = 0usize;
        let mut err = ERR_OTHER;
        unsafe {
            ar_fs_readdir(fat.ptr, fat.len, &mut buf, &mut count, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_OK);
        assert_eq!(count, 3, "f1.txt + f2.txt + sub");
        assert!(!buf.is_null());
        let descriptor_size = std::mem::size_of::<usize>() * 2;
        assert!(cap >= 8 + count * (16 + descriptor_size));

        let bytes = unsafe { std::slice::from_raw_parts(buf, cap) };
        let descriptors_base = 8 + count * 16;
        let names_base = descriptors_base + count * descriptor_size;
        let mut names = std::collections::HashSet::new();
        let mut has_sub = false;
        for i in 0..count {
            let base = 8 + i * 16;
            let off = u32::from_le_bytes(bytes[base..base + 4].try_into().unwrap()) as usize;
            let nlen = u32::from_le_bytes(bytes[base + 4..base + 8].try_into().unwrap()) as usize;
            let is_dir = bytes[base + 8] == 1;
            let descriptor = descriptors_base + i * descriptor_size;
            let ptr = usize::from_ne_bytes(
                bytes[descriptor..descriptor + std::mem::size_of::<usize>()]
                    .try_into()
                    .unwrap(),
            );
            let len = usize::from_ne_bytes(
                bytes[descriptor + std::mem::size_of::<usize>()..descriptor + descriptor_size]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(ptr, buf.wrapping_add(names_base + off) as usize);
            assert_eq!(len, nlen);
            let name = String::from_utf8_lossy(&bytes[names_base + off..names_base + off + nlen]);
            names.insert(name.to_string());
            if name == "sub" {
                assert!(is_dir);
                has_sub = true;
            } else if name == "f1.txt" || name == "f2.txt" {
                assert!(!is_dir);
            }
        }
        assert!(names.contains("f1.txt") && names.contains("f2.txt") && has_sub);
        unsafe { ar_vec_buf_free(buf, cap) };

        // Empty dir ⇒ NULL/0/0.
        let empty_sub = dir.join("empty_sub");
        std::fs::create_dir_all(&empty_sub).unwrap();
        let fat = FatStr::new(&empty_sub);
        buf = std::ptr::null_mut();
        count = 0;
        cap = 0;
        unsafe {
            ar_fs_readdir(fat.ptr, fat.len, &mut buf, &mut count, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_OK);
        assert!(buf.is_null());
        assert_eq!((count, cap), (0, 0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_dir_errors() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing_dir");
        let fat = FatStr::new(&missing);
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut count = 0usize;
        let mut cap = 0usize;
        let mut err = ERR_OK;
        unsafe {
            ar_fs_readdir(fat.ptr, fat.len, &mut buf, &mut count, &mut cap, &mut err);
        }
        assert_eq!(err, ERR_NOT_FOUND);
        assert!(buf.is_null());
        assert_eq!((count, cap), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn null_out_pointers_are_safely_ignored() {
        let text = b"safe";
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("safe.txt"), text).unwrap();
        let fat = FatStr::new(&dir.join("safe.txt"));
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        let mut cap = 0usize;

        unsafe {
            // Null err pointer should safely return without dereference
            ar_fs_read_all(
                fat.ptr,
                fat.len,
                &mut buf,
                &mut len,
                &mut cap,
                std::ptr::null_mut(),
            );
            // Null out_buf should safely return
            ar_fs_readdir(
                fat.ptr,
                fat.len,
                std::ptr::null_mut(),
                &mut len,
                &mut cap,
                std::ptr::null_mut(),
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
