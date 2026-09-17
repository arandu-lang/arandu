//! Pure, deterministic `arandu.lock` model. Filesystem policy belongs to the CLI.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::manifest::{ManifestData, ManifestEdition, PackageKind};

pub const LOCK_FILENAME: &str = "arandu.lock";
pub const LOCK_VERSION: u32 = 2;
const DIGEST_PREFIX: &str = "blake3:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    pub manifest_fingerprint: String,
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: String,
    pub manifest_fingerprint: String,
    pub origin: Option<String>,
    pub commit: Option<String>,
    pub content_digest: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockfileError(String);

impl fmt::Display for LockfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LockfileError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockfile {
    version: u32,
    manifest_fingerprint: String,
    package: Vec<RawPackage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: String,
    source: String,
    #[serde(default)]
    manifest_fingerprint: String,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    content_digest: Option<String>,
    dependencies: Vec<String>,
}

impl Lockfile {
    /// Stable, review-oriented changes between two resolved package graphs.
    #[must_use]
    pub fn graph_diff(&self, next: &Self) -> Vec<String> {
        fn lines(lock: &Lockfile) -> BTreeSet<String> {
            let mut lines = BTreeSet::new();
            for package in &lock.packages {
                lines.insert(format!(
                    "package {} name={} version={} origin={} commit={} digest={}",
                    package.source,
                    package.name,
                    package.version,
                    package.origin.as_deref().unwrap_or("local"),
                    package.commit.as_deref().unwrap_or("local"),
                    package.content_digest.as_deref().unwrap_or("local")
                ));
                for dependency in &package.dependencies {
                    lines.insert(format!("edge {} -> {dependency}", package.source));
                }
            }
            lines
        }
        let current = lines(self);
        let next = lines(next);
        current
            .difference(&next)
            .map(|line| format!("- {line}"))
            .chain(next.difference(&current).map(|line| format!("+ {line}")))
            .collect()
    }

    #[must_use]
    pub fn contains_remote_packages(&self) -> bool {
        self.packages.iter().any(|package| package.origin.is_some())
    }

    #[must_use]
    pub fn for_manifest(manifest: &ManifestData) -> Self {
        Self {
            manifest_fingerprint: semantic_manifest_fingerprint(manifest),
            packages: vec![LockedPackage {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                source: "root".into(),
                manifest_fingerprint: semantic_manifest_fingerprint(manifest),
                origin: None,
                commit: None,
                content_digest: None,
                // P3 freezes the single root. P4 replaces these aliases with
                // resolved package identities while retaining format v1.
                dependencies: manifest.dependencies.keys().cloned().collect(),
            }],
        }
    }

    #[must_use]
    pub fn for_packages(manifest: &ManifestData, mut packages: Vec<LockedPackage>) -> Self {
        packages.sort_by(|left, right| {
            (&left.source, &left.name, &left.version).cmp(&(
                &right.source,
                &right.name,
                &right.version,
            ))
        });
        let mut graph = semantic_manifest_fingerprint(manifest);
        for package in &packages {
            push_component(&mut graph, "package.source", &package.source);
            push_component(&mut graph, "package.name", &package.name);
            push_component(&mut graph, "package.version", &package.version);
            push_component(
                &mut graph,
                "package.manifest",
                &package.manifest_fingerprint,
            );
            push_component(
                &mut graph,
                "package.origin",
                package.origin.as_deref().unwrap_or(""),
            );
            push_component(
                &mut graph,
                "package.commit",
                package.commit.as_deref().unwrap_or(""),
            );
            push_component(
                &mut graph,
                "package.content",
                package.content_digest.as_deref().unwrap_or(""),
            );
            let mut dependencies = package.dependencies.clone();
            dependencies.sort();
            for dependency in dependencies {
                push_component(&mut graph, "package.dependency", &dependency);
            }
        }
        Self {
            manifest_fingerprint: format!(
                "{DIGEST_PREFIX}{}",
                blake3::hash(graph.as_bytes()).to_hex()
            ),
            packages,
        }
    }

    pub fn parse(path: &Path, text: &str) -> Result<Self, LockfileError> {
        let raw: RawLockfile = toml::from_str(text)
            .map_err(|error| LockfileError(format!("invalid {}: {error}", path.display())))?;
        if raw.version != LOCK_VERSION {
            return Err(LockfileError(format!(
                "unsupported lockfile version {}; expected {LOCK_VERSION}",
                raw.version
            )));
        }
        validate_digest(&raw.manifest_fingerprint)?;
        if raw.package.is_empty() {
            return Err(LockfileError("lockfile package graph is empty".into()));
        }
        let mut identities = BTreeSet::new();
        let mut packages = Vec::with_capacity(raw.package.len());
        for package in raw.package {
            if package.name.is_empty() || package.source.is_empty() {
                return Err(LockfileError(
                    "lockfile package name and source must be non-empty".into(),
                ));
            }
            semver::Version::parse(&package.version).map_err(|error| {
                LockfileError(format!(
                    "invalid locked version for `{}`: {error}",
                    package.name
                ))
            })?;
            validate_portable_source(&package.source)?;
            if !package.manifest_fingerprint.is_empty() {
                validate_digest(&package.manifest_fingerprint)?;
            }
            validate_package_provenance(
                &package.source,
                package.origin.as_deref(),
                package.commit.as_deref(),
                package.content_digest.as_deref(),
            )?;
            let identity = (package.source.clone(), package.name.clone());
            if !identities.insert(identity) {
                return Err(LockfileError(format!(
                    "duplicate locked package `{}` from `{}`",
                    package.name, package.source
                )));
            }
            let mut dependencies = package.dependencies;
            if dependencies.iter().any(|value| value.is_empty()) {
                return Err(LockfileError(
                    "empty dependency identity in lockfile".into(),
                ));
            }
            let original = dependencies.clone();
            dependencies.sort();
            dependencies.dedup();
            if dependencies != original {
                return Err(LockfileError(
                    "lockfile dependencies must be sorted and unique".into(),
                ));
            }
            packages.push(LockedPackage {
                name: package.name,
                version: package.version,
                source: package.source,
                manifest_fingerprint: package.manifest_fingerprint,
                origin: package.origin,
                commit: package.commit,
                content_digest: package.content_digest,
                dependencies,
            });
        }
        let original = packages.clone();
        packages.sort_by(|left, right| {
            (&left.source, &left.name, &left.version).cmp(&(
                &right.source,
                &right.name,
                &right.version,
            ))
        });
        if packages != original {
            return Err(LockfileError(
                "lockfile packages must use canonical identity order".into(),
            ));
        }
        Ok(Self {
            manifest_fingerprint: raw.manifest_fingerprint,
            packages,
        })
    }

    #[must_use]
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut output =
            format!("# This file is generated by Arandu. Do not edit.\nversion = {LOCK_VERSION}\n");
        push_string_field(
            &mut output,
            "manifest_fingerprint",
            &self.manifest_fingerprint,
        );
        let mut packages = self.packages.clone();
        packages.sort_by(|left, right| {
            (&left.source, &left.name, &left.version).cmp(&(
                &right.source,
                &right.name,
                &right.version,
            ))
        });
        for package in &packages {
            output.push_str("\n[[package]]\n");
            push_string_field(&mut output, "name", &package.name);
            push_string_field(&mut output, "version", &package.version);
            push_string_field(&mut output, "source", &package.source);
            push_string_field(
                &mut output,
                "manifest_fingerprint",
                &package.manifest_fingerprint,
            );
            if let Some(origin) = &package.origin {
                push_string_field(&mut output, "origin", origin);
            }
            if let Some(commit) = &package.commit {
                push_string_field(&mut output, "commit", commit);
            }
            if let Some(content_digest) = &package.content_digest {
                push_string_field(&mut output, "content_digest", content_digest);
            }
            output.push_str("dependencies = [");
            let mut dependencies = package.dependencies.clone();
            dependencies.sort();
            dependencies.dedup();
            for (index, dependency) in dependencies.iter().enumerate() {
                if index != 0 {
                    output.push_str(", ");
                }
                output.push_str(&toml_string(dependency));
            }
            output.push_str("]\n");
        }
        output.into_bytes()
    }
}

#[must_use]
pub fn semantic_manifest_fingerprint(manifest: &ManifestData) -> String {
    let mut canonical = String::new();
    push_component(&mut canonical, "schema", &manifest.schema.to_string());
    push_component(&mut canonical, "name", &manifest.name);
    push_component(&mut canonical, "version", &manifest.version);
    push_component(
        &mut canonical,
        "edition",
        match manifest.edition {
            ManifestEdition::Legacy => "legacy",
            ManifestEdition::V2026 => "2026",
        },
    );
    push_component(
        &mut canonical,
        "kind",
        match manifest.kind {
            PackageKind::Binary => "binary",
            PackageKind::Library => "library",
            PackageKind::Mixed => "mixed",
            PackageKind::Component => "component",
        },
    );
    push_component(
        &mut canonical,
        "toolchain",
        manifest.toolchain_requirement.as_deref().unwrap_or(""),
    );
    for (kind, target) in [
        ("bin", manifest.binary_target.as_ref()),
        ("lib", manifest.library_target.as_ref()),
        ("component", manifest.component_target.as_ref()),
    ] {
        if let Some(target) = target {
            push_component(&mut canonical, &format!("target.{kind}.name"), &target.name);
            push_component(&mut canonical, &format!("target.{kind}.root"), &target.root);
            for (public_name, source) in &target.exports {
                push_component(
                    &mut canonical,
                    &format!("target.{kind}.export.{public_name}"),
                    source,
                );
            }
        }
    }
    for (alias, dependency) in &manifest.dependencies {
        match dependency {
            crate::ManifestDependency::Path { path } => {
                push_component(&mut canonical, &format!("dependency.{alias}.path"), path);
            }
            crate::ManifestDependency::Git { origin, commit } => {
                push_component(&mut canonical, &format!("dependency.{alias}.git"), origin);
                push_component(&mut canonical, &format!("dependency.{alias}.rev"), commit);
            }
        }
    }
    for (name, values) in [
        ("network", &manifest.capabilities.network),
        ("filesystem_read", &manifest.capabilities.filesystem_read),
        ("filesystem_write", &manifest.capabilities.filesystem_write),
        ("environment", &manifest.capabilities.environment),
        ("process", &manifest.capabilities.process),
    ] {
        let mut values = values.iter().map(String::as_str).collect::<Vec<_>>();
        values.sort_unstable();
        for value in values {
            push_component(&mut canonical, &format!("capability.{name}"), value);
        }
    }
    push_component(
        &mut canonical,
        "capability.foreign",
        if manifest.capabilities.foreign {
            "true"
        } else {
            "false"
        },
    );
    push_component(
        &mut canonical,
        "policy.effects.deny_new_authority",
        if manifest.effect_policy.deny_new_authority {
            "true"
        } else {
            "false"
        },
    );
    push_component(
        &mut canonical,
        "policy.effects.warn_new_resources",
        if manifest.effect_policy.warn_new_resources {
            "true"
        } else {
            "false"
        },
    );
    let mut denied_effects = manifest
        .effect_policy
        .deny
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    denied_effects.sort_unstable();
    for effect in denied_effects {
        push_component(&mut canonical, "policy.effects.deny", effect);
    }
    if let Some(workspace) = &manifest.workspace {
        let mut members = workspace
            .members
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        members.sort_unstable();
        for member in members {
            push_component(&mut canonical, "workspace.member", member);
        }
    }
    format!(
        "{DIGEST_PREFIX}{}",
        blake3::hash(canonical.as_bytes()).to_hex()
    )
}

fn push_component(output: &mut String, key: &str, value: &str) {
    output.push_str(&key.len().to_string());
    output.push(':');
    output.push_str(key);
    output.push(':');
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
    output.push('\n');
}

fn validate_digest(value: &str) -> Result<(), LockfileError> {
    let Some(hex) = value.strip_prefix(DIGEST_PREFIX) else {
        return Err(LockfileError(
            "manifest fingerprint must use `blake3:<64 lowercase hex>`".into(),
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LockfileError(
            "manifest fingerprint must use `blake3:<64 lowercase hex>`".into(),
        ));
    }
    Ok(())
}

fn validate_portable_source(source: &str) -> Result<(), LockfileError> {
    let portable_path = source.strip_prefix("path+").is_some_and(|path| {
        !path.is_empty()
            && !path.contains('\\')
            && !path.starts_with('/')
            && !path
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    });
    let git = source.starts_with("git+");
    if source != "root" && !portable_path && !git {
        return Err(LockfileError(format!(
            "nonportable package source `{source}` in lockfile"
        )));
    }
    Ok(())
}

fn validate_package_provenance(
    source: &str,
    origin: Option<&str>,
    commit: Option<&str>,
    content_digest: Option<&str>,
) -> Result<(), LockfileError> {
    if source == "root" || source.starts_with("path+") {
        if origin.is_some() || commit.is_some() || content_digest.is_some() {
            return Err(LockfileError(
                "root and path packages must not contain remote provenance fields".into(),
            ));
        }
        return Ok(());
    }
    let (Some(origin), Some(commit), Some(content_digest)) = (origin, commit, content_digest)
    else {
        return Err(LockfileError(
            "Git packages require origin, commit and content_digest".into(),
        ));
    };
    let expected_source = format!("git+{origin}#{commit}");
    let valid_subpackage = source.strip_prefix(&expected_source).is_some_and(|suffix| {
        suffix.strip_prefix("/path+").is_some_and(|path| {
            !path.is_empty()
                && !path.contains('\\')
                && !path.starts_with('/')
                && !path
                    .split('/')
                    .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        })
    });
    if source != expected_source && !valid_subpackage {
        return Err(LockfileError(
            "Git package source must match its origin/commit and optional portable path subpackage"
                .into(),
        ));
    }
    crate::manifest::validate_git_dependency_identity(origin, commit).map_err(LockfileError)?;
    validate_sha256_digest(content_digest)
}

fn validate_sha256_digest(value: &str) -> Result<(), LockfileError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(LockfileError(
            "content digest must use `sha256:<64 lowercase hex>`".into(),
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LockfileError(
            "content digest must use `sha256:<64 lowercase hex>`".into(),
        ));
    }
    Ok(())
}

fn push_string_field(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(" = ");
    output.push_str(&toml_string(value));
    output.push('\n');
}

fn toml_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04X}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_manifest_str;

    fn manifest(text: &str) -> ManifestData {
        parse_manifest_str(Path::new("arandu.toml"), text).unwrap()
    }

    #[test]
    fn canonical_lock_is_byte_stable_and_lf_only() {
        let data = manifest(
            "schema=1\n[package]\nname='app'\nversion='1.0.0'\nedition='2026'\n[targets.bin]\nname='app'\nroot='src/main.aru'\n",
        );
        let lock = Lockfile::for_manifest(&data);
        let bytes = lock.to_canonical_bytes();
        assert!(!bytes.contains(&b'\r'));
        assert_eq!(
            Lockfile::parse(
                Path::new("arandu.lock"),
                std::str::from_utf8(&bytes).unwrap()
            )
            .unwrap(),
            lock
        );
    }

    #[test]
    fn semantic_fingerprint_ignores_toml_layout_but_not_resolution_inputs() {
        let compact = manifest("name='app'\nversion='1.0.0'\nentry='src/main.aru'\n");
        let spaced =
            manifest("# comment\nname = 'app'\nversion = '1.0.0'\nentry = 'src/main.aru'\n");
        assert_eq!(
            semantic_manifest_fingerprint(&compact),
            semantic_manifest_fingerprint(&spaced)
        );
        let changed = manifest("name='app'\nversion='1.0.1'\nentry='src/main.aru'\n");
        assert_ne!(
            semantic_manifest_fingerprint(&compact),
            semantic_manifest_fingerprint(&changed)
        );
    }

    #[test]
    fn semantic_fingerprint_sorts_sets_and_tracks_authority_policy() {
        let first = manifest(
            "schema=1\n[package]\nname='app'\nversion='1.0.0'\nedition='2026'\n[targets.bin]\nname='app'\nroot='src/main.aru'\n[capabilities]\nnetwork=['api.example.com', 'cdn.example.com']\nprocess=['git', 'cc']\n[policy.effects]\ndeny=['Network', 'Process']\n[workspace]\nmembers=['packages/z', 'packages/a']\n",
        );
        let reordered = manifest(
            "schema=1\n[package]\nname='app'\nversion='1.0.0'\nedition='2026'\n[workspace]\nmembers=['packages/a', 'packages/z']\n[targets.bin]\nroot='src/main.aru'\nname='app'\n[policy.effects]\ndeny=['Process', 'Network']\n[capabilities]\nprocess=['cc', 'git']\nnetwork=['cdn.example.com', 'api.example.com']\n",
        );
        assert_eq!(
            semantic_manifest_fingerprint(&first),
            semantic_manifest_fingerprint(&reordered),
            "set and TOML declaration order must not create lockfile churn"
        );

        let mut broader_authority = reordered;
        broader_authority.capabilities.foreign = true;
        assert_ne!(
            semantic_manifest_fingerprint(&first),
            semantic_manifest_fingerprint(&broader_authority),
            "an authority-policy change must alter the semantic identity"
        );
    }

    #[test]
    fn canonical_lock_sorts_package_and_edge_input_order() {
        let data = manifest(
            "schema=1\n[package]\nname='app'\nversion='1.0.0'\nedition='2026'\n[targets.bin]\nname='app'\nroot='src/main.aru'\n",
        );
        let digest = semantic_manifest_fingerprint(&data);
        let package = |name: &str, source: &str, dependencies: Vec<&str>| LockedPackage {
            name: name.into(),
            version: "1.0.0".into(),
            source: source.into(),
            manifest_fingerprint: digest.clone(),
            origin: None,
            commit: None,
            content_digest: None,
            dependencies: dependencies.into_iter().map(str::to_string).collect(),
        };
        let first = Lockfile::for_packages(
            &data,
            vec![
                package("z", "path+packages/z", vec!["b=root", "a=root"]),
                package("app", "root", vec!["z=path+packages/z"]),
            ],
        );
        let second = Lockfile::for_packages(
            &data,
            vec![
                package("app", "root", vec!["z=path+packages/z"]),
                package("z", "path+packages/z", vec!["a=root", "b=root"]),
            ],
        );
        assert_eq!(first.manifest_fingerprint, second.manifest_fingerprint);
        assert_eq!(first.to_canonical_bytes(), second.to_canonical_bytes());
    }

    #[test]
    fn parser_rejects_unknown_corrupt_duplicate_and_nonportable_data() {
        let digest = format!("blake3:{}", "0".repeat(64));
        let base = format!("version=2\nmanifest_fingerprint='{digest}'\n[[package]]\nname='a'\nversion='1.0.0'\nsource='root'\ndependencies=[]\n");
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &(base.clone() + "surprise=true\n")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &base.replace(&digest, "blake3:nope")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &base.replace("source='root'", "source='C:\\\\repo'")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &(base.clone() + &base[base.find("[[package]]").unwrap()..])
        )
        .is_err());
    }

    #[test]
    fn git_lock_provenance_is_explicit_and_tamper_evident() {
        let manifest_digest = format!("blake3:{}", "0".repeat(64));
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let origin = "https://github.com/example/math.git";
        let content = format!("sha256:{}", "1".repeat(64));
        let source = format!("git+{origin}#{commit}");
        let text = format!(
            "version=2\nmanifest_fingerprint='{manifest_digest}'\n[[package]]\nname='math'\nversion='1.0.0'\nsource='{source}'\nmanifest_fingerprint='{manifest_digest}'\norigin='{origin}'\ncommit='{commit}'\ncontent_digest='{content}'\ndependencies=[]\n"
        );
        let lock = Lockfile::parse(Path::new("arandu.lock"), &text).unwrap();
        assert_eq!(lock.packages[0].origin.as_deref(), Some(origin));
        assert_eq!(lock.packages[0].commit.as_deref(), Some(commit));
        assert_eq!(
            lock.packages[0].content_digest.as_deref(),
            Some(content.as_str())
        );
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &text.replace("content_digest='sha256:", "content_digest='blake3:")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &text.replace(
                &format!("commit='{commit}'"),
                "commit='1123456789abcdef0123456789abcdef01234567'"
            )
        )
        .is_err());
    }

    #[test]
    fn git_origin_substitution_is_rejected_even_with_a_valid_commit() {
        let digest = format!("blake3:{}", "0".repeat(64));
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let source_origin = "https://github.com/example/math.git";
        let declared_origin = "https://evil.example/math.git";
        let text = format!(
            "version=2\nmanifest_fingerprint='{digest}'\n[[package]]\nname='math'\nversion='1.0.0'\nsource='git+{source_origin}#{commit}'\nmanifest_fingerprint='{digest}'\norigin='{declared_origin}'\ncommit='{commit}'\ncontent_digest='sha256:{}'\ndependencies=[]\n",
            "1".repeat(64)
        );
        assert!(Lockfile::parse(Path::new("arandu.lock"), &text).is_err());
    }

    #[test]
    fn graph_diff_exposes_version_and_digest_rollbacks() {
        let data = manifest("name='app'\nversion='1.0.0'\nentry='src/main.aru'\n");
        let digest = semantic_manifest_fingerprint(&data);
        let package = |version: &str, content_digest: &str| LockedPackage {
            name: "dep".into(),
            version: version.into(),
            source: "git+https://example.com/dep.git#0123456789abcdef0123456789abcdef01234567"
                .into(),
            manifest_fingerprint: digest.clone(),
            origin: Some("https://example.com/dep.git".into()),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            content_digest: Some(content_digest.into()),
            dependencies: vec![],
        };
        let old = Lockfile::for_packages(
            &data,
            vec![package(
                "2.0.0",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )],
        );
        let rollback = Lockfile::for_packages(
            &data,
            vec![package(
                "1.0.0",
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )],
        );
        let diff = old.graph_diff(&rollback).join("\n");
        assert!(diff.contains("version=1.0.0"));
        assert!(diff.contains("digest=sha256:bbbb"));
    }
}
