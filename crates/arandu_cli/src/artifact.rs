//! Project-local artifact lifecycle. Filesystem effects stay outside Salsa.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cli_error::CliFailure;

const TARGET_MARKER: &str = ".arandu-target-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeProfile {
    Dev,
    Release,
}

impl NativeProfile {
    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Release => "release",
        }
    }

    fn backend(self) -> &'static str {
        match self {
            Self::Dev => "cranelift-aot",
            Self::Release => "cranelift-aot-speed",
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

pub fn publish_wasm_artifact(
    project_root: &Path,
    package: &str,
    version: &str,
    profile: NativeProfile,
    triple: &str,
    wasm_bytes: &[u8],
) -> Result<PublishedNativeArtifact, CliFailure> {
    let layout = layout_for_target(project_root, profile.directory(), triple);
    for directory in [&layout.bin, &layout.deps, &layout.incremental] {
        fs::create_dir_all(directory)
            .map_err(|error| failure("create artifact layout", directory, error))?;
    }
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
    atomic_write(
        &layout.target_root.join(TARGET_MARKER),
        b"arandu-target-v1\n",
    )?;

    let digest = blake3::hash(wasm_bytes).to_hex().to_string();
    let dep_artifact_name = format!("{package}-{}.wasm", &digest[..16]);
    let dep_artifact_path = layout.deps.join(&dep_artifact_name);
    atomic_write(&dep_artifact_path, wasm_bytes)?;

    let wasm_file_name = format!("{package}.wasm");
    let published_path = layout.bin.join(&wasm_file_name);
    atomic_write(&published_path, wasm_bytes)?;

    let profile_bin = layout
        .target_root
        .join(profile.directory())
        .join(&wasm_file_name);
    let _ = atomic_write(&profile_bin, wasm_bytes);

    let relative = format!("bin/{wasm_file_name}");
    let dep_relative = format!("deps/{dep_artifact_name}");
    let state = BuildState {
        schema: 2,
        package,
        version,
        profile: profile.directory(),
        target: triple,
        backend: "wasm",
        artifact_digest: &digest,
        compiler_version: crate::project::ARANDU_VERSION,
        artifact: &relative,
        object: &dep_relative,
        linker: "wasm-encoder",
    };
    let mut encoded = serde_json::to_vec_pretty(&state).map_err(|error| {
        CliFailure::operational("serialize build provenance", None, error.to_string())
    })?;
    encoded.push(b'\n');
    atomic_replace(&layout.profile_root.join("build-state.json"), &encoded)?;
    Ok(PublishedNativeArtifact {
        path: published_path,
        digest,
    })
}

#[allow(dead_code)]
pub fn current_wasm_artifact(
    project_root: &Path,
    profile: NativeProfile,
    triple: &str,
) -> Option<PublishedNativeArtifact> {
    let layout = layout_for_target(project_root, profile.directory(), triple);
    let state_path = layout.profile_root.join("build-state.json");
    let bytes = fs::read(&state_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let relative = value.get("artifact")?.as_str()?;
    let digest = value.get("artifact_digest")?.as_str()?;
    let artifact_path = layout.profile_root.join(relative);
    let wasm_bytes = fs::read(&artifact_path).ok()?;
    if !wasm_bytes.is_empty() && blake3::hash(&wasm_bytes).to_hex().as_str() == digest {
        Some(PublishedNativeArtifact {
            path: artifact_path,
            digest: digest.to_string(),
        })
    } else {
        None
    }
}

pub fn publish_native_artifact(
    project_root: &Path,
    package: &str,
    version: &str,
    profile: NativeProfile,
    object: &[u8],
    link: impl FnOnce(&Path, &Path) -> Result<&'static str, CliFailure>,
) -> Result<PublishedNativeArtifact, CliFailure> {
    let layout = layout(project_root, profile.directory());
    for directory in [&layout.bin, &layout.deps, &layout.incremental] {
        fs::create_dir_all(directory)
            .map_err(|error| failure("create artifact layout", directory, error))?;
    }
    // Builds for one profile may run in independent processes. Keep the
    // content-addressed publication and its mutable state pointer in one
    // transaction. File locks are released by the OS if a process crashes.
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
    atomic_write(
        &layout.target_root.join(TARGET_MARKER),
        b"arandu-target-v1\n",
    )?;

    let digest = blake3::hash(object).to_hex().to_string();
    let extension = if cfg!(windows) { "obj" } else { "o" };
    let artifact_name = format!("{package}-{}.{extension}", &digest[..16]);
    let artifact_path = layout.deps.join(&artifact_name);
    atomic_write(&artifact_path, object)?;

    let staging_dir = layout.bin.join(".staging");
    fs::create_dir_all(&staging_dir)
        .map_err(|error| failure("create artifact staging layout", &staging_dir, error))?;
    let staging = staging_dir.join(if cfg!(windows) {
        format!("{package}.exe")
    } else {
        package.to_string()
    });
    let _ = fs::remove_file(&staging);

    let linker = match link(&artifact_path, &staging) {
        Ok(linker) => linker,
        Err(error) => {
            let _ = fs::remove_file(&staging);
            let _ = fs::remove_dir(&staging_dir);
            return Err(error);
        }
    };
    let executable =
        fs::read(&staging).map_err(|error| failure("read linked artifact", &staging, error))?;
    if executable.is_empty() {
        let _ = fs::remove_file(&staging);
        let _ = fs::remove_dir(&staging_dir);
        return Err(CliFailure::operational(
            "publish linked artifact",
            Some(staging),
            "linker produced an empty file",
        ));
    }
    let executable_digest = blake3::hash(&executable).to_hex().to_string();
    let executable_name = if cfg!(windows) {
        format!("{package}-{}.exe", &executable_digest[..16])
    } else {
        format!("{package}-{}", &executable_digest[..16])
    };
    let executable_path = layout.bin.join(&executable_name);
    publish_staging(&staging, &executable_path)?;
    let _ = fs::remove_dir(&staging_dir);

    let relative = format!("bin/{executable_name}");
    let object_relative = format!("deps/{artifact_name}");
    let state = BuildState {
        schema: 2,
        package,
        version,
        profile: profile.directory(),
        target: &layout.triple,
        backend: profile.backend(),
        artifact_digest: &executable_digest,
        compiler_version: crate::project::ARANDU_VERSION,
        artifact: &relative,
        object: &object_relative,
        linker,
    };
    let mut encoded = serde_json::to_vec_pretty(&state).map_err(|error| {
        CliFailure::operational("serialize build provenance", None, error.to_string())
    })?;
    encoded.push(b'\n');
    atomic_replace(&layout.profile_root.join("build-state.json"), &encoded)?;
    Ok(PublishedNativeArtifact {
        path: executable_path,
        digest: executable_digest,
    })
}

/// Publishes a native executable linked from partitioned CGU object files.
pub fn publish_partitioned_native_artifact(
    project_root: &Path,
    package: &str,
    version: &str,
    profile: NativeProfile,
    cgu_objects: &[PathBuf],
    link: impl FnOnce(&[&Path], &Path) -> Result<&'static str, CliFailure>,
) -> Result<PublishedNativeArtifact, CliFailure> {
    let layout = layout(project_root, profile.directory());
    for directory in [&layout.bin, &layout.deps, &layout.incremental] {
        fs::create_dir_all(directory)
            .map_err(|error| failure("create artifact layout", directory, error))?;
    }
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
    atomic_write(
        &layout.target_root.join(TARGET_MARKER),
        b"arandu-target-v1\n",
    )?;

    let staging_dir = layout.bin.join(".staging");
    fs::create_dir_all(&staging_dir)
        .map_err(|error| failure("create artifact staging layout", &staging_dir, error))?;
    let staging = staging_dir.join(if cfg!(windows) {
        format!("{package}.exe")
    } else {
        package.to_string()
    });
    let _ = fs::remove_file(&staging);

    let object_refs: Vec<&Path> = cgu_objects.iter().map(|p| p.as_path()).collect();
    let linker = match link(&object_refs, &staging) {
        Ok(linker) => linker,
        Err(error) => {
            let _ = fs::remove_file(&staging);
            let _ = fs::remove_dir(&staging_dir);
            return Err(error);
        }
    };
    let executable =
        fs::read(&staging).map_err(|error| failure("read linked artifact", &staging, error))?;
    if executable.is_empty() {
        let _ = fs::remove_file(&staging);
        let _ = fs::remove_dir(&staging_dir);
        return Err(CliFailure::operational(
            "publish linked artifact",
            Some(staging),
            "linker produced an empty file",
        ));
    }
    let executable_digest = blake3::hash(&executable).to_hex().to_string();
    let executable_name = if cfg!(windows) {
        format!("{package}-{}.exe", &executable_digest[..16])
    } else {
        format!("{package}-{}", &executable_digest[..16])
    };
    let executable_path = layout.bin.join(&executable_name);
    publish_staging(&staging, &executable_path)?;
    let _ = fs::remove_dir(&staging_dir);

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
        artifact_digest: &executable_digest,
        compiler_version: crate::project::ARANDU_VERSION,
        artifact: &relative,
        object: &object_relative,
        linker,
    };
    let mut encoded = serde_json::to_vec_pretty(&state).map_err(|error| {
        CliFailure::operational("serialize build provenance", None, error.to_string())
    })?;
    encoded.push(b'\n');
    atomic_replace(&layout.profile_root.join("build-state.json"), &encoded)?;
    Ok(PublishedNativeArtifact {
        path: executable_path,
        digest: executable_digest,
    })
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

/// Publish the compiler-validated test registry and portable C entrypoint.
/// The files are content-addressed by the registry digest and replaced
/// atomically, so an interrupted test build cannot leave a partial harness.
pub fn publish_test_harness(
    project_root: &Path,
    registry: &arandu_codegen::testing::TestRegistry,
) -> Result<(PathBuf, PathBuf), CliFailure> {
    let layout = layout(project_root, "dev");
    fs::create_dir_all(&layout.profile_root)
        .map_err(|error| failure("create test harness layout", &layout.profile_root, error))?;
    let ids = registry
        .iter()
        .map(|entry| format!("{}\n{}\n", entry.id, entry.function))
        .collect::<String>();
    let digest = blake3::hash(ids.as_bytes()).to_hex().to_string();
    let manifest = layout
        .profile_root
        .join(format!("test-harness-{digest}.json"));
    let c_source = layout.profile_root.join(format!("test-harness-{digest}.c"));
    let manifest_bytes = serde_json::to_vec_pretty(
        &registry
            .iter()
            .map(|entry| (&entry.id, &entry.function))
            .collect::<Vec<_>>(),
    )
    .map_err(|error| CliFailure::operational("serialize test harness", None, error.to_string()))?;
    atomic_write(&manifest, &manifest_bytes)?;
    atomic_write(&c_source, registry.emit_c_entrypoint().as_bytes())?;
    atomic_replace(
        &layout.profile_root.join("test-harness.json"),
        manifest.to_string_lossy().as_bytes(),
    )?;
    Ok((manifest, c_source))
}

/// Publish the compiler-validated benchmark registry atomically. The registry
/// shape is shared with tests, while filenames remain protocol-specific.
pub fn publish_benchmark_harness(
    project_root: &Path,
    registry: &arandu_codegen::testing::BenchmarkRegistry,
) -> Result<(PathBuf, PathBuf), CliFailure> {
    let layout = layout(project_root, "dev");
    fs::create_dir_all(&layout.profile_root).map_err(|error| {
        failure(
            "create benchmark harness layout",
            &layout.profile_root,
            error,
        )
    })?;
    let ids = registry
        .iter()
        .map(|entry| format!("{}\n{}\n", entry.id, entry.function))
        .collect::<String>();
    let digest = blake3::hash(ids.as_bytes()).to_hex().to_string();
    let manifest = layout
        .profile_root
        .join(format!("bench-harness-{digest}.json"));
    let c_source = layout
        .profile_root
        .join(format!("bench-harness-{digest}.c"));
    let manifest_bytes = serde_json::to_vec_pretty(
        &registry
            .iter()
            .map(|entry| (&entry.id, &entry.function))
            .collect::<Vec<_>>(),
    )
    .map_err(|error| {
        CliFailure::operational("serialize benchmark harness", None, error.to_string())
    })?;
    atomic_write(&manifest, &manifest_bytes)?;
    atomic_write(&c_source, registry.emit_c_entrypoint().as_bytes())?;
    atomic_replace(
        &layout.profile_root.join("bench-harness.json"),
        manifest.to_string_lossy().as_bytes(),
    )?;
    Ok((manifest, c_source))
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
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
    if fs::symlink_metadata(path).is_err() {
        return fs::rename(&staging, path).map_err(|error| {
            let _ = fs::remove_file(&staging);
            failure("publish build state", path, error)
        });
    }
    atomic_platform_replace(path, &staging)
}

#[cfg(not(windows))]
fn atomic_platform_replace(path: &Path, staging: &Path) -> Result<(), CliFailure> {
    fs::rename(staging, path).map_err(|error| failure("publish build state", path, error))
}

#[cfg(windows)]
fn atomic_platform_replace(path: &Path, staging: &Path) -> Result<(), CliFailure> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};

    let replaced = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = staging
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are owned, NUL-terminated UTF-16 buffers that remain
    // alive for the duration of the Win32 call; optional pointers are null.
    let result = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(staging);
        return Err(failure("publish build state", path, error));
    }
    Ok(())
}

fn publish_staging(staging: &Path, destination: &Path) -> Result<(), CliFailure> {
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
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    path.with_extension(format!("{operation}-tmp-{}-{nonce}", std::process::id()))
}

fn failure(operation: &'static str, path: &Path, error: std::io::Error) -> CliFailure {
    CliFailure::operational(operation, Some(path.to_path_buf()), error.to_string())
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}
