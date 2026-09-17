//! TOML deserialization schema representations for `arandu.toml`.

use serde::Deserialize;
use std::collections::BTreeMap;

use super::model::{EffectPolicy, ManifestTarget};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestSchema {
    pub(super) schema: u32,
    pub(super) package: PackageSection,
    #[serde(default)]
    pub(super) toolchain: Option<ToolchainSection>,
    pub(super) targets: TargetsSection,
    #[serde(default)]
    pub(super) dependencies: BTreeMap<String, RawDependency>,
    #[serde(default)]
    pub(super) capabilities: RawCapabilityPolicy,
    #[serde(default)]
    pub(super) policy: RawPolicy,
    #[serde(default)]
    pub(super) metadata: toml::Table,
    #[serde(default)]
    pub(super) workspace: Option<WorkspaceSection>,
    #[serde(default)]
    pub(super) wasm: Option<WasmSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackageSection {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) edition: String,
    #[serde(rename = "target-type", default)]
    pub(super) target_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolchainSection {
    pub(super) arandu: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TargetsSection {
    #[serde(default)]
    pub(super) bin: Option<TargetSection>,
    #[serde(default)]
    pub(super) lib: Option<TargetSection>,
    #[serde(default)]
    pub(super) component: Option<TargetSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TargetSection {
    pub(super) name: String,
    pub(super) root: String,
    #[serde(default)]
    pub(super) exports: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PathDependency {
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum RawDependency {
    Path(PathDependency),
    Git(GitDependency),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitDependency {
    pub(super) git: String,
    pub(super) rev: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceSection {
    pub(super) members: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCapabilityPolicy {
    #[serde(default)]
    pub(super) network: Vec<String>,
    #[serde(default)]
    pub(super) filesystem_read: Vec<String>,
    #[serde(default)]
    pub(super) filesystem_write: Vec<String>,
    #[serde(default)]
    pub(super) environment: Vec<String>,
    #[serde(default)]
    pub(super) process: Vec<String>,
    #[serde(default)]
    pub(super) foreign: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPolicy {
    #[serde(default)]
    pub(super) effects: Option<RawEffectPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawEffectPolicy {
    pub(super) deny_new_authority: bool,
    pub(super) warn_new_resources: bool,
    pub(super) deny: Vec<String>,
}

impl Default for RawEffectPolicy {
    fn default() -> Self {
        let policy = EffectPolicy::default();
        Self {
            deny_new_authority: policy.deny_new_authority,
            warn_new_resources: policy.warn_new_resources,
            deny: policy.deny,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyManifest {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) entry: String,
}

impl From<TargetSection> for ManifestTarget {
    fn from(target: TargetSection) -> Self {
        Self {
            name: target.name,
            root: target.root,
            exports: target.exports,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WasmSection {
    #[serde(rename = "memory-initial-pages", default)]
    pub(super) memory_initial_pages: Option<u32>,
    #[serde(rename = "enable-threads", default)]
    pub(super) enable_threads: Option<bool>,
}
