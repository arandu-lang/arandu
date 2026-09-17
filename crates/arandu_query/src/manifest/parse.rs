//! Pure manifest string and bytes parsing without I/O or filesystem side effects.

use std::path::Path;

use super::model::{
    CapabilityPolicy, EffectPolicy, ManifestData, ManifestDependency, ManifestEdition,
    ManifestError, ManifestTarget, ManifestWorkspace, PackageKind, WasmConfig,
};
use super::schema::{LegacyManifest, ManifestSchema, RawDependency};
use super::validate::{validate_relative_path, validate_schema};

/// Parse `arandu.toml` from raw bytes. Does **not** perform I/O.
pub fn parse_manifest_bytes(path: &Path, bytes: &[u8]) -> Result<ManifestData, ManifestError> {
    let text = std::str::from_utf8(bytes).map_err(|error| ManifestError::Parse {
        path: path.to_path_buf(),
        message: format!("file is not valid UTF-8: {error}"),
    })?;
    parse_manifest_str(path, text)
}

/// Parse `arandu.toml` text. Does **not** read the filesystem.
///
/// Complete TOML syntax with a strict Arandu-owned schema. The legacy three-key
/// shape remains readable during the migration window.
pub fn parse_manifest_str(path: &Path, text: &str) -> Result<ManifestData, ManifestError> {
    let value: toml::Value = toml::from_str(text).map_err(|error| ManifestError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let data = if value.get("schema").is_some()
        || value.get("package").is_some()
        || value.get("targets").is_some()
    {
        let manifest: ManifestSchema = value.try_into().map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        validate_schema(path, &manifest)?;
        let binary_target = manifest.targets.bin.map(ManifestTarget::from);
        let library_target = manifest.targets.lib.map(ManifestTarget::from);
        let component_target = manifest.targets.component.map(ManifestTarget::from);
        let target_type = manifest.package.target_type.clone();
        let is_component =
            target_type.as_deref() == Some("component") || component_target.is_some();
        let Some(primary_target) = binary_target
            .as_ref()
            .or(library_target.as_ref())
            .or(component_target.as_ref())
        else {
            return Err(ManifestError::MissingField {
                path: path.to_path_buf(),
                field: "targets.bin, targets.lib, or targets.component",
            });
        };
        let primary_root = primary_target.root.clone();
        let kind = if is_component {
            PackageKind::Component
        } else {
            match (binary_target.is_some(), library_target.is_some()) {
                (true, false) => PackageKind::Binary,
                (false, true) => PackageKind::Library,
                (true, true) => PackageKind::Mixed,
                (false, false) => {
                    return Err(ManifestError::MissingField {
                        path: path.to_path_buf(),
                        field: "targets.bin or targets.lib",
                    });
                }
            }
        };
        ManifestData {
            name: manifest.package.name,
            version: manifest.package.version,
            entry: primary_root,
            schema: manifest.schema,
            edition: ManifestEdition::V2026,
            kind,
            toolchain_requirement: manifest.toolchain.map(|toolchain| toolchain.arandu),
            binary_target,
            library_target,
            component_target,
            target_type,
            wasm: manifest.wasm.map(|w| WasmConfig {
                memory_initial_pages: w.memory_initial_pages,
                enable_threads: w.enable_threads,
            }),
            capabilities: CapabilityPolicy {
                network: manifest.capabilities.network,
                filesystem_read: manifest.capabilities.filesystem_read,
                filesystem_write: manifest.capabilities.filesystem_write,
                environment: manifest.capabilities.environment,
                process: manifest.capabilities.process,
                foreign: manifest.capabilities.foreign,
            },
            effect_policy: manifest
                .policy
                .effects
                .map(|effects| EffectPolicy {
                    deny_new_authority: effects.deny_new_authority,
                    warn_new_resources: effects.warn_new_resources,
                    deny: effects.deny,
                })
                .unwrap_or_default(),
            dependencies: manifest
                .dependencies
                .into_iter()
                .map(|(alias, dependency)| {
                    let dependency = match dependency {
                        RawDependency::Path(dependency) => ManifestDependency::Path {
                            path: dependency.path,
                        },
                        RawDependency::Git(dependency) => ManifestDependency::Git {
                            origin: dependency.git,
                            commit: dependency.rev,
                        },
                    };
                    (alias, dependency)
                })
                .collect(),
            workspace: manifest.workspace.map(|workspace| ManifestWorkspace {
                members: workspace.members,
            }),
        }
    } else {
        for field in ["name", "version", "entry"] {
            if value.get(field).is_none() {
                return Err(ManifestError::MissingField {
                    path: path.to_path_buf(),
                    field,
                });
            }
        }
        let legacy: LegacyManifest = value.try_into().map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        ManifestData::legacy(legacy.name, legacy.version, legacy.entry)
    };

    if data.name.is_empty() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`name` must be non-empty".into(),
        });
    }
    if data.entry.is_empty() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`entry` must be non-empty".into(),
        });
    }
    semver::Version::parse(&data.version).map_err(|error| ManifestError::Parse {
        path: path.to_path_buf(),
        message: format!("`package.version` is not valid SemVer: {error}"),
    })?;
    validate_relative_path(path, &data.entry, "target root", false)?;
    // PROMOTE-L2: package name must not collide with stdlib roots.
    if let Err(e) = crate::vfs::validate_package_name(&data.name) {
        return Err(ManifestError::ReservedName {
            path: path.to_path_buf(),
            message: e.to_string(),
        });
    }

    Ok(data)
}
