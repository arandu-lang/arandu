//! RFC 0016 `Dir` capabilities on Windows NT (ST.12b).
//!
//! Token ABI: a `Dir`/`File` token is a CRT descriptor produced by
//! `_open_osfhandle`. The CRT owns the `HANDLE` and `_close` releases it once;
//! the `HANDLE` itself never travels through the 32-bit `int` used by the
//! Arandu ABI, so nothing is truncated. Linux/macOS keep their raw fds.
//!
//! Relative operations accept exactly one component and run `NtCreateFile`
//! with `RootDirectory` set to the pinned directory handle, `OBJ_DONT_REPARSE`
//! and `FILE_OPEN_REPARSE_POINT`; the opened object is then checked so a
//! reparse point, device or wrong object kind fails closed.
//! The C backend mirrors this file in `arandu_backend_c` (`fs_cap_windows.c`).

/// Reserved Win32 device base names, compared ASCII-case-insensitively.
#[cfg(any(windows, test))]
const RESERVED_DEVICE_NAMES: [&[u8]; 7] = [
    b"CON", b"PRN", b"AUX", b"NUL", b"CONIN$", b"CONOUT$", b"CLOCK$",
];

/// Returns whether Win32 would map `name` to a DOS device. The base name ends
/// at the first dot and ignores trailing spaces (`"COM1 .txt"`), and `COM`/`LPT`
/// accept a digit or a superscript one to three (Go CVE-2023-45284).
#[cfg(any(windows, test))]
fn is_reserved_device_name(name: &[u8]) -> bool {
    let mut base = name
        .iter()
        .position(|byte| *byte == b'.')
        .unwrap_or(name.len());
    while base > 0 && name[base - 1] == b' ' {
        base -= 1;
    }
    let base = &name[..base];
    if RESERVED_DEVICE_NAMES
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
    {
        return true;
    }
    let Some(prefix) = base.get(..3) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(b"COM") && !prefix.eq_ignore_ascii_case(b"LPT") {
        return false;
    }
    matches!(
        &base[3..],
        [b'0'..=b'9'] | [0xC2, 0xB9] | [0xC2, 0xB2] | [0xC2, 0xB3]
    )
}

/// Validates one capability component for Windows. Separators of both kinds,
/// drive/stream colons, wildcards, control characters, `.`/`..`, trailing
/// dot/space (Win32 strips them, creating aliases) and device names are
/// rejected; absolute, UNC, `\\?\` and `\??\` forms therefore never pass.
#[cfg(any(windows, test))]
pub(super) fn windows_component_valid(name: &[u8], allow_dot: bool) -> bool {
    if allow_dot && name == b"." {
        return true;
    }
    if name.is_empty() || name.len() >= 4096 || name == b"." || name == b".." {
        return false;
    }
    if std::str::from_utf8(name).is_err() {
        return false;
    }
    if name.iter().any(|byte| {
        *byte < 0x20
            || matches!(
                byte,
                b'\\' | b'/' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|'
            )
    }) {
        return false;
    }
    if matches!(name.last(), Some(b'.' | b' ')) {
        return false;
    }
    !is_reserved_device_name(name)
}

#[cfg(windows)]
mod nt {
    use super::windows_component_valid;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE, FILE_DISPOSITION_INFORMATION,
        FILE_DISPOSITION_INFORMATION_EX, FILE_DISPOSITION_POSIX_SEMANTICS, FILE_NON_DIRECTORY_FILE,
        FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        FileDispositionInformation, FileDispositionInformationEx, NtCreateFile,
        NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::{
        HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE,
        STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_TYPE_DISK,
        GetFileInformationByHandle, GetFileType, OPEN_EXISTING, SYNCHRONIZE, SetEndOfFile,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const DIR_ACCESS: u32 =
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Dir,
        File,
    }

    /// Borrows the HANDLE owned by a CRT descriptor. The CRT keeps ownership.
    fn root_handle(fd: i32) -> Option<HANDLE> {
        if fd < 0 {
            return None;
        }
        // SAFETY: Arandu only forwards descriptors it received from this
        // bridge; `_get_osfhandle` reports unknown descriptors as -1/-2.
        let raw = unsafe { libc::get_osfhandle(fd) };
        if raw == -1 || raw == -2 {
            return None;
        }
        Some(raw as HANDLE)
    }

    /// Accepts only disk objects of the expected kind; reparse points are
    /// rejected unless the caller is removing that link entry itself.
    fn object_matches(handle: HANDLE, kind: Kind, allow_reparse: bool) -> bool {
        // SAFETY: the handle is live for this call and the record is output.
        if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
            return false;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: as above; the record is a plain output buffer.
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return false;
        }
        let attributes = info.dwFileAttributes;
        if !allow_reparse && attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
        (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) == (kind == Kind::Dir)
    }

    fn checked(handle: OwnedHandle, kind: Kind, allow_reparse: bool) -> Option<OwnedHandle> {
        object_matches(handle.as_raw_handle() as HANDLE, kind, allow_reparse).then_some(handle)
    }

    /// Transfers the HANDLE to the CRT descriptor table without truncation.
    fn adopt(handle: OwnedHandle) -> Option<i32> {
        let raw: RawHandle = handle.into_raw_handle();
        // SAFETY: ownership moves to the CRT on success. On failure the CRT
        // does not take it, so it is re-owned and closed exactly once.
        let fd = unsafe { libc::open_osfhandle(raw as libc::intptr_t, libc::O_NOINHERIT) };
        if fd < 0 {
            drop(unsafe { OwnedHandle::from_raw_handle(raw) });
            return None;
        }
        Some(fd)
    }

    fn wide(text: &str) -> Option<Vec<u16>> {
        let units: Vec<u16> = text.encode_utf16().collect();
        (units.len() <= 32_767).then_some(units)
    }

    /// One `NtCreateFile` relative to `root`; `"."` (when allowed) reopens it.
    fn nt_open(
        root: HANDLE,
        name: &[u8],
        allow_dot: bool,
        access: u32,
        disposition: u32,
        options: u32,
    ) -> Option<OwnedHandle> {
        if !windows_component_valid(name, allow_dot) {
            return None;
        }
        let units = if name == b"." {
            Vec::new()
        } else {
            wide(std::str::from_utf8(name).ok()?)?
        };
        let byte_len = u16::try_from(units.len().checked_mul(2)?).ok()?;
        let object_name = UNICODE_STRING {
            Length: byte_len,
            MaximumLength: byte_len,
            Buffer: if units.is_empty() {
                std::ptr::null_mut()
            } else {
                units.as_ptr().cast_mut()
            },
        };
        let mut attributes = OBJECT_ATTRIBUTES {
            Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>()).ok()?,
            RootDirectory: root,
            ObjectName: &object_name,
            Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let options = options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
        let call = |attributes: &OBJECT_ATTRIBUTES| {
            let mut handle: HANDLE = std::ptr::null_mut();
            let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
            // SAFETY: every pointer refers to a live local for the call; the
            // name buffer is not NUL-terminated because Length bounds it.
            let status = unsafe {
                NtCreateFile(
                    &mut handle,
                    access | SYNCHRONIZE,
                    attributes,
                    &mut status_block,
                    std::ptr::null(),
                    FILE_ATTRIBUTE_NORMAL,
                    SHARE_ALL,
                    disposition,
                    options,
                    std::ptr::null(),
                    0,
                )
            };
            (status, handle)
        };
        let (mut status, mut handle) = call(&attributes);
        if status == STATUS_INVALID_PARAMETER {
            // OBJ_DONT_REPARSE predates some supported kernels. With a single
            // component, FILE_OPEN_REPARSE_POINT plus the attribute check is
            // the actual guarantee, so dropping the redundant flag is safe.
            attributes.Attributes = OBJ_CASE_INSENSITIVE;
            (status, handle) = call(&attributes);
        }
        if status < 0 || handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: a successful NtCreateFile returned a new owned handle.
        Some(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    pub(in crate::fs_runtime) fn dir_open(path: &str) -> Option<i32> {
        if path.is_empty() || path.len() >= 4096 || path.contains('\0') {
            return None;
        }
        let mut units = wide(path)?;
        units.push(0);
        // SAFETY: the path is NUL-terminated and lives across the call. This
        // is the explicit ambient-authority entry point, like POSIX `open`.
        let handle = unsafe {
            CreateFileW(
                units.as_ptr(),
                DIR_ACCESS,
                SHARE_ALL,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: CreateFileW succeeded and returned a new owned handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        adopt(checked(handle, Kind::Dir, false)?)
    }

    pub(in crate::fs_runtime) fn dir_openat(fd: i32, name: &[u8]) -> Option<i32> {
        let root = root_handle(fd)?;
        let handle = nt_open(root, name, true, DIR_ACCESS, FILE_OPEN, FILE_DIRECTORY_FILE)?;
        adopt(checked(handle, Kind::Dir, false)?)
    }

    pub(in crate::fs_runtime) fn openat(
        fd: i32,
        name: &[u8],
        read: bool,
        write: bool,
        create: bool,
        truncate: bool,
    ) -> Option<i32> {
        let root = root_handle(fd)?;
        let mut access = FILE_READ_ATTRIBUTES;
        if read {
            access |= FILE_GENERIC_READ;
        }
        if write {
            access |= FILE_GENERIC_WRITE;
        }
        let disposition = if create { FILE_OPEN_IF } else { FILE_OPEN };
        let handle = nt_open(
            root,
            name,
            false,
            access,
            disposition,
            FILE_NON_DIRECTORY_FILE,
        )?;
        let handle = checked(handle, Kind::File, false)?;
        // Truncation runs only after the object passed the checks, so a link
        // or device is never overwritten (no FILE_OVERWRITE_IF disposition).
        // SAFETY: the handle is live, freshly opened at offset 0 with write.
        if truncate && unsafe { SetEndOfFile(handle.as_raw_handle() as HANDLE) } == 0 {
            return None;
        }
        adopt(handle)
    }

    pub(in crate::fs_runtime) fn mkdirat(fd: i32, name: &[u8]) -> Option<i32> {
        let root = root_handle(fd)?;
        let handle = nt_open(
            root,
            name,
            false,
            DIR_ACCESS,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?;
        adopt(checked(handle, Kind::Dir, false)?)
    }

    pub(in crate::fs_runtime) fn unlinkat(fd: i32, name: &[u8], is_dir: bool) -> Option<()> {
        let root = root_handle(fd)?;
        let (kind, options) = if is_dir {
            (Kind::Dir, FILE_DIRECTORY_FILE)
        } else {
            (Kind::File, FILE_NON_DIRECTORY_FILE)
        };
        let handle = nt_open(
            root,
            name,
            false,
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            options,
        )?;
        // A file link is removed as the link itself (POSIX unlink). A
        // directory junction/symlink fails like rmdir on a symlink (ENOTDIR).
        let handle = checked(handle, kind, !is_dir)?;
        let raw = handle.as_raw_handle() as HANDLE;
        let set = |info: *const std::ffi::c_void, len: usize, class| {
            let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
            let len = u32::try_from(len).unwrap_or(0);
            // SAFETY: `info` points at a live record of `len` bytes.
            unsafe { NtSetInformationFile(raw, &mut status_block, info, len, class) }
        };
        let posix = FILE_DISPOSITION_INFORMATION_EX {
            Flags: FILE_DISPOSITION_DELETE | FILE_DISPOSITION_POSIX_SEMANTICS,
        };
        let mut status = set(
            (&raw const posix).cast(),
            std::mem::size_of_val(&posix),
            FileDispositionInformationEx,
        );
        if matches!(
            status,
            STATUS_INVALID_INFO_CLASS | STATUS_INVALID_PARAMETER | STATUS_NOT_SUPPORTED
        ) {
            // Pre-1607 kernels or non-NTFS volumes: delete-on-close keeps the
            // same confinement, only the visibility timing differs.
            let legacy = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
            status = set(
                (&raw const legacy).cast(),
                std::mem::size_of_val(&legacy),
                FileDispositionInformation,
            );
        }
        drop(handle);
        (status >= 0).then_some(())
    }
}

#[cfg(windows)]
pub(super) use nt::{dir_open, dir_openat, mkdirat, openat, unlinkat};

#[cfg(test)]
mod tests {
    use super::windows_component_valid;

    #[test]
    fn windows_components_reject_escapes_streams_devices_and_aliases() {
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "C:",
            "C:x",
            "file:stream",
            "file::$DATA",
            "\\\\?\\C:\\x",
            "\\??\\C:\\x",
            "\\\\server\\share",
            "/abs",
            "trail.",
            "trail ",
            "CON",
            "con",
            "Con.txt",
            "NUL",
            "nul.tar.gz",
            "AUX",
            "PRN",
            "COM1",
            "com9",
            "LPT0",
            "COM1 .txt",
            "COM\u{b9}",
            "LPT\u{b3}.log",
            "CONIN$",
            "conout$",
            "CLOCK$",
            "wild*",
            "q?",
            "a\"b",
            "<",
            ">",
            "pipe|",
            "nul\0x",
            "tab\tname",
        ] {
            assert!(!windows_component_valid(name.as_bytes(), false), "{name:?}");
        }
        assert!(
            !windows_component_valid(b"\xff\xfe", false),
            "invalid UTF-8"
        );
        for name in [
            "inside",
            "a.b",
            ".hidden",
            "COM10",
            "CONSOLE",
            "NULL",
            "LPT",
            "COM",
            "con_x",
            "x.CON",
            "~1",
            "PROGRA~1",
            "ação",
            "ünïcödé.txt",
        ] {
            assert!(windows_component_valid(name.as_bytes(), false), "{name:?}");
        }
        assert!(windows_component_valid(b".", true));
        assert!(!windows_component_valid(b"..", true));
    }

    /// Native NT contract: needs a Windows runner (CI `windows-latest`).
    #[test]
    #[cfg(windows)]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn nt_capability_confines_junctions_streams_devices_and_survives_rename() {
        use crate::fs_runtime::{
            ar_fs_close, ar_fs_dir_open, ar_fs_dir_openat, ar_fs_mkdirat, ar_fs_openat, ar_fs_read,
            ar_fs_unlinkat, ar_fs_write,
        };
        let base = std::env::temp_dir().join(format!("arandu_nt_cap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("inside"), "inside").unwrap();
        std::fs::write(outside.join("victim"), "outside").unwrap();
        // Junctions need no privilege, unlike symlinks, so they always run.
        let junction = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("junction"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(junction.status.success(), "mklink /J failed");
        let file_link =
            std::os::windows::fs::symlink_file(outside.join("victim"), root.join("link")).is_ok();
        let root_text = root.to_str().unwrap();
        // SAFETY: every pointer/length pair below is a live local buffer and
        // every successful token is closed exactly once by this test.
        unsafe {
            let dir = ar_fs_dir_open(root_text.as_ptr(), root_text.len());
            assert!(dir >= 0);
            for path in [
                "..\\outside\\victim",
                "../outside/victim",
                outside.join("victim").to_str().unwrap(),
                "junction\\victim",
                "junction/victim",
                "junction",
                "inside:ads",
                "inside::$DATA",
                "NUL",
                "con.txt",
                "COM1",
                "inside.",
                "inside ",
                "\\\\?\\C:\\Windows\\win.ini",
                "C:inside",
            ] {
                assert_eq!(
                    ar_fs_openat(dir, path.as_ptr(), path.len(), true, true, true, true),
                    -1,
                    "{path:?}"
                );
            }
            assert_eq!(ar_fs_dir_openat(dir, b"junction".as_ptr(), 8), -1);
            assert_eq!(ar_fs_unlinkat(dir, b"junction".as_ptr(), 8, true), -1);
            if file_link {
                assert_eq!(
                    ar_fs_openat(dir, b"link".as_ptr(), 4, false, true, true, true),
                    -1
                );
                assert_eq!(ar_fs_unlinkat(dir, b"link".as_ptr(), 4, false), 0);
                assert!(outside.join("victim").exists(), "only the link is removed");
            }
            assert_eq!(
                std::fs::read_to_string(outside.join("victim")).unwrap(),
                "outside"
            );
            assert!(!root.join("inside:ads").exists());
            assert_eq!(ar_fs_mkdirat(dir, b"NUL".as_ptr(), 3), -1);
            assert_eq!(
                ar_fs_dir_openat(dir, b"inside".as_ptr(), 6),
                -1,
                "file is not a Dir"
            );

            let child = ar_fs_mkdirat(dir, b"made".as_ptr(), 4);
            assert!(child >= 0);
            let file = ar_fs_openat(child, b"file".as_ptr(), 4, false, true, true, true);
            assert!(file >= 0);
            assert_eq!(ar_fs_write(file, b"hello".as_ptr(), 5), 5);
            ar_fs_close(file);
            let file = ar_fs_openat(child, b"FILE".as_ptr(), 4, true, false, false, false);
            assert!(file >= 0, "Win32 lookups stay case-insensitive");
            let mut buf = [0u8; 8];
            assert_eq!(ar_fs_read(file, buf.as_mut_ptr(), buf.len()), 5);
            assert_eq!(&buf[..5], b"hello");
            ar_fs_close(file);
            assert_eq!(
                ar_fs_unlinkat(dir, b"made".as_ptr(), 4, true),
                -1,
                "not empty"
            );
            assert_eq!(ar_fs_unlinkat(child, b"file".as_ptr(), 4, false), 0);
            ar_fs_close(child);
            assert_eq!(ar_fs_unlinkat(dir, b"made".as_ptr(), 4, true), 0);

            let same = ar_fs_dir_openat(dir, b".".as_ptr(), 1);
            assert!(same >= 0);
            ar_fs_close(same);
            std::fs::rename(&root, base.join("renamed")).unwrap();
            let file = ar_fs_openat(dir, b"inside".as_ptr(), 6, true, false, false, false);
            assert!(file >= 0, "renaming retains authority of the pinned handle");
            ar_fs_close(file);
            ar_fs_close(dir);
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}
