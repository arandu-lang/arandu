use crate::rt_runtime::path_from_fat;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
use std::path::Path;

// Capability paths are deliberately strict: no absolute paths, empty
// components, '.' or '..'. Dot is used only for an already-pinned parent.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn capability_path(path: &Path, allow_dot: bool) -> Option<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() >= 4096 {
        return None;
    }
    if !(allow_dot && bytes == b".")
        && bytes
            .split(|byte| *byte == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return None;
    }
    std::ffi::CString::new(bytes).ok()
}

#[cfg(target_os = "linux")]
fn capability_open(
    dir_fd: i32,
    path: &Path,
    flags: i32,
    mode: u32,
    allow_dot: bool,
) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    if dir_fd < 0 {
        return None;
    }
    let path = capability_path(path, allow_dot)?;
    // SAFETY: open_how is a UAPI integer record. Zero also initializes any
    // reserved fields so the kernel can safely reject unsupported versions.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::try_from(flags | libc::O_CLOEXEC).ok()?;
    how.mode = u64::from(mode);
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
    // SAFETY: CString and open_how remain live throughout the syscall. The
    // kernel resolves the entire path beneath the descriptor atomically.
    // ENOSYS/EINVAL fail closed; there is no weaker openat fallback.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir_fd,
            path.as_ptr(),
            &how,
            std::mem::size_of_val(&how),
        )
    };
    let fd = i32::try_from(raw).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: a successful openat2 transferred a new descriptor to this owner.
    Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

// A single component has no intermediate directory that can be renamed
// between validation and openat. Multi-component traversal deliberately fails
// closed on platforms without openat2; callers derive each child Dir explicitly.
#[cfg(any(
    all(test, target_os = "linux"),
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(super) fn capability_open_component(
    dir_fd: i32,
    path: &Path,
    flags: i32,
    mode: u32,
    allow_dot: bool,
) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    if dir_fd < 0 {
        return None;
    }
    let path = capability_path(path, allow_dot)?;
    if path.as_bytes().contains(&b'/') {
        return None;
    }
    // Permission bits fit the promoted C int argument on every supported ABI;
    // passing a 16-bit mode_t directly to a variadic call is invalid on Darwin.
    let mode = libc::c_int::try_from(mode).ok()?;
    // SAFETY: the name is one NUL-terminated component (or an allowed dot),
    // and O_NOFOLLOW rejects a final symlink in the same kernel operation.
    let raw = unsafe {
        libc::openat(
            dir_fd,
            path.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if raw < 0 {
        return None;
    }
    // SAFETY: successful openat transferred one new descriptor to this owner.
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    if flags & libc::O_DIRECTORY != 0 {
        // SAFETY: stat is an output record, and fd remains owned and live.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        {
            return None;
        }
    }
    Some(fd)
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn capability_open(
    dir_fd: i32,
    path: &Path,
    flags: i32,
    mode: u32,
    allow_dot: bool,
) -> Option<std::os::fd::OwnedFd> {
    capability_open_component(dir_fd, path, flags, mode, allow_dot)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn capability_parent(
    dir_fd: i32,
    path: &Path,
) -> Option<(std::os::fd::OwnedFd, std::ffi::CString)> {
    use std::os::unix::ffi::OsStrExt;
    let checked = capability_path(path, false)?;
    #[cfg(not(target_os = "linux"))]
    if checked.as_bytes().contains(&b'/') {
        return None;
    }
    #[cfg(target_os = "linux")]
    let _ = checked;
    let leaf = std::ffi::CString::new(path.file_name()?.as_bytes()).ok()?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let descriptor = capability_open(dir_fd, parent, libc::O_RDONLY | libc::O_DIRECTORY, 0, true)?;
    Some((descriptor, leaf))
}

/// Opens an ambient directory capability on Linux, macOS, BSD and Windows.
///
/// # Safety
/// The pointer/length pair must be readable for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_dir_open(path_ptr: *const u8, path_len: usize) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = path_from_fat(path_ptr, isize::try_from(path_len).ok()?)?;
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
            // SAFETY: the path is NUL-terminated and the returned descriptor is
            // transferred to the caller. This is explicit ambient authority.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            (fd >= 0).then_some(fd)
        }
        #[cfg(windows)]
        {
            super::capability_windows::dir_open(path.to_str()?)
        }
        #[cfg(not(any(
            windows,
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            let _ = path;
            None
        }
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Opens a file without following symlinks. Linux resolves paths beneath the
/// descriptor atomically; macOS/BSD accept a single relative component only.
///
/// # Safety
/// The pointer/length pair must be readable; the caller owns dir_fd's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_openat(
    dir_fd: i32,
    path_ptr: *const u8,
    path_len: usize,
    read: bool,
    write: bool,
    create: bool,
    truncate: bool,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if (!read && !write) || ((create || truncate) && !write) {
            return None;
        }
        let path = path_from_fat(path_ptr, isize::try_from(path_len).ok()?)?;
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            use std::os::fd::IntoRawFd;
            let mut flags = if read && write {
                libc::O_RDWR
            } else if write {
                libc::O_WRONLY
            } else if read {
                libc::O_RDONLY
            } else {
                return None;
            };
            if create {
                flags |= libc::O_CREAT;
            }
            if truncate {
                flags |= libc::O_TRUNC;
            }
            capability_open(dir_fd, &path, flags, if create { 0o666 } else { 0 }, false)
                .map(|fd| fd.into_raw_fd())
        }
        #[cfg(windows)]
        {
            super::capability_windows::openat(
                dir_fd,
                path.to_str()?.as_bytes(),
                read,
                write,
                create,
                truncate,
            )
        }
        #[cfg(not(any(
            windows,
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            let _ = (dir_fd, path, read, write, create, truncate);
            None
        }
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Opens a child directory with the same capability restrictions.
///
/// # Safety
/// The pointer/length pair must be readable; the caller owns dir_fd's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_dir_openat(
    dir_fd: i32,
    path_ptr: *const u8,
    path_len: usize,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = path_from_fat(path_ptr, isize::try_from(path_len).ok()?)?;
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            use std::os::fd::IntoRawFd;
            capability_open(dir_fd, &path, libc::O_RDONLY | libc::O_DIRECTORY, 0, true)
                .map(|fd| fd.into_raw_fd())
        }
        #[cfg(windows)]
        {
            super::capability_windows::dir_openat(dir_fd, path.to_str()?.as_bytes())
        }
        #[cfg(not(any(
            windows,
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            let _ = (dir_fd, path);
            None
        }
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Creates a child in a parent obtained through confined kernel resolution.
///
/// # Safety
/// The pointer/length pair must be readable; the caller owns dir_fd's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_mkdirat(dir_fd: i32, path_ptr: *const u8, path_len: usize) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = path_from_fat(path_ptr, isize::try_from(path_len).ok()?)?;
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            use std::os::fd::{AsRawFd, IntoRawFd};
            let (parent, leaf) = capability_parent(dir_fd, &path)?;
            // SAFETY: the parent is live and leaf has exactly one normal path
            // component, so this mutation cannot traverse outside that handle.
            if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o777) } != 0 {
                return None;
            }
            let leaf_path = Path::new(std::ffi::OsStr::from_bytes(leaf.as_bytes()));
            use std::os::unix::ffi::OsStrExt;
            capability_open(
                parent.as_raw_fd(),
                leaf_path,
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
                false,
            )
            .map(|fd| fd.into_raw_fd())
        }
        #[cfg(windows)]
        {
            super::capability_windows::mkdirat(dir_fd, path.to_str()?.as_bytes())
        }
        #[cfg(not(any(
            windows,
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            let _ = (dir_fd, path);
            None
        }
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Removes one entry in a parent obtained through confined kernel resolution.
///
/// # Safety
/// The pointer/length pair must be readable; the caller owns dir_fd's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_fs_unlinkat(
    dir_fd: i32,
    path_ptr: *const u8,
    path_len: usize,
    is_dir: bool,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = path_from_fat(path_ptr, isize::try_from(path_len).ok()?)?;
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            use std::os::fd::AsRawFd;
            let (parent, leaf) = capability_parent(dir_fd, &path)?;
            // SAFETY: unlinkat does not follow a final symlink; leaf contains
            // no separator and the pinned parent owns its own descriptor.
            let result = unsafe {
                libc::unlinkat(
                    parent.as_raw_fd(),
                    leaf.as_ptr(),
                    if is_dir { libc::AT_REMOVEDIR } else { 0 },
                )
            };
            (result == 0).then_some(0)
        }
        #[cfg(windows)]
        {
            super::capability_windows::unlinkat(dir_fd, path.to_str()?.as_bytes(), is_dir)
                .map(|()| 0)
        }
        #[cfg(not(any(
            windows,
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            let _ = (dir_fd, path, is_dir);
            None
        }
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}
