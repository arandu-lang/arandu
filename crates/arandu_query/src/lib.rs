pub mod analysis;
pub mod artifact_cache;
pub mod cache;
pub mod dataflow;
pub mod db;
pub mod debounce;
pub mod doc_store;
pub mod docs;
pub mod edit_vfs;
pub mod explain;
pub mod highlight;
pub mod lockfile;
pub mod manifest;
pub mod package_graph;
pub mod passes;
pub mod rename;
pub mod stable_hash;
pub mod stdlib;
pub mod testing;
pub mod vfs;
pub mod watch_buf;

pub use analysis::{
    AnalysisHost, AnalysisRevision, AnalysisSnapshot, LspSymbolId, PackageConfiguration,
    ResolvedPackageMap,
};
pub use artifact_cache::{
    decode_compiler_artifact, encode_compiler_artifact, ArtifactDecodeError, ArtifactDigest,
    CompilerArtifactKind, VerifiedCompilerArtifact, ARTIFACT_SCHEMA_VERSION,
};
pub use cache::{
    CacheDigest, CacheDigestError, CacheLayout, CacheLayoutError, CACHE_DIGEST_ALGORITHM,
};
pub use dataflow::{
    block_borrow_facts, block_dataflow_facts, block_diagnostics, file_func_symbols,
    file_ide_diagnostics, file_signature_ide_diagnostics, func_amir, func_analysis_diags,
    func_borrow_summaries, ide_diags_fingerprint, item_ide_diagnostics, item_ide_diags_fingerprint,
    liveness_facts, BorrowFacts, DataflowFacts, IdeDiagnostic, IdeHint, IdeLabel, IdeReplacement,
    LivenessMap,
};
pub use db::{
    catch_query_cancellation, ArandCompilerDb, DatabaseImpl, QueryCancellationToken,
    QueryCancelled, RegistryMetrics, SourceFile,
};
pub use doc_store::{DocumentId, DocumentStore, OpenDocument};
pub use docs::{file_doctests, item_doc, module_doc};
pub use explain::{any_execute, RebuildCounts, RebuildEvent, RebuildLog};
pub use highlight::{
    compute_highlights, file_highlights, highlights_in_range, HlKind, HlToken, MOD_DECLARATION,
    MOD_DEFINITION, MOD_MUTABLE,
};
pub use lockfile::{
    semantic_manifest_fingerprint, LockedPackage, Lockfile, LockfileError, LOCK_FILENAME,
    LOCK_VERSION,
};
pub use manifest::{
    ensure_toolchain_compatible, hash_manifest_bytes, manifest_fingerprint, parse_manifest_bytes,
    parse_manifest_str, register_manifest, validate_git_dependency_identity, validate_git_origin,
    CapabilityPolicy, EffectPolicy, ManifestData, ManifestDependency, ManifestDiscovery,
    ManifestEdition, ManifestError, ManifestSpelling, ManifestTarget, ManifestWorkspace,
    PackageKind, ProjectManifest, LEGACY_MANIFEST_FILENAME, MANIFEST_FILENAME,
};
pub use package_graph::{
    LocalPackage, LocalPackageGraph, MaterializedGitPackage, PackageGraphLimits, PackageModulePlan,
    PlannedModuleBinding,
};
pub use rename::{prepare_rename, rename_occurrences, validate_rename, RenameError, RenameTarget};
// re-export for tests/CLI convenience
pub use debounce::{DebouncedMap, DEFAULT_DEBOUNCE};
pub use edit_vfs::{EditVfs, Vfs};
pub use passes::{
    borrow_interfaces, file_typeck_view, item_body_typeck, lower_amir, syntax_tree,
    BorrowInterfaces, LowerAmirArtifacts,
};
pub use stable_hash::StableHash;
pub use stdlib::{
    import_path_on_disk, is_stdlib_root, resolve_exe_path, resolve_stdlib_root, StdlibNotFound,
    StdlibResolveOpts, StdlibRoot, StdlibSource, INSTALL_RELATIVE, STDLIB_ENV,
};
pub use testing::{
    file_benchmark_manifest, file_test_manifest, item_benchmark_case, item_test_case,
};
pub use vfs::{
    listing_contains, map_import_key, package_module, scan_aru_entries, validate_package_name,
    DirectoryListing, ModuleBinding, ModuleRoots, PackageModuleMap, ReservedNameError,
    RESERVED_PACKAGE_ROOTS,
};
pub use watch_buf::{
    abs_path, FsChange, PackageWatchConfig, PackageWatchSession, WatchBuffer, WatchCommitSummary,
};
