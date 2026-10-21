use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use super::atomic::{atomic_replace, atomic_write, failure, publish_staging};
use super::{
    BuildState, NativeProfile, PublishedNativeArtifact, TARGET_MARKER, layout, layout_for_target,
};
use crate::cli_error::CliFailure;

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

/// Publish the compiler-validated test registry.
/// The file is content-addressed by the registry digest and replaced
/// atomically, so an interrupted test build cannot leave a partial harness.
pub fn publish_test_harness(
    project_root: &Path,
    registry: &arandu_codegen::testing::TestRegistry,
) -> Result<PathBuf, CliFailure> {
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
    let manifest_bytes = serde_json::to_vec_pretty(
        &registry
            .iter()
            .map(|entry| (&entry.id, &entry.function))
            .collect::<Vec<_>>(),
    )
    .map_err(|error| CliFailure::operational("serialize test harness", None, error.to_string()))?;
    atomic_write(&manifest, &manifest_bytes)?;
    atomic_replace(
        &layout.profile_root.join("test-harness.json"),
        manifest.to_string_lossy().as_bytes(),
    )?;
    Ok(manifest)
}

/// Publish the compiler-validated benchmark registry atomically. The registry
/// shape is shared with tests, while filenames remain protocol-specific.
pub fn publish_benchmark_harness(
    project_root: &Path,
    registry: &arandu_codegen::testing::BenchmarkRegistry,
) -> Result<PathBuf, CliFailure> {
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
    atomic_replace(
        &layout.profile_root.join("bench-harness.json"),
        manifest.to_string_lossy().as_bytes(),
    )?;
    Ok(manifest)
}
