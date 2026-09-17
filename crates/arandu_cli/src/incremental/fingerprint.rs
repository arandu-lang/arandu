//! Content-addressed session fingerprint for CLI incremental builds.
//!
//! Persistent state is treated as untrusted: paths are constrained to the
//! profile directory and both inputs and final artifacts are verified by
//! BLAKE3 before an early cutoff is accepted.

use std::collections::BTreeMap;
use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use arandu_lexer::TokenKind;
use serde::{Deserialize, Serialize};

use crate::artifact::{self, NativeProfile};
use crate::cli_error::CliFailure;

const SCHEMA_VERSION: u32 = 2;
const FINGERPRINT_FILENAME: &str = "session-fingerprint.json";

/// One external input participating in the end-to-end build closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncrementalInput {
    pub key: String,
    pub path: PathBuf,
    /// Whether trivia and documentation-only changes may use semantic cutoff.
    pub semantic_source: bool,
}

/// Fingerprint of an individual build input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub mtime_nanos: Option<u128>,
    pub size: u64,
    pub content_hash: String,
    pub semantic_hash: Option<String>,
}

impl FileFingerprint {
    pub fn from_path(path: &Path, semantic_source: bool) -> Result<Self, std::io::Error> {
        let metadata = fs::metadata(path)?;
        let content = fs::read(path)?;
        Ok(Self {
            mtime_nanos: metadata_mtime_nanos(&metadata),
            size: metadata.len(),
            content_hash: blake3::hash(&content).to_hex().to_string(),
            semantic_hash: semantic_source_hash(&content, semantic_source),
        })
    }

    pub fn matches_metadata(&self, metadata: &Metadata) -> bool {
        self.mtime_nanos.is_some()
            && metadata_mtime_nanos(metadata) == self.mtime_nanos
            && metadata.len() == self.size
    }
}

pub fn metadata_mtime_nanos(metadata: &Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
}

/// Project-wide incremental session state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFingerprint {
    pub schema_version: u32,
    pub compiler_version: String,
    pub package: String,
    pub version: String,
    pub profile: String,
    pub target: String,
    pub pointer_width: u64,
    pub opt: bool,
    pub artifact_relative: String,
    pub artifact_digest: String,
    pub source_files: BTreeMap<String, FileFingerprint>,
}

/// Result of checking whether an existing session fingerprint is still valid.
#[derive(Debug)]
pub enum IncrementalCheck {
    UpToDate {
        artifact_path: PathBuf,
    },
    NeedsRebuild {
        reason: &'static str,
        /// Current input fingerprints captured after a safe cache miss.
        ///
        /// The caller may move these into the replacement session instead of
        /// reading unchanged toolchain, runtime and source inputs again.
        reusable_input_fingerprints: Option<BTreeMap<String, FileFingerprint>>,
    },
}

/// Compilation configuration for incremental session tracking.
#[derive(Clone, Copy, Debug)]
pub struct SessionConfig<'a> {
    pub project_root: &'a Path,
    pub package: &'a str,
    pub version: &'a str,
    pub profile: NativeProfile,
    pub pointer_width: u64,
    pub opt: bool,
    pub manifest_path: &'a Path,
    pub extra_inputs: &'a [IncrementalInput],
    pub target_triple: Option<&'a str>,
}

/// Check if an incremental build can be skipped entirely (early cutoff).
pub fn check_incremental(config: &SessionConfig<'_>) -> IncrementalCheck {
    let layout = match config.target_triple {
        Some(triple) => {
            artifact::layout_for_target(config.project_root, config.profile.directory(), triple)
        }
        None => artifact::layout(config.project_root, config.profile.directory()),
    };
    let fingerprint_path = layout.incremental.join(FINGERPRINT_FILENAME);
    let bytes = match fs::read(&fingerprint_path) {
        Ok(bytes) => bytes,
        Err(_) => return rebuild("no previous incremental session found"),
    };
    let session: SessionFingerprint = match serde_json::from_slice(&bytes) {
        Ok(session) => session,
        Err(_) => return rebuild("corrupted or incompatible incremental session fingerprint"),
    };

    if session.schema_version != SCHEMA_VERSION
        || session.compiler_version != crate::project::ARANDU_VERSION
        || session.package != config.package
        || session.version != config.version
        || session.profile != config.profile.directory()
        || session.target != layout.triple
        || session.pointer_width != config.pointer_width
        || session.opt != config.opt
    {
        return rebuild("compilation configuration or compiler version changed");
    }

    let Some(relative_artifact) = validated_relative_path(&session.artifact_relative) else {
        return rebuild("incremental session contains an unsafe artifact path");
    };
    let artifact_full = layout.profile_root.join(relative_artifact);
    let current_files = collect_project_file_paths(config);
    if current_files.len() != session.source_files.len() {
        return rebuild_with_inputs(
            "build input count changed",
            capture_input_fingerprints(&current_files, &session.source_files).ok(),
        );
    }
    for (key, input) in &current_files {
        let Some(saved) = session.source_files.get(key) else {
            return rebuild_with_inputs(
                "new build input detected",
                capture_input_fingerprints(&current_files, &session.source_files).ok(),
            );
        };
        let metadata = match fs::metadata(&input.path) {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => return rebuild("cannot stat a required build input"),
        };
        if saved.matches_metadata(&metadata) {
            continue;
        }
        let current = match FileFingerprint::from_path(&input.path, input.semantic_source) {
            Ok(current) => current,
            Err(_) => return rebuild("cannot read a modified build input"),
        };
        if current.content_hash == saved.content_hash {
            continue;
        }
        if saved.semantic_hash.is_some() && saved.semantic_hash == current.semantic_hash {
            continue;
        }
        return rebuild_with_inputs(
            "build input content modified",
            capture_input_fingerprints(&current_files, &session.source_files).ok(),
        );
    }

    // The artifact is usable only after the complete input closure cut off.
    // Defer its potentially large read and digest so a known source/configuration
    // change takes the rebuild path without hashing an artifact that will be replaced.
    let artifact_bytes = match fs::read(&artifact_full) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        _ => {
            return rebuild_with_inputs(
                "cached native artifact missing or empty on disk",
                capture_input_fingerprints(&current_files, &session.source_files).ok(),
            );
        }
    };
    if blake3::hash(&artifact_bytes).to_hex().as_str() != session.artifact_digest {
        return rebuild_with_inputs(
            "cached native artifact failed its BLAKE3 integrity check",
            capture_input_fingerprints(&current_files, &session.source_files).ok(),
        );
    }

    IncrementalCheck::UpToDate {
        artifact_path: artifact_full,
    }
}

/// Record a successful build session into the incremental cache directory.
pub fn record_session(
    config: &SessionConfig<'_>,
    artifact_full_path: &Path,
    published_artifact_digest: Option<String>,
    reusable_input_fingerprints: Option<BTreeMap<String, FileFingerprint>>,
) -> Result<(), CliFailure> {
    let layout = match config.target_triple {
        Some(triple) => {
            artifact::layout_for_target(config.project_root, config.profile.directory(), triple)
        }
        None => artifact::layout(config.project_root, config.profile.directory()),
    };
    fs::create_dir_all(&layout.incremental).map_err(|error| {
        CliFailure::operational(
            "create incremental cache directory",
            Some(layout.incremental.clone()),
            error.to_string(),
        )
    })?;

    let relative = artifact_full_path
        .strip_prefix(&layout.profile_root)
        .map_err(|_| {
            CliFailure::operational(
                "record incremental artifact",
                Some(artifact_full_path.to_path_buf()),
                "artifact is outside the active build profile",
            )
        })?;
    if !is_safe_relative_path(relative) {
        return Err(CliFailure::operational(
            "record incremental artifact",
            Some(artifact_full_path.to_path_buf()),
            "artifact path is not a safe profile-relative path",
        ));
    }
    let artifact_relative = relative.to_string_lossy().into_owned();
    // Publication already hashed the exact staged bytes to derive the
    // content-addressed filename. Move that digest into the session rather
    // than reading the executable again; every future cutoff still re-hashes
    // the artifact before accepting it.
    let artifact_digest = match published_artifact_digest {
        Some(digest) => digest,
        None => content_digest(artifact_full_path)?,
    };

    let source_files = if let Some(fingerprints) = reusable_input_fingerprints {
        fingerprints
    } else {
        capture_input_fingerprints(&collect_project_file_paths(config), &BTreeMap::new()).map_err(
            |(path, error)| {
                CliFailure::operational("fingerprint build input", Some(path), error.to_string())
            },
        )?
    };

    let session = SessionFingerprint {
        schema_version: SCHEMA_VERSION,
        compiler_version: crate::project::ARANDU_VERSION.to_owned(),
        package: config.package.to_owned(),
        version: config.version.to_owned(),
        profile: config.profile.directory().to_owned(),
        target: layout.triple,
        pointer_width: config.pointer_width,
        opt: config.opt,
        artifact_relative,
        artifact_digest,
        source_files,
    };
    let mut encoded = serde_json::to_vec_pretty(&session).map_err(|error| {
        CliFailure::operational("serialize session fingerprint", None, error.to_string())
    })?;
    encoded.push(b'\n');
    artifact::atomic_replace(&layout.incremental.join(FINGERPRINT_FILENAME), &encoded)
}

/// Compute a BLAKE3 digest for a required build input with a descriptive error.
pub fn content_digest(path: &Path) -> Result<String, CliFailure> {
    let bytes = fs::read(path).map_err(|error| {
        CliFailure::operational(
            "fingerprint build input",
            Some(path.to_path_buf()),
            error.to_string(),
        )
    })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn semantic_source_hash(content: &[u8], enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    let source = std::str::from_utf8(content).ok()?;
    let lexed = arandu_lexer::lex_recovering(source);
    if !lexed.diagnostics.is_empty() {
        return None;
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"arandu-semantic-source-v1\0");
    for token in &lexed.tokens {
        if matches!(token.kind, TokenKind::DocComment | TokenKind::Eof) {
            continue;
        }
        let index = u16::try_from(token.kind.index()).ok()?;
        hash.update(&index.to_le_bytes());
        hash.update(&[u8::from(token.inserted)]);
        let lexeme = token.lexeme(source).as_bytes();
        hash.update(&u64::try_from(lexeme.len()).ok()?.to_le_bytes());
        hash.update(lexeme);
    }
    Some(hash.finalize().to_hex().to_string())
}

fn collect_project_file_paths(config: &SessionConfig<'_>) -> BTreeMap<String, IncrementalInput> {
    let mut files = BTreeMap::new();
    files.insert(
        "manifest".to_owned(),
        IncrementalInput {
            key: "manifest".to_owned(),
            path: config.manifest_path.to_path_buf(),
            semantic_source: false,
        },
    );
    if let Some(parent) = config.manifest_path.parent() {
        let lockfile = parent.join(arandu_query::LOCK_FILENAME);
        if lockfile.is_file() {
            files.insert(
                "lockfile".to_owned(),
                IncrementalInput {
                    key: "lockfile".to_owned(),
                    path: lockfile,
                    semantic_source: false,
                },
            );
        }
    }
    for input in config.extra_inputs {
        files.insert(input.key.clone(), input.clone());
    }
    files
}

fn capture_input_fingerprints(
    inputs: &BTreeMap<String, IncrementalInput>,
    saved: &BTreeMap<String, FileFingerprint>,
) -> Result<BTreeMap<String, FileFingerprint>, (PathBuf, std::io::Error)> {
    let mut fingerprints = BTreeMap::new();
    for (key, input) in inputs {
        let metadata = fs::metadata(&input.path).map_err(|error| (input.path.clone(), error))?;
        let fingerprint = if let Some(saved) = saved.get(key)
            && saved.matches_metadata(&metadata)
        {
            saved.clone()
        } else {
            FileFingerprint::from_path(&input.path, input.semantic_source)
                .map_err(|error| (input.path.clone(), error))?
        };
        fingerprints.insert(key.clone(), fingerprint);
    }
    Ok(fingerprints)
}

fn validated_relative_path(value: &str) -> Option<&Path> {
    let path = Path::new(value);
    is_safe_relative_path(path).then_some(path)
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn rebuild(reason: &'static str) -> IncrementalCheck {
    rebuild_with_inputs(reason, None)
}

fn rebuild_with_inputs(
    reason: &'static str,
    reusable_input_fingerprints: Option<BTreeMap<String, FileFingerprint>>,
) -> IncrementalCheck {
    IncrementalCheck::NeedsRebuild {
        reason,
        reusable_input_fingerprints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_hash_ignores_docs_and_trivia_but_not_code() {
        let first = semantic_source_hash(b"/// one\nfunc main(): int { return 1 }\n", true);
        let docs = semantic_source_hash(b"/// two\n\nfunc main(): int {  return 1 }\n", true);
        let code = semantic_source_hash(b"/// two\nfunc main(): int { return 2 }\n", true);
        assert_eq!(first, docs);
        assert_ne!(first, code);
    }

    #[test]
    fn rejects_absolute_and_parent_artifact_paths() {
        assert!(validated_relative_path("bin/app").is_some());
        assert!(validated_relative_path("../outside").is_none());
        assert!(validated_relative_path("/outside").is_none());
        assert!(validated_relative_path("").is_none());
    }
}
