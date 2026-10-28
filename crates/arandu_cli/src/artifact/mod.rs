//! Project-local artifact lifecycle. Filesystem effects stay outside Salsa.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli_error::CliFailure;

mod atomic;
mod publish;

pub(crate) use atomic::atomic_replace;
use atomic::{failure, publish_staging, safe_relative_path};
pub use publish::{
    publish_benchmark_harness, publish_native_artifact, publish_partitioned_native_artifact,
    publish_test_harness, publish_wasm_artifact,
};

const TARGET_MARKER: &str = ".arandu-target-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeProfile {
    Dev,
    Release,
    Size,
}

impl NativeProfile {
    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Release => "release",
            Self::Size => "size",
        }
    }

    fn backend(self) -> &'static str {
        match self {
            Self::Dev => "cranelift-aot",
            Self::Release => "cranelift-aot-speed",
            Self::Size => "cranelift-aot-size",
        }
    }
}

#[derive(Debug)]
pub struct ArtifactLayout {
    pub target_root: PathBuf,
    pub profile_root: PathBuf,
    pub bin: PathBuf,
    pub deps: PathBuf,
    pub incremental: PathBuf,
    pub triple: String,
}

/// Content-addressed executable published by the artifact transaction.
#[derive(Debug)]
pub struct PublishedNativeArtifact {
    pub path: PathBuf,
    pub digest: String,
}

#[derive(Serialize)]
struct BuildState<'a> {
    schema: u32,
    package: &'a str,
    version: &'a str,
    profile: &'a str,
    target: &'a str,
    backend: &'a str,
    artifact_digest: &'a str,
    compiler_version: &'a str,
    artifact: &'a str,
    object: &'a str,
    linker: &'a str,
}

pub fn host_triple() -> String {
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        other => other,
    };
    match (arch, std::env::consts::OS) {
        (arch, "windows") => format!("{arch}-pc-windows-msvc"),
        (arch, "macos") => format!("{arch}-apple-darwin"),
        (arch, "linux") => format!("{arch}-unknown-linux-gnu"),
        (arch, os) => format!("{arch}-unknown-{os}"),
    }
}

pub fn layout(project_root: &Path, profile: &str) -> ArtifactLayout {
    let triple = host_triple();
    let target_root = project_root.join("target");
    let profile_root = target_root.join(profile).join(&triple);
    let bin = profile_root.join("bin");
    let deps = profile_root.join("deps");
    let incremental = profile_root.join("incremental");
    ArtifactLayout {
        target_root,
        profile_root,
        bin,
        deps,
        incremental,
        triple,
    }
}

pub fn layout_for_target(project_root: &Path, profile: &str, triple: &str) -> ArtifactLayout {
    let target_root = project_root.join("target");
    let profile_root = target_root.join(profile).join(triple);
    let bin = profile_root.join("bin");
    let deps = profile_root.join("deps");
    let incremental = profile_root.join("incremental");
    ArtifactLayout {
        target_root,
        profile_root,
        bin,
        deps,
        incremental,
        triple: triple.to_string(),
    }
}

/// What a process that published an artifact knows of it: its path, digest and
/// file identity right after publication. While the identity holds, that
/// process may use the bytes without hashing them. It is never persisted: after
/// a crash the metadata can survive data that never reached the disk, so a new
/// process always verifies the digest (HOT.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactIdentity {
    path: PathBuf,
    digest: String,
    stat: FileStat,
}

/// Device, inode, size and nanosecond modification and change times. Any
/// write, rename or truncation of the file changes at least one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStat {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStat {
    #[cfg(unix)]
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::symlink_metadata(path).ok()?;
        metadata.is_file().then(|| Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }

    /// Without stable inode numbers there is no identity to trust.
    #[cfg(not(unix))]
    fn of(_path: &Path) -> Option<Self> {
        None
    }
}

impl ArtifactIdentity {
    /// The identity of `artifact` as it is now, just after its publication.
    #[must_use]
    pub fn of(artifact: &PublishedNativeArtifact) -> Option<Self> {
        Some(Self {
            path: artifact.path.clone(),
            digest: artifact.digest.clone(),
            stat: FileStat::of(&artifact.path)?,
        })
    }

    /// Whether `artifact` is still the file this identity was taken from.
    #[must_use]
    pub fn verifies(&self, artifact: &PublishedNativeArtifact) -> bool {
        self.path == artifact.path
            && self.digest == artifact.digest
            && FileStat::of(&artifact.path) == Some(self.stat)
    }
}

/// Returns a verified current native artifact if one exists in build-state.json.
pub fn current_native_artifact(
    project_root: &Path,
    profile: NativeProfile,
) -> Option<PublishedNativeArtifact> {
    let artifact = current_native_artifact_candidate(project_root, profile)?;
    let bytes = fs::read(&artifact.path).ok()?;
    (!bytes.is_empty() && blake3::hash(&bytes).to_hex().as_str() == artifact.digest)
        .then_some(artifact)
}

/// Resolves a safely confined artifact candidate without trusting its bytes.
///
/// Callers must verify the digest before reuse. The ELF patcher may instead
/// copy this candidate to staging and validate that copy against its
/// independent layout digest before performing any write.
pub fn current_native_artifact_candidate(
    project_root: &Path,
    profile: NativeProfile,
) -> Option<PublishedNativeArtifact> {
    let layout = layout(project_root, profile.directory());
    let state_path = layout.profile_root.join("build-state.json");
    let content = fs::read_to_string(state_path).ok()?;
    #[derive(Deserialize)]
    struct CurrentBuildState {
        schema: u32,
        artifact_digest: String,
        artifact: String,
    }
    let state: CurrentBuildState = serde_json::from_str(&content).ok()?;
    let relative = Path::new(&state.artifact);
    if state.schema != 2 || !safe_relative_path(relative) {
        return None;
    }
    let path = layout.profile_root.join(relative);
    path.is_file().then_some(PublishedNativeArtifact {
        path,
        digest: state.artifact_digest,
    })
}

/// Records the provenance of an in-place patched native executable.
pub fn record_patched_native_artifact(
    project_root: &Path,
    package: &str,
    version: &str,
    profile: NativeProfile,
    cgu_objects: &[PathBuf],
    patched_executable_path: &Path,
    new_digest: &str,
) -> Result<PublishedNativeArtifact, CliFailure> {
    let layout = layout(project_root, profile.directory());
    fs::create_dir_all(&layout.bin)
        .map_err(|error| failure("create artifact layout", &layout.bin, error))?;
    let lock_path = layout.profile_root.join(".publish.lock");
    let publish_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| failure("open build publication lock", &lock_path, error))?;
    publish_lock
        .lock()
        .map_err(|error| failure("lock build publication", &lock_path, error))?;
    let digest_prefix = new_digest.get(..16).ok_or_else(|| {
        CliFailure::operational(
            "record patched native artifact",
            Some(patched_executable_path.to_path_buf()),
            "BLAKE3 digest is shorter than 16 hexadecimal characters",
        )
    })?;
    let executable_name = if cfg!(windows) {
        format!("{package}-{digest_prefix}.exe")
    } else {
        format!("{package}-{digest_prefix}")
    };
    let target_path = layout.bin.join(&executable_name);

    if patched_executable_path != target_path {
        publish_staging(patched_executable_path, &target_path)?;
    }

    let relative = format!("bin/{executable_name}");
    let object_relative = if let Some(first) = cgu_objects.first() {
        if let Ok(rel) = first.strip_prefix(&layout.profile_root) {
            rel.to_string_lossy().to_string()
        } else {
            first.to_string_lossy().to_string()
        }
    } else {
        String::new()
    };
    let state = BuildState {
        schema: 2,
        package,
        version,
        profile: profile.directory(),
        target: &layout.triple,
        backend: profile.backend(),
        artifact_digest: new_digest,
        compiler_version: crate::project::ARANDU_VERSION,
        artifact: &relative,
        object: &object_relative,
        linker: crate::linker::LinkerKind::InProcessElf.label(),
    };
    let mut encoded = serde_json::to_vec_pretty(&state).map_err(|error| {
        CliFailure::operational("serialize build provenance", None, error.to_string())
    })?;
    encoded.push(b'\n');
    atomic_replace(&layout.profile_root.join("build-state.json"), &encoded)?;
    Ok(PublishedNativeArtifact {
        path: target_path,
        digest: new_digest.to_owned(),
    })
}

pub fn clean(project_root: &Path) -> Result<bool, CliFailure> {
    let canonical_root = fs::canonicalize(project_root)
        .map_err(|error| failure("resolve project root", project_root, error))?;
    let target = canonical_root.join("target");
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(failure("inspect target directory", &target, error)),
    };
    let marker_valid = fs::read(target.join(TARGET_MARKER))
        .is_ok_and(|contents| contents == b"arandu-target-v1\n");
    if !metadata.is_dir() || metadata.file_type().is_symlink() || !marker_valid {
        return Err(CliFailure::operational(
            "clean project artifacts",
            Some(target),
            "refusing to remove an unowned, non-directory, symlink or junction-like target",
        ));
    }
    fs::remove_dir_all(&target)
        .map_err(|error| failure("clean project artifacts", &target, error))?;
    Ok(true)
}

#[cfg(test)]
mod atomic_replace_tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn failed_publication_preserves_destination_and_removes_staging() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "arandu-publication-failure-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("isolated directory");
        let destination = directory.join("current.amir");
        fs::create_dir(&destination).expect("conflicting directory");
        fs::write(destination.join("keep"), b"original content").expect("original data");
        assert!(atomic_replace(&destination, b"new content").is_err());
        assert_eq!(
            fs::read(destination.join("keep")).unwrap(),
            b"original content"
        );
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            1,
            "failed staging must be removed"
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn concurrent_identical_replacements_are_idempotent() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "arandu-atomic-replace-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("create isolated test directory");
        let destination = directory.join("current.amir");
        fs::write(&destination, b"old state").expect("write initial state");

        let writers = 8;
        let barrier = Arc::new(Barrier::new(writers));
        let handles: Vec<_> = (0..writers)
            .map(|_| {
                let path = destination.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    atomic_replace(&path, b"complete new state")
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .expect("replacement worker should not panic")
                .expect("identical concurrent replacements should succeed");
        }
        assert_eq!(fs::read(&destination).unwrap(), b"complete new state");
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            1,
            "all staging files must be consumed"
        );
        fs::remove_dir_all(directory).expect("remove isolated test directory");
    }
}
