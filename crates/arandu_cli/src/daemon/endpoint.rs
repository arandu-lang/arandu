//! Where a project's daemon listens and who may talk to it (HOT.3).
//!
//! The socket lives in a directory only the current user can enter:
//! `$XDG_RUNTIME_DIR/arandu`, or `/tmp/arandu-$UID` when there is no runtime
//! directory. A directory that is a symlink, belongs to another user or is
//! open to others is refused rather than repaired, and every connection is
//! checked against the peer's credentials.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// `sun_path` holds 108 bytes on Linux and 104 on macOS, the terminator
/// included.
const MAX_SOCKET_PATH: usize = 103;

fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// A private directory for this user's daemon sockets.
pub fn socket_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && private_dir(path).is_ok());
    let dir = match base {
        Some(runtime) => runtime.join("arandu"),
        None => std::env::temp_dir().join(format!("arandu-{}", effective_uid())),
    };
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    private_dir(&dir)?;
    Ok(dir)
}

/// The directory exists, is not a symlink, belongs to this user and no one
/// else may enter it.
fn private_dir(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let refuse = |reason: &str| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: {reason}", path.display()),
        ))
    };
    if !metadata.is_dir() {
        return refuse("not a directory");
    }
    if metadata.uid() != effective_uid() {
        return refuse("owned by another user");
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return refuse("accessible to other users");
    }
    Ok(())
}

/// The socket of the daemon serving `root`. One per project, whatever the
/// toolchain: a client of another toolchain reaches the running daemon and
/// the handshake replaces it.
pub fn socket_path(root: &Path) -> io::Result<PathBuf> {
    let mut key = blake3::Hasher::new();
    key.update(b"arandu-daemon-endpoint/v1\0");
    key.update(root.as_os_str().as_encoded_bytes());
    let name = format!("{}.sock", &key.finalize().to_hex()[..32]);
    let path = socket_dir()?.join(name);
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon socket path exceeds sun_path",
        ));
    }
    Ok(path)
}

/// The lock a live daemon of `root` holds for as long as it serves.
pub fn lock_path(root: &Path) -> PathBuf {
    root.join(".arandu").join("daemon.lock")
}

/// Whether the process at the other end runs as this user.
pub fn peer_is_owner(stream: &UnixStream) -> bool {
    peer_uid(stream).is_some_and(|uid| uid == effective_uid())
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;

    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` and `length` describe a writable `ucred` buffer of
    // the size the call expects, and the descriptor is a live socket.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    (result == 0 && length as usize == std::mem::size_of::<libc::ucred>())
        .then_some(credentials.uid)
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;

    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: both out-parameters are valid for writes and the descriptor is
    // a live connected socket.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    (result == 0).then_some(uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_directory_is_private_and_a_public_one_is_refused() {
        let dir = socket_dir().unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{}", dir.display());

        let public = std::env::temp_dir().join(format!("arandu-public-{}", std::process::id()));
        fs::create_dir_all(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&public).is_err());
        let link = public.with_extension("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(private_dir(&link).is_err(), "a symlink is never trusted");
        let _ = fs::remove_file(&link);
        let _ = fs::remove_dir(&public);
    }

    #[test]
    fn a_connected_peer_of_this_process_is_its_owner() {
        let (left, _right) = UnixStream::pair().unwrap();
        assert!(peer_is_owner(&left));
    }

    #[test]
    fn socket_names_depend_on_the_project_root() {
        let a = socket_path(Path::new("/p")).unwrap();
        assert_ne!(a, socket_path(Path::new("/q")).unwrap());
        assert_eq!(a, socket_path(Path::new("/p")).unwrap());
    }
}
