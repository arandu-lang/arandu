use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli_error::CliFailure;

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
    if path.is_file() {
        if fs::read(path).is_ok_and(|existing| existing == bytes) {
            return Ok(());
        }
        return atomic_replace(path, bytes);
    }
    write_staging(path, bytes).and_then(|staging| {
        fs::rename(&staging, path).map_err(|error| {
            let _ = fs::remove_file(&staging);
            failure("publish artifact", path, error)
        })
    })
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
    let staging = write_staging(path, bytes)?;
    atomic_platform_replace(path, &staging)
}

#[cfg(not(windows))]
fn atomic_platform_replace(path: &Path, staging: &Path) -> Result<(), CliFailure> {
    fs::rename(staging, path).map_err(|error| {
        let _ = fs::remove_file(staging);
        failure("publish build state", path, error)
    })
}

#[cfg(windows)]
fn atomic_platform_replace(path: &Path, staging: &Path) -> Result<(), CliFailure> {
    let candidate = fs::read(staging).map_err(|error| {
        let _ = fs::remove_file(staging);
        failure("read staged build state", staging, error)
    })?;
    for attempt in 0..8 {
        // Staging is a sibling on the same filesystem. std's Windows rename
        // uses MoveFileExW(REPLACE_EXISTING), avoiding ReplaceFileW's partial
        // backup/delete states when competing publishers race.
        let error = match fs::rename(staging, path) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if fs::read(path).is_ok_and(|published| published == candidate) {
            let _ = fs::remove_file(staging);
            return Ok(());
        }
        // Sharing/locking violations can be transient. Other failures remain
        // operational errors; never delete the destination to force publication.
        if !matches!(error.raw_os_error(), Some(32 | 33)) || attempt == 7 {
            let _ = fs::remove_file(staging);
            return Err(failure("publish build state", path, error));
        }
        std::thread::sleep(std::time::Duration::from_millis(5 << attempt.min(3)));
    }
    Err(failure(
        "publish build state",
        path,
        std::io::Error::other("replacement retry limit exhausted"),
    ))
}

pub(super) fn publish_staging(staging: &Path, destination: &Path) -> Result<(), CliFailure> {
    if destination.is_file() {
        let existing = fs::read(destination)
            .map_err(|error| failure("verify existing linked artifact", destination, error))?;
        let candidate = fs::read(staging)
            .map_err(|error| failure("verify staged linked artifact", staging, error))?;
        if existing == candidate {
            fs::remove_file(staging)
                .map_err(|error| failure("discard duplicate artifact", staging, error))?;
            return Ok(());
        }
        return atomic_platform_replace(destination, staging);
    }
    fs::rename(staging, destination)
        .map_err(|error| failure("publish linked artifact", destination, error))
}

fn write_staging(path: &Path, bytes: &[u8]) -> Result<PathBuf, CliFailure> {
    let staging = unique_staging_path(path, "write");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| failure("create artifact staging file", &staging, error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            let _ = fs::remove_file(&staging);
            failure("flush artifact staging file", &staging, error)
        })?;
    Ok(staging)
}

fn unique_staging_path(path: &Path, operation: &str) -> PathBuf {
    static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!(
        "{operation}-tmp-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}

pub(super) fn failure(operation: &'static str, path: &Path, error: std::io::Error) -> CliFailure {
    CliFailure::operational(operation, Some(path.to_path_buf()), error.to_string())
}

pub(super) fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}
