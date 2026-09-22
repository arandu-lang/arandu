//! Background workspace discovery and file registration.
//!
//! Runs after the initialize handshake (AGENTS.md): the main thread registers
//! discovered files into [`ServerState`] while local resources stay available.
//! Discovery is bounded by the worker pool backlog; it never blocks `initialize`.

use crate::pool::{Priority, WorkerPool};
use crate::state::{discover_aru_files, ServerState};
use crate::uri_util::uri_from_path;
use arandu_package::{find_manifest, load_manifest};
use arandu_query::{
    ensure_toolchain_compatible, resolve_stdlib_root, scan_aru_entries, LocalPackageGraph,
    ManifestData, PackageModulePlan, StdlibResolveOpts,
};
use crossbeam_channel::{bounded, Receiver};
use std::path::PathBuf;

pub(crate) struct WorkspaceFile {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
}

pub(crate) struct WorkspaceStdlib {
    pub(crate) root: PathBuf,
    pub(crate) files: Vec<WorkspaceFile>,
}

pub(crate) struct WorkspaceProject {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_data: ManifestData,
    pub(crate) manifest_hash: String,
    pub(crate) package_src: PathBuf,
    pub(crate) entries: Vec<String>,
    pub(crate) stdlib_root: Option<PathBuf>,
    pub(crate) module_plan: Option<PackageModulePlan>,
    pub(crate) module_files: Vec<WorkspaceFile>,
}

pub(crate) enum WorkspaceEvent {
    Stdlib(WorkspaceStdlib),
    Project(Box<WorkspaceProject>),
    NoManifest,
    File(WorkspaceFile),
    Error(String),
    Done,
}

pub(crate) fn spawn_workspace_discovery(
    pool: &WorkerPool,
    roots: Vec<PathBuf>,
) -> Receiver<WorkspaceEvent> {
    const DISCOVERY_BACKLOG: usize = 8;
    let (tx, rx) = bounded(DISCOVERY_BACKLOG);
    let _ = pool.spawn(Priority::Background, None, move |cancellation| {
        match discover_stdlib() {
            Ok(stdlib) => {
                if tx.send(WorkspaceEvent::Stdlib(stdlib)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(WorkspaceEvent::Error(error));
            }
        }
        if cancellation.is_cancelled() {
            let _ = tx.send(WorkspaceEvent::Done);
            return;
        }
        // The compiler DB currently owns one ModuleRoots input. Select the
        // first workspace package deterministically; multi-root ownership is a
        // separate protocol capability, not an order-dependent overwrite.
        match discover_workspace_project(&roots) {
            Ok(Some(project)) => {
                if tx.send(WorkspaceEvent::Project(Box::new(project))).is_err() {
                    return;
                }
            }
            Ok(None) => {
                if !roots.is_empty() && tx.send(WorkspaceEvent::NoManifest).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(WorkspaceEvent::Error(error));
                let _ = tx.send(WorkspaceEvent::Done);
                return;
            }
        }
        for (path, text) in discover_aru_files(&roots) {
            if cancellation.is_cancelled() {
                break;
            }
            if tx
                .send(WorkspaceEvent::File(WorkspaceFile { path, text }))
                .is_err()
            {
                break;
            }
        }
        let _ = tx.send(WorkspaceEvent::Done);
    });
    rx
}

fn discover_stdlib() -> Result<WorkspaceStdlib, String> {
    let stdlib =
        resolve_stdlib_root(StdlibResolveOpts::default()).map_err(|error| error.to_string())?;
    let root = stdlib.path;
    let mut files = Vec::new();
    for relative in scan_aru_entries(&root) {
        let path = root.join(relative);
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read stdlib module {}: {error}", path.display()))?;
        files.push(WorkspaceFile { path, text });
    }
    Ok(WorkspaceStdlib { root, files })
}

/// Rebuild the active package graph after a manifest notification. Discovery
/// stays on a coalesced background job; only the finished immutable plan is
/// committed by the event-loop writer thread.
pub(crate) fn spawn_package_reload(
    pool: &WorkerPool,
    tx: crossbeam_channel::Sender<crate::dispatcher::JobResult>,
    manifest_path: PathBuf,
) {
    let _ = pool.spawn(
        Priority::Background,
        Some(crate::pool::JobKey::WorkspaceReload),
        move |cancellation| {
            if cancellation.is_cancelled() {
                return;
            }
            let result = discover_workspace_project(std::slice::from_ref(&manifest_path))
                .and_then(|project| {
                    project.ok_or_else(|| {
                        format!("no package manifest found from {}", manifest_path.display())
                    })
                })
                .map(Box::new);
            if !cancellation.is_cancelled() {
                let _ = tx.send(crate::dispatcher::JobResult::WorkspaceReload(result));
            }
        },
    );
}

pub(crate) fn discover_workspace_project(
    roots: &[PathBuf],
) -> Result<Option<WorkspaceProject>, String> {
    discover_workspace_project_with_cache(roots, None)
}

fn discover_workspace_project_with_cache(
    roots: &[PathBuf],
    cache_override: Option<&std::path::Path>,
) -> Result<Option<WorkspaceProject>, String> {
    for root in roots {
        let Some(discovery) = find_manifest(root).map_err(|error| error.to_string())? else {
            continue;
        };
        let manifest_path = discovery.path;
        let Ok((manifest_data, manifest_hash, _)) = load_manifest(&manifest_path) else {
            continue;
        };
        ensure_toolchain_compatible(&manifest_path, &manifest_data, env!("CARGO_PKG_VERSION"))
            .map_err(|error| error.to_string())?;
        let package_root = manifest_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        let entry_path = package_root.join(&manifest_data.entry);
        let package_src = entry_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| package_root.clone());
        let entries = scan_aru_entries(&package_src);
        let cache = arandu_package::cache::resolve_cache_layout(cache_override)?;
        let remote_packages = arandu_package::resolver::materialize_remote_graph(
            &package_root,
            &manifest_path,
            &manifest_data,
            &cache,
            arandu_package::resolver::ResolutionPolicy {
                locked: true,
                offline: true,
            },
        )?;
        let graph = LocalPackageGraph::discover_materialized(
            &package_root,
            &manifest_path,
            &manifest_data,
            &remote_packages,
        )?;
        let module_plan = graph.module_plan(&entry_path)?;
        let mut module_files = Vec::new();
        if let Some(plan) = module_plan.as_ref() {
            let paths = plan
                .bindings
                .iter()
                .map(|binding| binding.physical.clone())
                .collect::<std::collections::BTreeSet<_>>();
            for path in paths {
                let text = std::fs::read_to_string(&path)
                    .map_err(|error| format!("cannot read module {}: {error}", path.display()))?;
                module_files.push(WorkspaceFile { path, text });
            }
        }
        let stdlib_root = resolve_stdlib_root(StdlibResolveOpts::default())
            .ok()
            .map(|stdlib| stdlib.path);
        return Ok(Some(WorkspaceProject {
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src,
            entries,
            stdlib_root,
            module_plan,
            module_files,
        }));
    }
    Ok(None)
}

/// Registers a discovered file unless an editor buffer already owns the URI —
/// never replace a newer editor buffer with a stale disk snapshot.
pub(crate) fn register_workspace_file(state: &mut ServerState, file: WorkspaceFile) {
    let Some(uri) = uri_from_path(&file.path) else {
        return;
    };
    if !state.by_uri.contains_key(uri.as_str()) {
        state.open_or_commit(&uri, file.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_package::cache::{CacheStore, TreeLimits};
    use arandu_query::{CacheLayout, LockedPackage, Lockfile, LOCK_FILENAME};

    #[test]
    fn workspace_project_discovery_loads_manifest_and_listing() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-project-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).expect("create fixture");
        std::fs::write(
            root.join("Arandu.toml"),
            "name = \"editor_gold\"\nversion = \"0.1.0\"\nentry = \"src/main.aru\"\n",
        )
        .expect("write manifest");
        std::fs::write(root.join("src/main.aru"), "func main() {}\n").expect("write entry");

        let project = discover_workspace_project(std::slice::from_ref(&root))
            .expect("workspace discovery should not fail")
            .expect("discover package metadata");
        assert_eq!(project.manifest_data.name, "editor_gold");
        assert_eq!(
            project.package_src,
            std::fs::canonicalize(root.join("src")).expect("canonical fixture source")
        );
        assert_eq!(project.entries, vec!["main.aru"]);

        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn workspace_remote_dependency_uses_only_revalidated_locked_cache() {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(format!("arandu-lsp-remote-{suffix}"));
        let cache_root = std::env::temp_dir().join(format!("arandu-lsp-cache-{suffix}"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Arandu.toml"),
            "schema=1\n[package]\nname='editor_remote'\nversion='0.1.0'\nedition='2026'\n[targets.bin]\nname='editor_remote'\nroot='src/main.aru'\n[dependencies]\nmath={git='https://example.com/math.git',rev='0123456789abcdef0123456789abcdef01234567'}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/main.aru"), "func main(): int { return 0 }\n").unwrap();

        let layout = CacheLayout::new(cache_root.clone()).unwrap();
        std::fs::create_dir_all(layout.staging()).unwrap();
        let staging = layout.staging().join("math");
        std::fs::create_dir_all(staging.join("src")).unwrap();
        std::fs::write(
            staging.join("arandu.toml"),
            "schema=1\n[package]\nname='math'\nversion='1.0.0'\nedition='2026'\n[targets.lib]\nname='math'\nroot='src/lib.aru'\n[targets.lib.exports]\n'.'='src/lib.aru'\n",
        )
        .unwrap();
        std::fs::write(
            staging.join("src/lib.aru"),
            "public func answer(): int { return 42 }\n",
        )
        .unwrap();
        let (_, verified, _) = CacheStore::new(layout)
            .publish_tree(&staging, TreeLimits::default())
            .unwrap();
        let source = "git+https://example.com/math.git#0123456789abcdef0123456789abcdef01234567";
        let lock = Lockfile {
            manifest_fingerprint:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            packages: vec![LockedPackage {
                name: "math".into(),
                version: "1.0.0".into(),
                source: source.into(),
                manifest_fingerprint: String::new(),
                origin: Some("https://example.com/math.git".into()),
                commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
                content_digest: Some(verified.digest.to_string()),
                dependencies: Vec::new(),
            }],
        };
        std::fs::write(root.join(LOCK_FILENAME), lock.to_canonical_bytes()).unwrap();

        let project =
            discover_workspace_project_with_cache(std::slice::from_ref(&root), Some(&cache_root))
                .expect("offline cached remote discovery")
                .expect("workspace project");
        assert!(project.module_plan.is_some());
        assert!(project
            .module_files
            .iter()
            .any(|file| file.path.ends_with("src/lib.aru")));

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(cache_root).unwrap();
    }
}
