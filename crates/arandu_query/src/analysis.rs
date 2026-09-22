//! Analysis host / snapshot handles for IDE-facing work (A10 gold + F2).
//!
//! Stale-safety of analysis is **revision of snapshot**, not generational
//! `SymbolId`. `DocumentId` (see [`crate::doc_store`]) remains the generational
//! buffer handle; dense compiler IDs stay dense.

use crate::db::{DatabaseImpl, SourceFile};
use crate::manifest::{register_manifest, ManifestData, ProjectManifest};
use crate::vfs::{DirectoryListing, ModuleRoots};
use crate::{ModuleBinding, PackageModuleMap};
use arandu_middle::SymbolId;
use arandu_middle::{PackageId, TargetId};
use salsa::Setter;
use std::path::PathBuf;
use std::sync::Arc;

/// Monotonic analysis generation advanced on every input commit (`set_text` /
/// register). IDE handles that embed a revision must match the live host.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnalysisRevision(u64);

pub struct ResolvedPackageMap {
    pub current_package: PackageId,
    pub current_target: TargetId,
    pub bindings: Vec<(String, ModuleBinding)>,
}

pub struct PackageConfiguration {
    pub manifest_path: PathBuf,
    pub manifest_data: ManifestData,
    pub manifest_hash: String,
    pub package_src: PathBuf,
    pub entries: Vec<String>,
    pub stdlib_root: Option<PathBuf>,
    pub package_map: Option<ResolvedPackageMap>,
}

impl AnalysisRevision {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Frozen view of the database for a worker: cheap `Clone` of Salsa storage +
/// the host revision at capture time.
#[derive(Clone)]
pub struct AnalysisSnapshot {
    pub revision: AnalysisRevision,
    pub db: DatabaseImpl,
}

impl AnalysisSnapshot {
    #[must_use]
    pub fn capture(db: &DatabaseImpl, revision: AnalysisRevision) -> Self {
        Self {
            revision,
            db: db.clone(),
        }
    }
}

/// Symbol handle valid only for a specific [`AnalysisRevision`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct LspSymbolId {
    pub symbol: SymbolId,
    pub revision: AnalysisRevision,
}

impl LspSymbolId {
    #[must_use]
    pub fn new(symbol: SymbolId, revision: AnalysisRevision) -> Self {
        Self { symbol, revision }
    }

    /// Returns the dense symbol only if this handle matches `snap.revision`.
    #[must_use]
    pub fn resolve(self, snap: &AnalysisSnapshot) -> Option<SymbolId> {
        if self.revision == snap.revision {
            Some(self.symbol)
        } else {
            None
        }
    }
}

/// Single-threaded owner of the Salsa DB + analysis revision counter.
///
/// Writes go through this host so revision advances in lock-step with inputs.
/// Workers receive only [`AnalysisSnapshot`] (read-only clone).
///
/// **Deadlock rule:** never hold an [`AnalysisSnapshot`] (or any `DatabaseImpl`
/// clone) on the same thread that calls [`Self::set_text`] / other writes.
/// Salsa waits for all storage clones to drop before mutating; a live snapshot
/// on the writer thread blocks forever.
pub struct AnalysisHost {
    db: DatabaseImpl,
    revision: AnalysisRevision,
}

impl Default for AnalysisHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisHost {
    #[must_use]
    pub fn new() -> Self {
        Self {
            db: DatabaseImpl::new(),
            revision: AnalysisRevision::new(0),
        }
    }

    #[must_use]
    pub fn db(&self) -> &DatabaseImpl {
        &self.db
    }

    #[must_use]
    pub fn db_mut(&mut self) -> &mut DatabaseImpl {
        &mut self.db
    }

    #[must_use]
    pub fn revision(&self) -> AnalysisRevision {
        self.revision
    }

    /// O(1) snapshot for a worker (Salsa `Storage` clone shares memos).
    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        AnalysisSnapshot::capture(&self.db, self.revision)
    }

    /// Commit new text for an input; advances revision.
    pub fn set_text(&mut self, source: SourceFile, text: impl Into<Arc<str>>) {
        source.set_text(&mut self.db).to(text.into());
        self.revision = self.revision.next();
    }

    /// Register a path without necessarily changing text revision policy:
    /// new files still bump revision so open/import order is observable.
    pub fn register_source_file(&mut self, path: String, file: SourceFile) {
        self.db.register_source_file(path, file);
        self.revision = self.revision.next();
    }

    /// Remove a source registration and advance the observable analysis revision.
    /// A later registration for the same path must receive a fresh `FileId`.
    pub fn unregister_source_file(&mut self, path: &str) {
        self.db.unregister_source_file(path);
        self.revision = self.revision.next();
    }

    /// Install package metadata discovered outside the query graph.
    ///
    /// The LSP calls this only on its writer thread after `initialize`; workers
    /// observe the resulting Salsa inputs through later snapshots.
    pub fn configure_package(
        &mut self,
        manifest_path: PathBuf,
        manifest_data: ManifestData,
        manifest_hash: String,
        package_src: PathBuf,
        entries: Vec<String>,
        stdlib_root: Option<PathBuf>,
    ) -> (ProjectManifest, DirectoryListing, ModuleRoots) {
        self.configure_package_with_map(PackageConfiguration {
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src,
            entries,
            stdlib_root,
            package_map: None,
        })
    }

    /// Install package roots and an optional resolved namespace as one writer
    /// transaction and one observable analysis revision.
    pub fn configure_package_with_map(
        &mut self,
        configuration: PackageConfiguration,
    ) -> (ProjectManifest, DirectoryListing, ModuleRoots) {
        let PackageConfiguration {
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src,
            entries,
            stdlib_root,
            package_map,
        } = configuration;
        let (listing, roots) = if let Some(roots) = self.db.module_roots() {
            let listing = *roots.package_listing(&self.db);
            listing
                .set_dir(&mut self.db)
                .to(Arc::new(package_src.clone()));
            listing.set_entries(&mut self.db).to(Arc::new(entries));
            roots
                .set_package_name(&mut self.db)
                .to(manifest_data.name.clone());
            roots
                .set_package_src(&mut self.db)
                .to(Arc::new(package_src));
            roots
                .set_stdlib_root(&mut self.db)
                .to(stdlib_root.clone().map(Arc::new));
            (listing, roots)
        } else {
            let listing =
                DirectoryListing::new(&self.db, Arc::new(package_src.clone()), Arc::new(entries));
            let roots = ModuleRoots::new(
                &self.db,
                manifest_data.name.clone(),
                Arc::new(package_src),
                stdlib_root.clone().map(Arc::new),
                listing,
            );
            (listing, roots)
        };
        let manifest = if let Some(manifest) = self.db.project_manifest() {
            manifest
                .set_name(&mut self.db)
                .to(manifest_data.name.clone());
            manifest
                .set_version(&mut self.db)
                .to(manifest_data.version.clone());
            manifest.set_entry(&mut self.db).to(manifest_data.entry);
            manifest.set_content_hash(&mut self.db).to(manifest_hash);
            manifest.set_path(&mut self.db).to(Arc::new(manifest_path));
            manifest
        } else {
            register_manifest(&self.db, manifest_path, manifest_data, manifest_hash)
        };
        self.db.set_module_roots(roots);
        self.db.set_project_manifest(manifest);
        if let Some(package_map) = package_map {
            let map = if let Some(map) = self.db.package_module_map() {
                map.set_current_package(&mut self.db)
                    .to(package_map.current_package);
                map.set_current_target(&mut self.db)
                    .to(package_map.current_target);
                map.set_bindings(&mut self.db)
                    .to(Arc::new(package_map.bindings));
                map
            } else {
                PackageModuleMap::new(
                    &self.db,
                    package_map.current_package,
                    package_map.current_target,
                    Arc::new(package_map.bindings),
                )
            };
            self.db.set_package_module_map(map);
        } else {
            if let Some(map) = self.db.package_module_map() {
                map.set_bindings(&mut self.db).to(Arc::new(Vec::new()));
            }
            self.db.clear_package_module_map();
        }
        if let Some(root) = stdlib_root {
            self.db.set_stdlib_root(root);
        }
        self.revision = self.revision.next();
        (manifest, listing, roots)
    }

    /// Replace one package directory snapshot with one observable revision.
    pub fn set_directory_entries(&mut self, listing: DirectoryListing, entries: Vec<String>) {
        listing.set_entries(&mut self.db).to(Arc::new(entries));
        self.revision = self.revision.next();
    }

    /// Create + register a new file (CLI/test helper); bumps revision.
    pub fn new_file(&mut self, path: String, text: String) -> SourceFile {
        let file = self.db.new_file(path, text);
        self.revision = self.revision.next();
        file
    }

    /// Bump revision without writing (tests / synthetic cancel).
    pub fn bump_revision(&mut self) {
        self.revision = self.revision.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_middle::SymbolId;
    use std::path::PathBuf;

    #[test]
    fn set_text_advances_revision_and_stales_handles() {
        let mut host = AnalysisHost::new();
        let file = SourceFile::new(
            host.db(),
            1,
            Arc::from("func main() {}"),
            Arc::new(PathBuf::from("a.aru")),
        );
        host.register_source_file("a.aru".into(), file);
        let r0 = host.revision();

        // Resolve against revision r0 without holding a Storage clone across the write
        // (Salsa blocks `set_text` until all clones drop — holding a snapshot would deadlock).
        {
            let snap0 = host.snapshot();
            assert_eq!(snap0.revision, r0);
            let sym = LspSymbolId::new(SymbolId::new(1, 0), r0);
            assert_eq!(sym.resolve(&snap0), Some(SymbolId::new(1, 0)));
        }

        let sym = LspSymbolId::new(SymbolId::new(1, 0), r0);
        host.set_text(file, Arc::from("func main() { let x = 1; }"));
        let snap1 = host.snapshot();
        assert_ne!(snap1.revision, r0);
        assert!(
            sym.resolve(&snap1).is_none(),
            "old revision must not resolve"
        );
        let sym1 = LspSymbolId::new(SymbolId::new(1, 0), snap1.revision);
        assert_eq!(sym1.resolve(&snap1), Some(SymbolId::new(1, 0)));
    }

    #[test]
    fn snapshot_clone_is_independent_handle() {
        let mut host = AnalysisHost::new();
        let _ = host.new_file("t.aru".into(), "func main() {}".into());
        {
            let a = host.snapshot();
            let b = host.snapshot();
            assert_eq!(a.revision, b.revision);
        }
        // After drop, writes must not deadlock.
        host.bump_revision();
        assert!(host.revision().as_u64() > 0);
    }
}
