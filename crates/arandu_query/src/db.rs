use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use crate::explain::RebuildLog;
use crate::manifest::ProjectManifest;
use crate::vfs::{ModuleRoots, PackageModuleMap, ResolutionEpoch};
use arandu_middle::DataLayout;

mod cancel;
mod hash_eq;
mod registry;

pub use cancel::{catch_query_cancellation, QueryCancellationToken, QueryCancelled};
pub use hash_eq::HashEq;
use registry::{CstCache, FileRegistry};
use salsa::{Database as _, Setter, Storage};

pub type FileId = u32;

/// Origin is supplied by the embedding driver, never by Arandu source names.
/// A separate input per file preserves cutoff when another file is registered.
#[salsa::input]
pub(crate) struct SourceAuthority {
    pub trusted_stdlib: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RegistryMetrics {
    pub registered_paths: usize,
    pub live_file_ids: usize,
    pub allocated_file_ids: FileId,
}

pub use crate::stable_hash::StableHash;

#[salsa::db]
pub trait ArandCompilerDb: salsa::Database {
    fn source_text(&self, file: FileId) -> Arc<str>;
    fn file_path(&self, file: FileId) -> Arc<PathBuf>;
    /// Registered Salsa input for this numeric file id, if any.
    fn source_file_by_id(&self, file: FileId) -> Option<SourceFile>;
    fn as_source_db(&self) -> &dyn arandu_middle::db::SourceDatabase;
    /// Downcast to [`DatabaseImpl`] for CST cache / incremental reparse (default: none).
    fn as_db_impl(&self) -> Option<&DatabaseImpl> {
        None
    }
    /// Registered driver policy; missing registration is a reportable invariant failure.
    fn ctfe_config(&self) -> Option<crate::ctfe::CtfeConfig> {
        self.as_db_impl().and_then(DatabaseImpl::ctfe_config)
    }

    /// Salsa input for the compilation target. Defaults to the host layout;
    /// the CLI sets it from `--layout=` before any query runs.
    fn target_config(&self) -> TargetConfig {
        self.as_db_impl()
            .expect("target_config requires DatabaseImpl")
            .target_config()
    }
}

pub use arandu_middle::db::SourceFile;
pub use arandu_middle::db::TargetConfig;

/// Salsa database with optional DX.5 rebuild logging.
///
/// Prefer [`Self::default`] / [`Self::new`] for production (no event callback).
/// Use [`Self::with_rebuild_log`] when `-Zexplain-rebuild` is active.
#[salsa::db]
pub struct DatabaseImpl {
    storage: Storage<Self>,
    files: Arc<RwLock<FileRegistry>>,
    source_authorities: Arc<RwLock<FxHashMap<SourceFile, SourceAuthority>>>,
    /// Incremental CST reuse across `syntax_tree` queries (side cache; result still pure in text).
    cst_cache: Arc<Mutex<CstCache>>,
    /// Shared with the Salsa event callback when explain mode is on.
    rebuild_log: Option<Arc<RebuildLog>>,
    /// Resolved stdlib root (`stdlib/std/…` import prefix maps under this dir).
    /// Set via [`Self::set_stdlib_root`]; when `None`, import resolution falls
    /// back to walk-from-cwd (legacy monorepo) — prefer setting it always in CLI.
    stdlib_root: Arc<RwLock<Option<PathBuf>>>,
    /// Optional project manifest Salsa input (day-1 registration for invalidation).
    project_manifest: Arc<RwLock<Option<ProjectManifest>>>,
    /// Dual roots: package (`Arandu.toml`) + stdlib. Same `resolve_module_path`.
    module_roots: Arc<RwLock<Option<ModuleRoots>>>,
    /// P4 pre-resolved logical namespace. When present it is authoritative.
    package_modules: Arc<RwLock<Option<PackageModuleMap>>>,
    /// Compilation target Salsa input (default: host layout). See [`Self::set_target_config`].
    target_config: Arc<RwLock<Option<TargetConfig>>>,
    ctfe_config: Arc<RwLock<Option<crate::ctfe::CtfeConfig>>>,
    /// Stable Salsa input bumped on every resolution-config change so queries
    /// that ran before configuration invalidate. See [`ResolutionEpoch`].
    resolution_epoch: Arc<RwLock<Option<ResolutionEpoch>>>,
}

// Manual Clone: Storage is cloneable; share Arc file registry + log + CST cache.
impl Clone for DatabaseImpl {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            files: Arc::clone(&self.files),
            source_authorities: Arc::clone(&self.source_authorities),
            cst_cache: Arc::clone(&self.cst_cache),
            rebuild_log: self.rebuild_log.clone(),
            stdlib_root: Arc::clone(&self.stdlib_root),
            project_manifest: Arc::clone(&self.project_manifest),
            module_roots: Arc::clone(&self.module_roots),
            package_modules: Arc::clone(&self.package_modules),
            target_config: Arc::clone(&self.target_config),
            ctfe_config: Arc::clone(&self.ctfe_config),
            resolution_epoch: Arc::clone(&self.resolution_epoch),
        }
    }
}

impl Default for DatabaseImpl {
    fn default() -> Self {
        Self::new()
    }
}

#[salsa::db]
impl salsa::Database for DatabaseImpl {}

impl DatabaseImpl {
    /// Cancellation handle scoped to this database clone. Long-running tracked
    /// queries periodically observe it and unwind with Salsa's typed payload.
    #[must_use]
    pub fn query_cancellation_token(&self) -> QueryCancellationToken {
        QueryCancellationToken(self.cancellation_token())
    }

    /// Database without rebuild event overhead (installs lightweight counter callback only if `-Zprofile-queries` is enabled).
    #[must_use]
    pub fn new() -> Self {
        let callback = arandu_base::perf::PROFILE_QUERIES
            .load(std::sync::atomic::Ordering::Relaxed)
            .then(RebuildLog::profile_queries_callback);
        let mut db = Self {
            storage: Storage::new(callback),
            files: Arc::new(RwLock::new(FileRegistry::default())),
            source_authorities: Arc::new(RwLock::new(FxHashMap::default())),
            cst_cache: Arc::new(Mutex::new(CstCache::default())),
            rebuild_log: None,
            stdlib_root: Arc::new(RwLock::new(None)),
            project_manifest: Arc::new(RwLock::new(None)),
            module_roots: Arc::new(RwLock::new(None)),
            package_modules: Arc::new(RwLock::new(None)),
            target_config: Arc::new(RwLock::new(None)),
            ctfe_config: Arc::new(RwLock::new(None)),
            resolution_epoch: Arc::new(RwLock::new(None)),
        };
        db.set_target_config(DataLayout::host());
        db.set_ctfe_limits(crate::ctfe::CtfeLimits::default());
        db.ensure_resolution_epoch();
        db
    }

    /// Database with DX.5 causal-chain recording (Salsa `WillExecute` / validate).
    #[must_use]
    pub fn with_rebuild_log() -> (Self, Arc<RebuildLog>) {
        let log = RebuildLog::new();
        let callback = RebuildLog::salsa_callback(Arc::clone(&log));
        let mut db = Self {
            storage: Storage::new(Some(callback)),
            files: Arc::new(RwLock::new(FileRegistry::default())),
            source_authorities: Arc::new(RwLock::new(FxHashMap::default())),
            cst_cache: Arc::new(Mutex::new(CstCache::default())),
            rebuild_log: Some(Arc::clone(&log)),
            stdlib_root: Arc::new(RwLock::new(None)),
            project_manifest: Arc::new(RwLock::new(None)),
            module_roots: Arc::new(RwLock::new(None)),
            package_modules: Arc::new(RwLock::new(None)),
            target_config: Arc::new(RwLock::new(None)),
            ctfe_config: Arc::new(RwLock::new(None)),
            resolution_epoch: Arc::new(RwLock::new(None)),
        };
        db.set_target_config(DataLayout::host());
        db.set_ctfe_limits(crate::ctfe::CtfeLimits::default());
        db.ensure_resolution_epoch();
        (db, log)
    }

    #[must_use]
    pub fn rebuild_log(&self) -> Option<&Arc<RebuildLog>> {
        self.rebuild_log.as_ref()
    }

    /// Stable registry metrics. Allocated identities include unregistered files;
    /// they are intentionally monotonic for the lifetime of this database.
    #[must_use]
    pub fn registry_metrics(&self) -> RegistryMetrics {
        let registry = self.files.read().unwrap_or_else(|e| e.into_inner());
        RegistryMetrics {
            registered_paths: registry.by_path.len(),
            live_file_ids: registry.by_id.len(),
            allocated_file_ids: registry.next_id.saturating_sub(100),
        }
    }

    /// Create the resolution epoch input (idempotent). Called by the
    /// constructors so queries running before any configuration still register
    /// a dependency on the epoch and invalidate once configuration lands.
    fn ensure_resolution_epoch(&mut self) {
        if self
            .resolution_epoch
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        let epoch = ResolutionEpoch::new(self, 0);
        *self
            .resolution_epoch
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(epoch);
    }

    /// Advance the resolution epoch so that cached resolution queries recompute.
    ///
    /// Mutates the *same* Salsa input (rather than swapping in a fresh one) so
    /// queries that already read it are invalidated.
    fn bump_resolution_epoch(&mut self) {
        let epoch = *self
            .resolution_epoch
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let Some(epoch) = epoch else {
            return;
        };
        let next = epoch.generation(self).wrapping_add(1);
        epoch.set_generation(self).to(next);
    }

    /// Read the resolution epoch inside a tracked query, registering the
    /// dependency that `bump_resolution_epoch` later invalidates.
    pub fn resolution_epoch(&self) {
        let epoch = *self
            .resolution_epoch
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(epoch) = epoch {
            let _ = epoch.generation(self);
        }
    }

    /// Pin the stdlib root used by [`arandu_middle::db::SourceDatabase::resolve_module_path`].
    pub fn set_stdlib_root(&mut self, root: PathBuf) {
        let mut g = self.stdlib_root.write().unwrap_or_else(|e| e.into_inner());
        *g = Some(root);
        drop(g);
        self.bump_resolution_epoch();
    }

    #[must_use]
    pub fn stdlib_root(&self) -> Option<PathBuf> {
        self.stdlib_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Store the project manifest Salsa input (invalidation key for package mode).
    pub fn set_project_manifest(&mut self, manifest: ProjectManifest) {
        let mut g = self
            .project_manifest
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *g = Some(manifest);
        drop(g);
        self.bump_resolution_epoch();
    }

    #[must_use]
    pub fn project_manifest(&self) -> Option<ProjectManifest> {
        *self
            .project_manifest
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Register dual module roots (package + stdlib) for `resolve_module_path`.
    pub fn set_module_roots(&mut self, roots: ModuleRoots) {
        let std = roots.stdlib_root(self);
        {
            let mut g = self.module_roots.write().unwrap_or_else(|e| e.into_inner());
            *g = Some(roots);
        }
        // Keep flat stdlib_root in sync when roots carry one.
        if let Some(std) = std {
            self.set_stdlib_root(std.as_ref().clone());
        }
        self.bump_resolution_epoch();
    }

    #[must_use]
    pub fn module_roots(&self) -> Option<ModuleRoots> {
        *self.module_roots.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Set the compilation target (pointer width / alignment classes) as a
    /// Salsa input. Changing it invalidates exactly the semantic queries that
    /// depend on it (`module_signatures`, `item_body_typeck`, `file_typeck_view`).
    /// Defaults to the host layout when never called.
    pub fn set_target_config(&mut self, data_layout: DataLayout) {
        let existing = self
            .target_config
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .copied();
        match existing {
            Some(input) => {
                input.set_data_layout(self).to(data_layout);
            }
            None => {
                let input =
                    TargetConfig::new(self, data_layout, arandu_middle::db::TargetIdentity::host());
                let mut slot = self
                    .target_config
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                *slot = Some(input);
            }
        }
    }

    /// Set both data layout and platform target identity.
    pub fn set_target(
        &mut self,
        data_layout: DataLayout,
        identity: arandu_middle::db::TargetIdentity,
    ) {
        let existing = self
            .target_config
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .copied();
        match existing {
            Some(input) => {
                input.set_data_layout(self).to(data_layout);
                input.set_identity(self).to(identity);
            }
            None => {
                let input = TargetConfig::new(self, data_layout, identity);
                let mut slot = self
                    .target_config
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                *slot = Some(input);
            }
        }
    }

    /// Change public CTFE policy before querying (drop snapshots before writes).
    pub fn set_ctfe_limits(&mut self, limits: crate::ctfe::CtfeLimits) {
        let existing = *self
            .ctfe_config
            .read()
            .unwrap_or_else(|error| error.into_inner());
        match existing {
            Some(input) => {
                input.set_limits(self).to(limits);
            }
            None => {
                let input = crate::ctfe::CtfeConfig::new(self, limits);
                *self
                    .ctfe_config
                    .write()
                    .unwrap_or_else(|error| error.into_inner()) = Some(input);
            }
        }
    }

    /// Registered public CTFE policy input, shared by snapshots.
    #[must_use]
    pub fn ctfe_config(&self) -> Option<crate::ctfe::CtfeConfig> {
        *self
            .ctfe_config
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Supply platform identity explicitly before requesting semantic queries.
    pub fn set_target_identity(&mut self, identity: arandu_middle::db::TargetIdentity) {
        self.target_config().set_identity(self).to(identity);
    }

    /// Registered Salsa input for the compilation target.
    #[must_use]
    pub fn target_config(&self) -> TargetConfig {
        *self
            .target_config
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .expect("TargetConfig input is initialized by DatabaseImpl::new/with_rebuild_log")
    }

    pub fn set_package_module_map(&mut self, map: PackageModuleMap) {
        let mut current = self
            .package_modules
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *current = Some(map);
        drop(current);
        self.bump_resolution_epoch();
    }

    pub fn clear_package_module_map(&mut self) {
        let mut current = self
            .package_modules
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *current = None;
        drop(current);
        self.bump_resolution_epoch();
    }

    #[must_use]
    pub fn package_module_map(&self) -> Option<PackageModuleMap> {
        *self
            .package_modules
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn register_source_authority(&self, file: SourceFile) -> SourceAuthority {
        let mut authorities = self
            .source_authorities
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *authorities
            .entry(file)
            .or_insert_with(|| SourceAuthority::new(self, false))
    }

    /// Grant compiler-owned stdlib identities to a driver-verified source.
    /// Module declarations and path spellings cannot grant this authority.
    /// Call before analysis, from the stdlib loader or an embedded stdlib fixture.
    pub fn trust_stdlib_source(&mut self, file: SourceFile) {
        let was_registered = self
            .source_authorities
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&file);
        let authority = self.register_source_authority(file);
        if !was_registered {
            self.bump_resolution_epoch();
        }
        if !authority.trusted_stdlib(self) {
            authority.set_trusted_stdlib(self).to(true);
        }
    }

    #[must_use]
    pub fn is_trusted_stdlib_source(&self, file: SourceFile) -> bool {
        let authority = self
            .source_authorities
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file)
            .copied();
        match authority {
            Some(authority) => *authority.trusted_stdlib(self),
            None => {
                self.resolution_epoch();
                false
            }
        }
    }

    /// Register a source delivered by the stdlib loader. This API is an
    /// embedding trust boundary; it is never inferred from source syntax.
    pub fn new_stdlib_file(&mut self, path: String, text: String) -> SourceFile {
        let file = self.new_file(path, text);
        self.trust_stdlib_source(file);
        file
    }

    pub fn new_file(&mut self, path: String, text: String) -> SourceFile {
        // Drop previous registration first (separate lock scope — avoid deadlock).
        self.unregister_source_file(&path);
        let mut reg = self.files.write().unwrap_or_else(|e| e.into_inner());
        let file_id = reg
            .next_id()
            .expect("FileId allocator exhausted; ids are never reused");
        let file = SourceFile::new(
            self,
            file_id,
            Arc::from(text),
            Arc::new(std::path::PathBuf::from(&path)),
        );
        self.register_source_authority(file);
        reg.insert(path, file_id, file);
        file
    }

    /// Update the text of an already-registered source file and notify Salsa.
    ///
    /// This is the preferred API for tests and fuzz targets that depend on
    /// `arandu_query` but do not (and should not) take a direct `salsa` dep.
    pub fn update_file_text(&mut self, file: SourceFile, text: impl Into<Arc<str>>) {
        use salsa::Setter as _;
        file.set_text(self).to(text.into());
    }

    pub fn register_source_file(&self, path: String, file: SourceFile) {
        let mut reg = self.files.write().unwrap_or_else(|e| e.into_inner());
        let file_id = file.file_id(self.as_source_db());
        self.register_source_authority(file);
        if *file_id >= reg.next_id {
            reg.next_id = file_id.saturating_add(1);
        }
        reg.insert(path, *file_id, file);
    }

    /// Drop a registry key so `resolve_module_path` no longer returns a stale file.
    ///
    /// Used by watch mode when an `.aru` is deleted. Does **not** swallow the
    /// broken import — dependents re-resolve and emit M001.
    pub fn unregister_source_file(&self, path: &str) {
        let file = {
            let mut reg = self.files.write().unwrap_or_else(|e| e.into_inner());
            reg.remove_path(path)
        };
        if let Some(file) = file {
            let fid = file.file_id(self.as_source_db());
            // A file can deliberately have more than one registry key (for
            // example its absolute editor path plus package-qualified and bare
            // import keys). Keep the reverse index alive while any alias still
            // refers to the same Salsa input.
            let candidates: Vec<SourceFile> = {
                let reg = self.files.read().unwrap_or_else(|e| e.into_inner());
                reg.by_path.values().copied().collect()
            };
            let has_alias = candidates
                .into_iter()
                .any(|candidate| candidate.file_id(self.as_source_db()) == fid);
            if !has_alias {
                let mut reg = self.files.write().unwrap_or_else(|e| e.into_inner());
                reg.by_id.remove(fid);
                drop(reg);
                self.cst_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .by_file
                    .remove(fid);
            }
        }
    }

    /// True if `path` is currently registered as a SourceFile key.
    #[must_use]
    pub fn is_registered(&self, path: &str) -> bool {
        let reg = self.files.read().unwrap_or_else(|e| e.into_inner());
        reg.by_path.contains_key(path)
    }

    /// Lookup registered SourceFile by import/registry key.
    #[must_use]
    pub fn source_file_by_path(&self, path: &str) -> Option<SourceFile> {
        let reg = self.files.read().unwrap_or_else(|e| e.into_inner());
        reg.by_path.get(path).copied()
    }

    /// O(1) reverse lookup: compiler `FileId` → open/registered [`SourceFile`].
    #[must_use]
    pub fn source_file_by_id(&self, file_id: FileId) -> Option<SourceFile> {
        let reg = self.files.read().unwrap_or_else(|e| e.into_inner());
        reg.by_id.get(&file_id).copied()
    }

    /// Build or reuse CST for `file_id`/`text` via [`arandu_parser::reparse_subtree`] when possible.
    /// Shares the `Arc<str>` buffer with the tree (no extra text copy).
    pub(crate) fn syntax_tree_for_arc(
        &self,
        file_id: FileId,
        text: Arc<str>,
    ) -> arandu_parser::SyntaxTree {
        let mut cache = self.cst_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = cache.by_file.get(&file_id) {
            if prev.text() == text.as_ref() {
                return prev.clone();
            }
            if let Some((start, end, repl)) =
                arandu_parser::single_contiguous_edit(prev.text(), text.as_ref())
            {
                let (_src, tree) = arandu_parser::reparse_subtree(prev, start, end, &repl);
                if tree.text() == text.as_ref() {
                    cache.by_file.insert(file_id, tree.clone());
                    return tree;
                }
            }
        }
        let tree = arandu_parser::parse_syntax_arc(text);
        cache.by_file.insert(file_id, tree.clone());
        tree
    }
}

impl arandu_middle::db::SourceDatabase for DatabaseImpl {
    fn source_file_by_id(&self, file_id: FileId) -> Option<SourceFile> {
        DatabaseImpl::source_file_by_id(self, file_id)
    }

    fn exported_symbols(&self, file: SourceFile) -> Arc<arandu_middle::ExportedSymbolTable> {
        crate::passes::exported_symbols(self, file).clone()
    }

    fn internal_symbols(&self, file: SourceFile) -> Arc<arandu_middle::ExportedSymbolTable> {
        crate::passes::internal_symbols(self, file).clone()
    }

    fn same_package(&self, current_file: FileId, imported_file: SourceFile) -> bool {
        self.resolution_epoch();
        let Some(map) = self.package_module_map() else {
            return false;
        };
        let bindings = map.bindings(self);
        let current_package = bindings
            .iter()
            .find(|(_, binding)| *binding.file.file_id(self) == current_file)
            .map(|(_, binding)| binding.package);
        let imported_package = bindings
            .iter()
            .find(|(_, binding)| binding.file == imported_file)
            .map(|(_, binding)| binding.package);
        matches!((current_package, imported_package), (Some(a), Some(b)) if a == b)
    }

    fn symbol_span(&self, symbol_id: arandu_middle::SymbolId) -> arandu_base::Span {
        crate::passes::symbol_span(self, symbol_id)
    }

    fn resolve_module_path(&self, path: &str) -> Option<SourceFile> {
        // Depend on the resolution epoch so a query that ran before the
        // package/stdlib configuration existed recomputes once it lands.
        self.resolution_epoch();
        if let Some(map) = self.package_module_map() {
            if let Some(binding) = crate::vfs::package_module(self, map, path) {
                return Some(binding.file);
            }
            // Package mode is fail-closed. Stdlib retains its separately
            // installed root; no arbitrary registry/cwd fallback is allowed.
            if !path.starts_with("stdlib/") && !path.starts_with("stdlib\\") {
                return None;
            }
        }
        // Fast path: O(1) lookup by import path string (registry key).
        {
            let reg = self.files.read().unwrap_or_else(|e| e.into_inner());
            if let Some(file) = reg.by_path.get(path) {
                return Some(*file);
            }
        }

        // Module roots translate logical imports into paths, but never load
        // those paths. CLI/LSP discovery must register the corresponding
        // SourceFile before semantic queries run.
        let physical = self
            .module_roots()
            .and_then(|roots| crate::vfs::map_import_key(self, roots, path))
            .or_else(|| {
                (path.starts_with("stdlib/") || path.starts_with("stdlib\\"))
                    .then(|| self.stdlib_root())
                    .flatten()
                    .map(|root| crate::stdlib::import_path_on_disk(&root, path))
            })?;
        let reg = self.files.read().unwrap_or_else(|e| e.into_inner());
        let key = physical.to_string_lossy();
        if let Some(file) = reg.by_path.get(key.as_ref()) {
            return Some(*file);
        }
        // LSP registry keys are URI-portable (`/` separators and no Windows
        // verbatim prefix), while ModuleRoots retains an OS-native PathBuf.
        let portable = key.replace('\\', "/");
        let portable = portable.strip_prefix("//?/").unwrap_or(&portable);
        reg.by_path.get(portable).copied()
    }

    fn package_mode(&self) -> bool {
        self.resolution_epoch();
        self.project_manifest().is_some()
    }
}

#[salsa::db]
impl ArandCompilerDb for DatabaseImpl {
    /// O(1) lookup by FileId via the reverse index.
    fn source_text(&self, file: FileId) -> Arc<str> {
        let reg = self
            .files
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.by_id
            .get(&file)
            .map(|f| f.text(self.as_source_db()).clone())
            .unwrap_or_else(|| Arc::from(""))
    }

    /// O(1) lookup by FileId via the reverse index.
    fn file_path(&self, file: FileId) -> Arc<PathBuf> {
        let reg = self
            .files
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.by_id
            .get(&file)
            .map(|f| f.path(self.as_source_db()).clone())
            .unwrap_or_else(|| Arc::new(PathBuf::new()))
    }

    fn source_file_by_id(&self, file: FileId) -> Option<SourceFile> {
        DatabaseImpl::source_file_by_id(self, file)
    }

    fn as_source_db(&self) -> &dyn arandu_middle::db::SourceDatabase {
        self
    }

    fn as_db_impl(&self) -> Option<&DatabaseImpl> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::DatabaseImpl;
    use std::sync::Arc;

    #[test]
    fn unregistering_last_file_alias_evicts_its_cached_cst() {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("temporary.aru".into(), "func f(): int { return 1 }".into());
        let file_id = *file.file_id(&db);
        let _tree = db.syntax_tree_for_arc(file_id, Arc::clone(file.text(&db)));
        db.register_source_file("temporary-alias.aru".into(), file);

        assert!(db
            .cst_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .by_file
            .contains_key(&file_id));

        db.unregister_source_file("temporary.aru");
        assert!(db
            .cst_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .by_file
            .contains_key(&file_id));

        db.unregister_source_file("temporary-alias.aru");

        assert!(!db
            .cst_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .by_file
            .contains_key(&file_id));
    }

    #[test]
    fn file_id_allocator_does_not_wrap() {
        let mut registry = super::registry::FileRegistry {
            next_id: u32::MAX,
            ..super::registry::FileRegistry::default()
        };
        assert!(registry.next_id().is_none());
        assert_eq!(registry.next_id, u32::MAX);
    }
}
