//! Core project manifest domain models, targets, policies, and errors.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

/// Canonical on-disk filename for a project package.
pub const MANIFEST_FILENAME: &str = "arandu.toml";

/// Previous case-sensitive spelling, readable only during migration.
pub const LEGACY_MANIFEST_FILENAME: &str = "Arandu.toml";

/// Parsed package fields (MVP: name / version / entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestData {
    pub name: String,
    pub version: String,
    pub entry: String,
    pub schema: u32,
    pub edition: ManifestEdition,
    pub kind: PackageKind,
    pub toolchain_requirement: Option<String>,
    pub binary_target: Option<ManifestTarget>,
    pub library_target: Option<ManifestTarget>,
    pub component_target: Option<ManifestTarget>,
    pub target_type: Option<String>,
    pub wasm: Option<WasmConfig>,
    pub capabilities: CapabilityPolicy,
    pub effect_policy: EffectPolicy,
    /// Dependency requirements, ordered by import alias. Resolution lands in P4.
    pub dependencies: BTreeMap<String, ManifestDependency>,
    pub workspace: Option<ManifestWorkspace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestDependency {
    Path { path: String },
    Git { origin: String, commit: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestWorkspace {
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestEdition {
    Legacy,
    V2026,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Binary,
    Library,
    Mixed,
    Component,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTarget {
    pub name: String,
    pub root: String,
    /// Public logical module path → package-relative source path.
    pub exports: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityPolicy {
    pub network: Vec<String>,
    pub filesystem_read: Vec<String>,
    pub filesystem_write: Vec<String>,
    pub environment: Vec<String>,
    pub process: Vec<String>,
    pub foreign: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectPolicy {
    pub deny_new_authority: bool,
    pub warn_new_resources: bool,
    pub deny: Vec<String>,
}

impl Default for EffectPolicy {
    fn default() -> Self {
        Self {
            deny_new_authority: true,
            warn_new_resources: true,
            deny: vec!["UnknownCapability".into()],
        }
    }
}

impl ManifestData {
    #[must_use]
    pub fn legacy(name: String, version: String, entry: String) -> Self {
        Self {
            name: name.clone(),
            version,
            entry: entry.clone(),
            schema: 0,
            edition: ManifestEdition::Legacy,
            kind: PackageKind::Binary,
            toolchain_requirement: None,
            binary_target: Some(ManifestTarget {
                name,
                root: entry,
                exports: BTreeMap::new(),
            }),
            library_target: None,
            component_target: None,
            target_type: None,
            wasm: None,
            capabilities: CapabilityPolicy::default(),
            effect_policy: EffectPolicy::default(),
            dependencies: BTreeMap::new(),
            workspace: None,
        }
    }
}

/// WebAssembly specific configuration declared in `[wasm]` section of manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WasmConfig {
    pub memory_initial_pages: Option<u32>,
    pub enable_threads: Option<bool>,
}

/// Result of filesystem discovery before the manifest enters Salsa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDiscovery {
    pub path: PathBuf,
    pub spelling: ManifestSpelling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestSpelling {
    Canonical,
    Legacy,
}

/// Why reading or parsing `Arandu.toml` failed.
#[derive(Debug, Clone)]
pub enum ManifestError {
    Io {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    MissingField {
        path: PathBuf,
        field: &'static str,
    },
    /// Package `name` collides with a reserved stdlib/language root (PROMOTE-L2).
    ReservedName {
        path: PathBuf,
        message: String,
    },
    ConflictingFiles {
        canonical: PathBuf,
        legacy: PathBuf,
    },
    IncompatibleToolchain {
        path: PathBuf,
        required: String,
        current: String,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            ManifestError::Parse { path, message } => {
                write!(f, "malformed {}: {message}", path.display())
            }
            ManifestError::MissingField { path, field } => {
                write!(
                    f,
                    "malformed {}: missing required field `{field}`",
                    path.display()
                )
            }
            ManifestError::ReservedName { path, message } => {
                write!(f, "invalid {}: {message}", path.display())
            }
            ManifestError::ConflictingFiles { canonical, legacy } => write!(
                f,
                "conflicting package manifests: both {} and {} exist; keep only `{MANIFEST_FILENAME}`",
                canonical.display(),
                legacy.display()
            ),
            ManifestError::IncompatibleToolchain {
                path,
                required,
                current,
            } => write!(
                f,
                "incompatible {}: requires Arandu `{required}`, current toolchain is `{current}`",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ManifestError {}
