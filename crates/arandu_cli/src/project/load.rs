//! Project graph discovery, manifest validation, and Salsa context initialization.

use std::fs;
use std::path::{Path, PathBuf};

use arandu_query::{
    DirectoryListing, LocalPackageGraph, MANIFEST_FILENAME, ManifestError, ManifestSpelling,
    ModuleRoots, ProjectManifest, StdlibResolveOpts, StdlibRoot, ensure_toolchain_compatible,
    register_manifest, resolve_stdlib_root, scan_aru_entries,
};

use super::lock::synchronize_lockfile;
use super::module_map::install_package_module_map;
use crate::manifest_io::{find_manifest, load_manifest};

/// CLI version string (mirrors package version).
pub const ARANDU_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Canonical root target kind used in stable tooling identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Bin,
    Lib,
    Component,
}

impl TargetKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bin => "bin",
            Self::Lib => "lib",
            Self::Component => "component",
        }
    }
}

impl std::fmt::Display for TargetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolved package context for project-mode commands.
pub struct ProjectContext {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    /// Live Salsa input — kept so dependents can re-query fingerprint/entry.
    pub manifest: ProjectManifest,
    pub entry_path: PathBuf,
    /// Resolved stdlib root (cascade); available for doctor/logs.
    pub stdlib: StdlibRoot,
    /// Platform-native, verified global package cache.
    pub cache: arandu_query::CacheLayout,
    pub lockfile: arandu_query::Lockfile,
    pub name: String,
    pub version: String,
    pub entry_rel: String,
    /// Canonical root target kind used in stable tooling identities.
    pub target_kind: TargetKind,
    /// Deterministic filesystem closure consumed by native compilation.
    pub build_inputs: Vec<crate::incremental::IncrementalInput>,
}

/// Shared flags for project / doctor commands.
#[derive(Debug, Clone, Default)]
pub struct ProjectFlags {
    pub stdlib_path: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub release: bool,
    pub verbose: bool,
    pub locked: bool,
    pub offline: bool,
    /// Explicit authority to publish a changed graph containing remote code.
    pub accept_lock: bool,
    pub color: crate::args::ColorChoice,
    pub target: Option<String>,
}

/// Parse `--stdlib-path=…` / `--stdlib-path …` / `--release` / `-v` from leftover args.
pub fn parse_project_flags(args: &[String]) -> Result<(ProjectFlags, Vec<String>), String> {
    let mut flags = ProjectFlags::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--stdlib-path=") {
            flags.stdlib_path = Some(PathBuf::from(v));
        } else if a == "--stdlib-path" {
            i += 1;
            if i < args.len() {
                flags.stdlib_path = Some(PathBuf::from(&args[i]));
            } else {
                return Err("--stdlib-path requires a directory argument".into());
            }
        } else if let Some(v) = a.strip_prefix("--cache-dir=") {
            if v.is_empty() {
                return Err("--cache-dir requires a directory argument".into());
            }
            flags.cache_dir = Some(PathBuf::from(v));
        } else if a == "--cache-dir" {
            i += 1;
            if i < args.len() {
                flags.cache_dir = Some(PathBuf::from(&args[i]));
            } else {
                return Err("--cache-dir requires a directory argument".into());
            }
        } else if a == "--release" {
            flags.release = true;
        } else if a == "-v" || a == "--verbose" {
            flags.verbose = true;
        } else if a == "--locked" {
            flags.locked = true;
        } else if a == "--offline" {
            flags.offline = true;
        } else if a == "--frozen" {
            flags.locked = true;
            flags.offline = true;
        } else if a == "--accept" {
            flags.accept_lock = true;
        } else if a == "--no-color" {
            flags.color = crate::args::ColorChoice::Never;
        } else if a == "--color=always" {
            flags.color = crate::args::ColorChoice::Always;
        } else if a == "--color=never" {
            flags.color = crate::args::ColorChoice::Never;
        } else if a == "--color=auto" {
            flags.color = crate::args::ColorChoice::Auto;
        } else if a == "--color" {
            i += 1;
            if i < args.len() {
                match args[i].as_str() {
                    "always" => flags.color = crate::args::ColorChoice::Always,
                    "never" => flags.color = crate::args::ColorChoice::Never,
                    "auto" => flags.color = crate::args::ColorChoice::Auto,
                    other => {
                        return Err(format!(
                            "unknown --color option: '{other}' (use auto|always|never)"
                        ));
                    }
                }
            } else {
                return Err("--color requires an argument (use auto|always|never)".into());
            }
        } else if let Some(v) = a.strip_prefix("--color=") {
            return Err(format!(
                "unknown --color option: '{v}' (use auto|always|never)"
            ));
        } else if let Some(v) = a.strip_prefix("--target=") {
            if v.is_empty() {
                return Err("--target requires a target triple argument".into());
            }
            flags.target = Some(v.to_string());
        } else if a == "--target" {
            i += 1;
            if i < args.len() {
                flags.target = Some(args[i].clone());
            } else {
                return Err("--target requires a target triple argument".into());
            }
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    Ok((flags, rest))
}

/// Load project from `start` (file, dir, or cwd) and register Salsa inputs.
pub fn load_project(
    db: &mut arandu_query::DatabaseImpl,
    start: &Path,
    flags: &ProjectFlags,
) -> Result<ProjectContext, String> {
    let discovery = find_manifest(start)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "no {MANIFEST_FILENAME} found from {} — run `arandu_cli new <name>` or pass a path to a package",
                start.display()
            )
        })?;
    if discovery.spelling == ManifestSpelling::Legacy {
        eprintln!(
            "warning: `{}` is deprecated; rename it to `{MANIFEST_FILENAME}`",
            arandu_query::LEGACY_MANIFEST_FILENAME
        );
    }
    let manifest_path = discovery.path;

    let (data, hash, _bytes) =
        load_manifest(&manifest_path).map_err(|e: ManifestError| e.to_string())?;
    ensure_toolchain_compatible(&manifest_path, &data, ARANDU_VERSION)
        .map_err(|error| error.to_string())?;

    let root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let canonical_root = fs::canonicalize(&root).map_err(|error| {
        format!(
            "cannot canonicalize project root {}: {error}",
            root.display()
        )
    })?;

    let cache = arandu_package::cache::resolve_cache_layout(flags.cache_dir.as_deref())?;
    let remote_packages = arandu_package::resolver::materialize_remote_graph(
        &canonical_root,
        &manifest_path,
        &data,
        &cache,
        arandu_package::resolver::ResolutionPolicy {
            locked: flags.locked,
            offline: flags.offline,
        },
    )?;
    let package_graph = LocalPackageGraph::discover_materialized(
        &canonical_root,
        &manifest_path,
        &data,
        &remote_packages,
    )?;
    let lockfile = package_graph.lockfile(&data);
    synchronize_lockfile(&canonical_root, lockfile.clone(), flags)?;

    let entry_candidate = canonical_root.join(&data.entry);
    let entry_path = fs::canonicalize(&entry_candidate).map_err(|error| {
        format!(
            "entry `{}` from {} does not exist (resolved to {}): {error}",
            data.entry,
            manifest_path.display(),
            entry_candidate.display()
        )
    })?;
    if !entry_path.starts_with(&canonical_root) || !entry_path.is_file() {
        return Err(format!(
            "entry `{}` from {} escapes the project root or is not a file (resolved to {})",
            data.entry,
            manifest_path.display(),
            entry_path.display(),
        ));
    }

    let stdlib = resolve_stdlib_root(StdlibResolveOpts {
        explicit: flags.stdlib_path.clone(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    db.set_stdlib_root(stdlib.path.clone());
    crate::pipeline::register_stdlib_sources(db, &stdlib.path);
    let build_inputs = collect_build_inputs(&package_graph, &stdlib.path);

    // Package source root = directory containing the entry file (usually `src/`).
    let package_src = entry_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.clone());
    let entries = scan_aru_entries(&package_src);
    for relative in &entries {
        let path = package_src.join(relative);
        let key = path.to_string_lossy().into_owned();
        if db.source_file_by_path(&key).is_none() {
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("cannot read module {}: {error}", path.display()))?;
            db.new_file(key, text);
        }
    }
    let listing = DirectoryListing::new(
        db,
        std::sync::Arc::new(package_src.clone()),
        std::sync::Arc::new(entries),
    );
    let roots = ModuleRoots::new(
        db,
        data.name.clone(),
        std::sync::Arc::new(package_src),
        Some(std::sync::Arc::new(stdlib.path.clone())),
        listing,
    );
    db.set_module_roots(roots);
    install_package_module_map(db, &package_graph, &entry_path)?;

    let name = data.name.clone();
    let version = data.version.clone();
    let entry_rel = data.entry.clone();
    let target_kind = if data.kind == arandu_query::PackageKind::Component
        || data.target_type.as_deref() == Some("component")
        || data.component_target.is_some()
    {
        TargetKind::Component
    } else if data.binary_target.is_some() {
        TargetKind::Bin
    } else {
        TargetKind::Lib
    };

    let manifest = register_manifest(db, manifest_path.clone(), data, hash);
    // Touch tracked fingerprint so the input is live in the Salsa graph.
    let _fp = arandu_query::manifest_fingerprint(db, manifest);
    db.set_project_manifest(manifest);

    Ok(ProjectContext {
        root: canonical_root,
        manifest_path,
        manifest,
        entry_path,
        stdlib,
        cache,
        lockfile,
        name,
        version,
        entry_rel,
        target_kind,
        build_inputs,
    })
}

fn collect_build_inputs(
    graph: &LocalPackageGraph,
    stdlib_root: &Path,
) -> Vec<crate::incremental::IncrementalInput> {
    let mut inputs = std::collections::BTreeMap::new();
    for package in &graph.packages {
        let manifest = [MANIFEST_FILENAME, arandu_query::LEGACY_MANIFEST_FILENAME]
            .into_iter()
            .map(|name| package.root.join(name))
            .find(|path| path.is_file());
        if let Some(path) = manifest {
            let key = format!("package/{}/manifest", package.source);
            inputs.insert(
                key.clone(),
                crate::incremental::IncrementalInput {
                    key,
                    path,
                    semantic_source: false,
                },
            );
        }
        for relative in scan_aru_entries(&package.root) {
            let key = format!("package/{}/source/{relative}", package.source);
            inputs.insert(
                key.clone(),
                crate::incremental::IncrementalInput {
                    key,
                    path: package.root.join(relative),
                    semantic_source: true,
                },
            );
        }
    }
    for relative in scan_aru_entries(stdlib_root) {
        let key = format!("stdlib/{relative}");
        inputs.insert(
            key.clone(),
            crate::incremental::IncrementalInput {
                key,
                path: stdlib_root.join(relative),
                semantic_source: true,
            },
        );
    }
    inputs.into_values().collect()
}

/// Backend selection convention (roadmap 4.1 dual backend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Fast host path — Cranelift JIT for `run`, AOT object/link for `build`.
    CraneliftDev,
    /// Speed-oriented Cranelift AOT path used by `build --release`.
    CraneliftRelease,
}

impl BackendChoice {
    #[must_use]
    pub fn from_release_flag(release: bool) -> Self {
        if release {
            BackendChoice::CraneliftRelease
        } else {
            BackendChoice::CraneliftDev
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            BackendChoice::CraneliftDev => "cranelift-dev",
            BackendChoice::CraneliftRelease => "cranelift-release",
        }
    }
}
