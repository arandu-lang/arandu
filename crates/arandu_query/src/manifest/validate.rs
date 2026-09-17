//! Pure semantic validation for parsed manifest structures.

use std::collections::BTreeSet;
use std::path::Path;

use super::model::{ManifestData, ManifestError};
use super::schema::{ManifestSchema, RawDependency, TargetSection};

pub(super) fn validate_schema(path: &Path, manifest: &ManifestSchema) -> Result<(), ManifestError> {
    if manifest.schema != 1 {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("unsupported schema {}; expected 1", manifest.schema),
        });
    }
    if manifest.package.edition != "2026" {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!(
                "unsupported package edition `{}`; expected `2026`",
                manifest.package.edition
            ),
        });
    }
    if let Some(toolchain) = &manifest.toolchain {
        semver::VersionReq::parse(&toolchain.arandu).map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("invalid `toolchain.arandu` requirement: {error}"),
        })?;
    }
    for target in [
        manifest.targets.bin.as_ref(),
        manifest.targets.lib.as_ref(),
        manifest.targets.component.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if target.name.is_empty() {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: "target `name` must be non-empty".into(),
            });
        }
        validate_relative_path(path, &target.root, "target root", false)?;
    }
    if manifest
        .targets
        .bin
        .as_ref()
        .is_some_and(|target| !target.exports.is_empty())
    {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`targets.bin.exports` is invalid; only a library target exports modules"
                .into(),
        });
    }
    if let Some(library) = &manifest.targets.lib {
        validate_exports(path, library)?;
    }
    if let Some(component) = &manifest.targets.component {
        validate_exports(path, component)?;
    }
    for (alias, dependency) in &manifest.dependencies {
        crate::vfs::validate_package_name(alias).map_err(|error| ManifestError::ReservedName {
            path: path.to_path_buf(),
            message: format!("invalid dependency alias `{alias}`: {error}"),
        })?;
        match dependency {
            RawDependency::Path(dependency) => {
                validate_relative_path(path, &dependency.path, "dependency path", true)?;
            }
            RawDependency::Git(dependency) => {
                validate_git_origin(path, &dependency.git)?;
                validate_git_commit(path, &dependency.rev)?;
            }
        }
    }
    if let Some(workspace) = &manifest.workspace {
        let mut members = BTreeSet::new();
        let mut folded = BTreeSet::new();
        for member in &workspace.members {
            validate_relative_path(path, member, "workspace member", false)?;
            let canonical = member.trim_end_matches('/');
            if canonical.is_empty()
                || !members.insert(canonical)
                || !folded.insert(canonical.to_ascii_lowercase())
            {
                return Err(ManifestError::Parse {
                    path: path.to_path_buf(),
                    message: format!("duplicate or case-colliding workspace member `{member}`"),
                });
            }
        }
    }
    for (name, values) in [
        ("capabilities.network", &manifest.capabilities.network),
        (
            "capabilities.filesystem_read",
            &manifest.capabilities.filesystem_read,
        ),
        (
            "capabilities.filesystem_write",
            &manifest.capabilities.filesystem_write,
        ),
        (
            "capabilities.environment",
            &manifest.capabilities.environment,
        ),
        ("capabilities.process", &manifest.capabilities.process),
    ] {
        validate_policy_values(path, name, values)?;
    }
    if let Some(effects) = &manifest.policy.effects {
        validate_policy_values(path, "policy.effects.deny", &effects.deny)?;
        if effects.deny.iter().any(|effect| {
            !effect
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_uppercase())
        }) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: "`policy.effects.deny` names must use PascalCase".into(),
            });
        }
    }
    let _ = &manifest.metadata;
    Ok(())
}

pub fn validate_git_origin(path: &Path, origin: &str) -> Result<(), ManifestError> {
    validate_git_dependency_identity(origin, "0123456789abcdef0123456789abcdef01234567").map_err(
        |message| ManifestError::Parse {
            path: path.to_path_buf(),
            message,
        },
    )?;
    Ok(())
}

pub fn validate_git_dependency_identity(origin: &str, commit: &str) -> Result<(), String> {
    if origin.len() > 2048
        || !origin.is_ascii()
        || origin.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(
            "dependency `git` origin must be an ASCII HTTPS URL no longer than 2048 bytes".into(),
        );
    }
    let Some(remainder) = origin.strip_prefix("https://") else {
        return Err("dependency `git` origin must use canonical `https://` transport".into());
    };
    if remainder.contains(['@', '?', '#', '\\']) {
        return Err(
            "dependency `git` origin must not contain credentials, query, fragment or backslash"
                .into(),
        );
    }
    let Some((host, repository)) = remainder.split_once('/') else {
        return Err("dependency `git` origin must include a host and repository path".into());
    };
    if host.is_empty()
        || host.contains(':')
        || host != host.to_ascii_lowercase()
        || host.starts_with('.')
        || host.ends_with('.')
        || host.split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(
            "dependency `git` origin must use a lowercase DNS host without a custom port".into(),
        );
    }
    if !repository.ends_with(".git")
        || repository.split('/').any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(
            "dependency `git` origin must use a portable repository path ending in `.git`".into(),
        );
    }
    validate_git_commit_value(commit)?;
    Ok(())
}

pub fn validate_git_commit(path: &Path, commit: &str) -> Result<(), ManifestError> {
    validate_git_commit_value(commit).map_err(|message| ManifestError::Parse {
        path: path.to_path_buf(),
        message,
    })
}

pub fn validate_git_commit_value(commit: &str) -> Result<(), String> {
    if !matches!(commit.len(), 40 | 64)
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("dependency `rev` must be a complete 40- or 64-character lowercase hexadecimal commit ID".into());
    }
    Ok(())
}

pub(super) fn validate_exports(path: &Path, target: &TargetSection) -> Result<(), ManifestError> {
    let mut folded_names = BTreeSet::new();
    let mut folded_paths = BTreeSet::new();
    for (public_name, source) in &target.exports {
        if public_name != "." && !public_name.split('.').all(is_portable_module_segment) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: format!("invalid library export name `{public_name}`"),
            });
        }
        validate_relative_path(path, source, "library export target", false)?;
        if !source.ends_with(".aru") {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: format!("library export target `{source}` must be an `.aru` file"),
            });
        }
        if !folded_names.insert(public_name.to_ascii_lowercase()) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: format!("case-fold collision in library export `{public_name}`"),
            });
        }
        if !folded_paths.insert(source.to_ascii_lowercase()) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: format!("duplicate library export target `{source}`"),
            });
        }
    }
    Ok(())
}

pub fn is_portable_module_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

pub fn validate_policy_values(
    path: &Path,
    field: &str,
    values: &[String],
) -> Result<(), ManifestError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.is_empty() || !seen.insert(value) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: format!("`{field}` contains an empty or duplicate value"),
            });
        }
    }
    Ok(())
}

pub fn ensure_toolchain_compatible(
    path: &Path,
    manifest: &ManifestData,
    current: &str,
) -> Result<(), ManifestError> {
    let Some(required) = &manifest.toolchain_requirement else {
        return Ok(());
    };
    let current_version =
        semver::Version::parse(current).map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("compiler version `{current}` is not valid SemVer: {error}"),
        })?;
    let requirement =
        semver::VersionReq::parse(required).map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("invalid `toolchain.arandu` requirement: {error}"),
        })?;
    if requirement.matches(&current_version) {
        Ok(())
    } else {
        Err(ManifestError::IncompatibleToolchain {
            path: path.to_path_buf(),
            required: required.clone(),
            current: current.to_owned(),
        })
    }
}

pub fn validate_relative_path(
    manifest_path: &Path,
    value: &str,
    field: &str,
    allow_parent: bool,
) -> Result<(), ManifestError> {
    let candidate = Path::new(value);
    let bytes = value.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let rooted = value.starts_with('/') || value.starts_with("//") || windows_drive;
    let has_parent = candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir));
    if value.is_empty()
        || candidate.is_absolute()
        || rooted
        || (!allow_parent && has_parent)
        || value.contains('\\')
    {
        return Err(ManifestError::Parse {
            path: manifest_path.to_path_buf(),
            message: format!(
                "unsafe `{field}` `{value}`; use a non-empty relative path with `/` separators"
            ),
        });
    }
    Ok(())
}
