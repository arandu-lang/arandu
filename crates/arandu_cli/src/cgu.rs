//! Partitioned AOT Code Generation Unit cache.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use arandu_backend_cranelift::object::{self, Object};
use arandu_backend_cranelift::{AotOptimization, CodegenUnit, TargetArchitecture, Triple};
use arandu_middle::amir::AmirProgram;
use arandu_middle::symbol_table::SymbolTable;
use arandu_semantics::TypeInfo;
use serde::{Deserialize, Serialize};

use crate::artifact;
use crate::cli_error::CliFailure;

const CGU_METADATA_SCHEMA: u32 = 1;
const DEFAULT_CGU_CACHE_LIMIT: u64 = 500 * 1024 * 1024;

/// Partitioning output carrying relocatable object paths and cache stats.
#[derive(Debug)]
pub struct PartitionedBuildResult {
    pub units: Vec<CodegenUnit>,
    pub recompiled_units: Vec<(CodegenUnit, Vec<u8>)>,
    pub object_files: Vec<PathBuf>,
    pub cached_count: usize,
    pub recompiled_count: usize,
    // Held until linking/publication completes so another process cannot GC
    // an object between cache validation and linker open.
    _cache_lock: File,
}

#[derive(Debug, Serialize)]
struct CguCacheMetadata<'a> {
    schema: u32,
    input_hash: &'a str,
    object_hash: &'a str,
}

#[derive(Debug, Deserialize)]
struct OwnedCguCacheMetadata {
    schema: u32,
    input_hash: String,
    object_hash: String,
}

/// Compile `program` partitioned into CGUs, reusing only cryptographically
/// validated relocatable objects.
pub fn compile_partitioned(
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &TypeInfo,
    target: &Triple,
    optimization: AotOptimization,
    toolchain_fingerprint: &str,
    cgu_cache_dir: &Path,
) -> Result<PartitionedBuildResult, CliFailure> {
    fs::create_dir_all(cgu_cache_dir).map_err(|error| {
        failure(
            "create CGU cache directory",
            cgu_cache_dir,
            error.to_string(),
        )
    })?;
    let lock_path = cgu_cache_dir.join(".cache.lock");
    let cache_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| failure("open CGU cache lock", &lock_path, error.to_string()))?;
    cache_lock
        .lock()
        .map_err(|error| failure("lock CGU cache", &lock_path, error.to_string()))?;

    let units = arandu_backend_cranelift::partition_program(
        program,
        symbols,
        type_info,
        target,
        optimization,
        toolchain_fingerprint,
    );
    let mut object_files = Vec::with_capacity(units.len());
    let mut recompiled_units = Vec::new();
    let mut cached_count = 0;
    let mut recompiled_count = 0;

    for unit in &units {
        let cgu_path = object_path(cgu_cache_dir, unit);
        let metadata_path = metadata_path(&cgu_path);

        if cached_object_is_valid(&cgu_path, &metadata_path, &unit.hash, target) {
            cached_count += 1;
            object_files.push(cgu_path);
            continue;
        }

        recompiled_count += 1;
        let bytes = arandu_backend_cranelift::compile_cgu(
            unit,
            program,
            symbols,
            type_info,
            target,
            optimization,
        )
        .map_err(|diagnostic| CliFailure::diagnostics(std::iter::once(diagnostic), None))?;
        validate_object(&bytes, target).map_err(|reason| {
            failure(
                "validate emitted CGU object",
                &cgu_path,
                format!("backend emitted an invalid relocatable object: {reason}"),
            )
        })?;

        let object_hash = blake3::hash(&bytes).to_hex().to_string();
        let metadata = CguCacheMetadata {
            schema: CGU_METADATA_SCHEMA,
            input_hash: &unit.hash,
            object_hash: &object_hash,
        };
        let mut encoded = serde_json::to_vec_pretty(&metadata).map_err(|error| {
            failure(
                "serialize CGU cache metadata",
                &metadata_path,
                error.to_string(),
            )
        })?;
        encoded.push(b'\n');

        // Publish the object first and its validating sidecar last. An
        // interrupted build therefore leaves a cache miss, never a false hit.
        artifact::atomic_replace(&cgu_path, &bytes)?;
        artifact::atomic_replace(&metadata_path, &encoded)?;

        recompiled_units.push((unit.clone(), bytes));
        object_files.push(cgu_path);
    }

    let live: BTreeSet<PathBuf> = object_files.iter().cloned().collect();
    collect_cache(cgu_cache_dir, &live, DEFAULT_CGU_CACHE_LIMIT)?;

    Ok(PartitionedBuildResult {
        units,
        recompiled_units,
        object_files,
        cached_count,
        recompiled_count,
        _cache_lock: cache_lock,
    })
}

fn object_path(cache_dir: &Path, unit: &CodegenUnit) -> PathBuf {
    let mut safe_name: String = unit
        .name
        .chars()
        .take(80)
        .map(|character| {
            // Cache filenames must have the same bound in bytes and UTF-16
            // units, even when a source symbol contains Unicode.
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    while safe_name.starts_with('-') {
        safe_name.remove(0);
    }
    if safe_name.is_empty() {
        safe_name = "cgu".to_owned();
    }
    let extension = if cfg!(windows) { "obj" } else { "o" };
    cache_dir.join(format!("{safe_name}-{}.{extension}", unit.hash))
}

fn metadata_path(object_path: &Path) -> PathBuf {
    let mut name = object_path.as_os_str().to_owned();
    name.push(".meta.json");
    PathBuf::from(name)
}

fn cached_object_is_valid(
    object_path: &Path,
    metadata_path: &Path,
    expected_input_hash: &str,
    target: &Triple,
) -> bool {
    let Ok(metadata_bytes) = fs::read(metadata_path) else {
        return false;
    };
    let Ok(metadata) = serde_json::from_slice::<OwnedCguCacheMetadata>(&metadata_bytes) else {
        return false;
    };
    if metadata.schema != CGU_METADATA_SCHEMA || metadata.input_hash != expected_input_hash {
        return false;
    }
    let Ok(bytes) = fs::read(object_path) else {
        return false;
    };
    if bytes.is_empty()
        || blake3::hash(&bytes).to_hex().as_str() != metadata.object_hash
        || validate_object(&bytes, target).is_err()
    {
        return false;
    }
    true
}

fn validate_object(bytes: &[u8], target: &Triple) -> Result<(), String> {
    let file = object::File::parse(bytes).map_err(|error| error.to_string())?;
    if file.kind() != object::ObjectKind::Relocatable {
        return Err(format!(
            "expected relocatable object, found {:?}",
            file.kind()
        ));
    }
    let expected = match target.architecture {
        TargetArchitecture::X86_32(_) => object::Architecture::I386,
        TargetArchitecture::X86_64 | TargetArchitecture::X86_64h => object::Architecture::X86_64,
        TargetArchitecture::Aarch64(_) => object::Architecture::Aarch64,
        architecture => {
            return Err(format!(
                "CGU cache validation does not support target architecture {architecture:?}"
            ));
        }
    };
    if file.architecture() != expected {
        return Err(format!(
            "expected architecture {expected:?}, found {:?}",
            file.architecture()
        ));
    }
    Ok(())
}

fn collect_cache(
    cache_dir: &Path,
    live_objects: &BTreeSet<PathBuf>,
    limit: u64,
) -> Result<(), CliFailure> {
    let mut entries = Vec::new();
    let mut total = 0_u64;
    for entry in fs::read_dir(cache_dir)
        .map_err(|error| failure("scan CGU cache", cache_dir, error.to_string()))?
    {
        let entry =
            entry.map_err(|error| failure("read CGU cache entry", cache_dir, error.to_string()))?;
        let path = entry.path();
        if !is_cgu_object_path(&path) {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| failure("inspect CGU cache entry", &path, error.to_string()))?;
        let len = metadata.len();
        total = total
            .checked_add(len)
            .ok_or_else(|| failure("measure CGU cache", cache_dir, "cache size overflowed u64"))?;
        entries.push((
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path,
            len,
        ));
    }

    if total <= limit {
        return Ok(());
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, path, len) in entries {
        if total <= limit {
            break;
        }
        if live_objects.contains(&path) {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                total = total.saturating_sub(len);
                let sidecar = metadata_path(&path);
                match fs::remove_file(&sidecar) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(failure(
                            "remove stale CGU cache metadata",
                            &sidecar,
                            error.to_string(),
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failure(
                    "remove stale CGU cache object",
                    &path,
                    error.to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn is_cgu_object_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("o" | "obj")
    )
}

fn failure(operation: &'static str, path: &Path, source: impl Into<String>) -> CliFailure {
    CliFailure::operational(operation, Some(path.to_path_buf()), source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_path_uses_the_complete_digest() {
        let unit = CodegenUnit {
            name: "hello/world".to_owned(),
            symbol: arandu_middle::SymbolId::new(0, 0),
            hash: "a".repeat(64),
        };
        let path = object_path(Path::new("cache"), &unit);
        assert!(path.to_string_lossy().contains(&"a".repeat(64)));
        assert!(path.to_string_lossy().contains("hello_world"));
    }

    #[test]
    fn object_path_sanitizes_leading_dashes_and_empty_names() {
        let unit_dash = CodegenUnit {
            name: "-flag/test".to_owned(),
            symbol: arandu_middle::SymbolId::new(0, 0),
            hash: "b".repeat(64),
        };
        let path = object_path(Path::new("cache"), &unit_dash);
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert!(!file_name.starts_with('-'));
        assert!(file_name.starts_with("flag_test-"));

        let unit_empty = CodegenUnit {
            name: "---".to_owned(),
            symbol: arandu_middle::SymbolId::new(0, 0),
            hash: "c".repeat(64),
        };
        let path_empty = object_path(Path::new("cache"), &unit_empty);
        let file_name_empty = path_empty.file_name().unwrap().to_str().unwrap();
        assert!(file_name_empty.starts_with("cgu-"));
    }

    #[test]
    fn object_path_bounds_unicode_names_without_shortening_the_digest() {
        let unit = CodegenUnit {
            name: "変数".repeat(80),
            symbol: arandu_middle::SymbolId::new(0, 0),
            hash: "d".repeat(64),
        };
        let path = object_path(Path::new("cache"), &unit);
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.is_ascii());
        assert_eq!(name.len(), if cfg!(windows) { 149 } else { 147 });
        assert!(name.contains(&unit.hash));
    }

    #[test]
    fn cache_collection_preserves_live_objects() {
        let directory = std::env::temp_dir().join(format!(
            "arandu-cgu-gc-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create temporary CGU cache");
        let live = directory.join("live.o");
        let stale = directory.join("stale.o");
        fs::write(&live, [0_u8; 8]).expect("write live object");
        fs::write(&stale, [0_u8; 8]).expect("write stale object");
        fs::write(metadata_path(&stale), b"{}").expect("write stale metadata");

        collect_cache(&directory, &BTreeSet::from([live.clone()]), 8).expect("collect cache");
        assert!(live.is_file());
        assert!(!stale.exists());
        assert!(!metadata_path(&stale).exists());

        fs::remove_dir_all(directory).expect("remove temporary CGU cache");
    }
}
