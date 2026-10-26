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

use std::io::Read;

mod capability;
#[cfg(any(windows, test))]
mod capability_windows;
mod dir;
mod fd;
mod read;

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

/// Returns whether the stream contains bytes beyond the configured read limit.
fn has_more_data(file: &mut std::fs::File) -> Result<bool, std::io::Error> {
    let mut probe = [0u8; 1];
    read_retry(file, &mut probe).map(|read| read != 0)
}

fn next_read_capacity(capacity: usize) -> usize {
    if capacity == 0 {
        INITIAL_GROWTH_CAPACITY.min(MAX_BUFFER_SIZE)
    } else {
        capacity.saturating_mul(2).min(MAX_BUFFER_SIZE)
    }
}

pub use capability::{
    ar_fs_dir_open, ar_fs_dir_openat, ar_fs_mkdirat, ar_fs_openat, ar_fs_unlinkat,
};
pub use dir::ar_fs_readdir;
pub use fd::{ar_fs_close, ar_fs_open, ar_fs_read, ar_fs_write, exists};
pub use read::ar_fs_read_all;

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
    ),
))]
use capability::capability_open_component;
#[cfg(test)]
use dir::directory_blob_len;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec_runtime::ar_vec_buf_free;
    #[cfg(unix)]
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("arandu_fs_runtime_{}_{id}", std::process::id()))
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn directory_capability_rejects_traversal_symlinks_and_absolute_paths() {
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let base = temp_dir();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("inside"), "inside").unwrap();
        std::fs::write(outside.join("victim"), "outside").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        symlink(outside.join("victim"), root.join("link")).unwrap();
        let root_bytes = root.as_os_str().as_bytes();
        // SAFETY: all buffers and descriptors below are owned by this test and
        // remain live for each host call; each successful descriptor is closed.
        unsafe {
            let fd = ar_fs_dir_open(root_bytes.as_ptr(), root_bytes.len());
            assert!(fd >= 0, "Linux openat2 capability host is required");
            for path in [
                "../outside/victim".to_owned(),
                outside.join("victim").to_str().unwrap().to_owned(),
                "escape/victim".to_owned(),
                "link".to_owned(),
                "nested/../../outside/victim".to_owned(),
                "nested/../inside".to_owned(),
                "inside\0ignored".to_owned(),
            ] {
                assert_eq!(
                    ar_fs_openat(fd, path.as_ptr(), path.len(), true, true, false, true),
                    -1,
                    "{path:?}"
                );
                if path != "link" {
                    assert_eq!(
                        ar_fs_unlinkat(fd, path.as_ptr(), path.len(), false),
                        -1,
                        "{path:?}"
                    );
                }
                // Removing a final symlink removes only that entry, so omit
                // that safe operation from the traversal-rejection assertion.
            }
            assert_eq!(
                std::fs::read_to_string(outside.join("victim")).unwrap(),
                "outside"
            );
            assert_eq!(
                ar_fs_dir_openat(fd, b"inside".as_ptr(), 6),
                -1,
                "a file is not a Dir"
            );
            let child = ar_fs_mkdirat(fd, b"nested/created".as_ptr(), 14);
            assert!(child >= 0);
            ar_fs_close(child);
            assert_eq!(ar_fs_mkdirat(fd, b"../outside/pwn".as_ptr(), 14), -1);
            assert!(!outside.join("pwn").exists());
            let file_fd = ar_fs_openat(fd, b"inside".as_ptr(), 6, true, false, false, false);
            assert!(file_fd >= 0);
            let owned = OwnedFd::from_raw_fd(file_fd);
            let mut file = std::fs::File::from(owned);
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();
            assert_eq!(contents, "inside");
            std::fs::rename(&root, base.join("renamed")).unwrap();
            let file_fd = ar_fs_openat(fd, b"inside".as_ptr(), 6, true, false, false, false);
            assert!(
                file_fd >= 0,
                "renaming retains authority of the pinned handle"
            );
            ar_fs_close(file_fd);
            ar_fs_close(fd);
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    fn single_component_capabilities_reject_traversal_and_pin_object_authority() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::symlink;
        let base = temp_dir();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("child/inside"), "inside").unwrap();
        std::fs::write(outside.join("victim"), "outside").unwrap();
        let root_fd = std::fs::File::open(&root).unwrap();
        for path in [
            "",
            "/victim",
            "..",
            "./child",
            "child/inside",
            "child/../outside",
            "child//inside",
        ] {
            assert!(
                capability_open_component(
                    root_fd.as_raw_fd(),
                    Path::new(path),
                    libc::O_RDONLY,
                    0,
                    false
                )
                .is_none(),
                "{path:?}"
            );
        }
        symlink(&outside, root.join("link")).unwrap();
        symlink(outside.join("victim"), root.join("file-link")).unwrap();
        for (name, flags) in [
            ("link", libc::O_RDONLY | libc::O_DIRECTORY),
            ("file-link", libc::O_WRONLY | libc::O_TRUNC),
        ] {
            assert!(
                capability_open_component(root_fd.as_raw_fd(), Path::new(name), flags, 0, false)
                    .is_none()
            );
        }
        let child = capability_open_component(
            root_fd.as_raw_fd(),
            Path::new("child"),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
            false,
        )
        .unwrap();
        std::fs::rename(root.join("child"), base.join("moved")).unwrap();
        symlink(&outside, root.join("child")).unwrap();
        // A replacement name cannot redirect a previously acquired capability.
        assert!(
            capability_open_component(
                root_fd.as_raw_fd(),
                Path::new("child"),
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
                false
            )
            .is_none()
        );
        let file = capability_open_component(
            child.as_raw_fd(),
            Path::new("inside"),
            libc::O_RDONLY,
            0,
            false,
        )
        .unwrap();
        let mut contents = String::new();
        std::fs::File::from(file)
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "inside");
        assert_eq!(
            std::fs::read_to_string(outside.join("victim")).unwrap(),
            "outside"
        );
        drop(child);
        drop(root_fd);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn capability_read_only_options_never_create_or_truncate() {
        use std::os::unix::ffi::OsStrExt;
        let root = temp_dir();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("inside"), "preserved").unwrap();
        let bytes = root.as_os_str().as_bytes();
        // SAFETY: all path buffers remain live and the returned root is closed.
        unsafe {
            let fd = ar_fs_dir_open(bytes.as_ptr(), bytes.len());
            assert!(fd >= 0);
            for (create, truncate) in [(true, false), (false, true), (true, true)] {
                assert_eq!(
                    ar_fs_openat(fd, b"inside".as_ptr(), 6, true, false, create, truncate),
                    -1
                );
                assert_eq!(
                    ar_fs_openat(fd, b"missing".as_ptr(), 7, true, false, create, truncate),
                    -1
                );
            }
            assert_eq!(
                ar_fs_openat(fd, b"inside".as_ptr(), 6, false, false, false, false),
                -1
            );
            ar_fs_close(fd);
        }
        assert_eq!(
            std::fs::read_to_string(root.join("inside")).unwrap(),
            "preserved"
        );
        assert!(!root.join("missing").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_read_probe_distinguishes_eof_from_trailing_data() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.bin");
        std::fs::write(&path, b"ab").unwrap();

        let mut file = std::fs::File::open(&path).unwrap();
        let mut first = [0u8; 1];
        read_retry(&mut file, &mut first).unwrap();
        assert!(has_more_data(&mut file).unwrap());
        let mut last = [0u8; 1];
        read_retry(&mut file, &mut last).unwrap();
        assert!(!has_more_data(&mut file).unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn directory_blob_preflight_checks_overflow_and_limits() {
        assert!(directory_blob_len(1, 3).is_some());
        assert!(directory_blob_len(usize::MAX, usize::MAX).is_none());
    }

    #[test]
    fn read_capacity_growth_saturates_at_the_abi_limit() {
        assert_eq!(next_read_capacity(0), INITIAL_GROWTH_CAPACITY);
        assert_eq!(next_read_capacity(MAX_BUFFER_SIZE / 2), MAX_BUFFER_SIZE - 1);
        assert_eq!(next_read_capacity(MAX_BUFFER_SIZE / 2 + 1), MAX_BUFFER_SIZE);
        assert_eq!(next_read_capacity(MAX_BUFFER_SIZE), MAX_BUFFER_SIZE);
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
