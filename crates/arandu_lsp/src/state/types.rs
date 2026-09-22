//! Core ServerState types, snapshot handles, and document index mappings.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use arandu_query::db::SourceFile;
use arandu_query::{
    AnalysisHost, AnalysisRevision, AnalysisSnapshot, DirectoryListing, DocumentId, DocumentStore,
};
use lsp_types::Uri;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::uri_util::path_from_uri;
use crate::vfs::Vfs;

pub struct PackageState {
    pub manifest_path: PathBuf,
    pub package_src: PathBuf,
    pub package_name: String,
    pub listing: DirectoryListing,
    pub(super) entries: Vec<String>,
}

/// Cloneable view of a registered document handed to worker jobs.
#[derive(Clone)]
pub struct DocInfo {
    pub source: SourceFile,
    pub path: Arc<PathBuf>,
}

pub struct ServerState {
    /// Admitted requests whose terminal result has not been delivered yet.
    /// Worker completion alone must not release the discovery writer.
    pub(crate) pending_requests: FxHashSet<lsp_server::RequestId>,
    /// One coalesced background package update, applied after response delivery.
    pub(crate) deferred_reload: Option<Result<Box<crate::workspace::WorkspaceProject>, String>>,
    pub host: AnalysisHost,
    pub docs: DocumentStore,
    pub vfs: Vfs,
    pub by_uri: FxHashMap<String, DocumentId>,
    /// URIs currently owned by editor overlays. Known workspace files may be
    /// registered and queryable without being open.
    pub open_uris: FxHashSet<String>,
    /// Numeric compiler `file_id` → open document (multi-file workspace).
    pub by_file_id: FxHashMap<u32, DocumentId>,
    /// Latest client document version for each open buffer.
    pub versions: FxHashMap<DocumentId, i32>,
    /// Last published diagnostic fingerprint per document (skip no-op publish).
    pub last_diag_fp: FxHashMap<DocumentId, ([u8; 32], Option<i32>)>,
    /// P3: last per-item IDE diag fingerprints (DocumentId, item local key).
    pub last_item_diag_fp: FxHashMap<(DocumentId, u32, u32), [u8; 32]>,
    /// Scheduler rejections produced on the event-loop thread itself.
    ///
    /// The loop is the only consumer of the bounded job channel, so sending
    /// its own rejections through that channel could deadlock it. They are
    /// staged here and drained at the top of the next iteration (bounded in
    /// practice by one rejection per processed protocol message).
    pub(crate) deferred_rejections: VecDeque<lsp_server::RequestId>,
    /// Import registry keys owned by each compiler file identity. Filesystem
    /// events may arrive through a path spelling that cannot be reconstructed
    /// after rename (Windows verbatim paths, junctions), so removal uses this
    /// recorded ownership rather than guessing aliases from the stale path.
    pub(super) package_aliases: FxHashMap<u32, Vec<String>>,
    /// Active package metadata. It is installed after the initialize handshake
    /// and its directory listing is the watched Salsa input for local imports.
    pub package: Option<PackageState>,
}

impl ServerState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending_requests: FxHashSet::default(),
            deferred_reload: None,
            host: AnalysisHost::new(),
            docs: DocumentStore::new(),
            vfs: Vfs::new(),
            by_uri: FxHashMap::default(),
            open_uris: FxHashSet::default(),
            by_file_id: FxHashMap::default(),
            versions: FxHashMap::default(),
            last_diag_fp: FxHashMap::default(),
            last_item_diag_fp: FxHashMap::default(),
            deferred_rejections: VecDeque::new(),
            package_aliases: FxHashMap::default(),
            package: None,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        self.host.snapshot()
    }

    #[must_use]
    pub fn revision(&self) -> AnalysisRevision {
        self.host.revision()
    }

    /// URI → document view for all registered documents.
    pub(crate) fn doc_info_map(&self) -> FxHashMap<String, DocInfo> {
        let mut map = FxHashMap::default();
        for (uri, &id) in &self.by_uri {
            if let Some(doc) = self.docs.get(id) {
                map.insert(
                    uri.clone(),
                    DocInfo {
                        source: doc.source,
                        path: Arc::clone(&doc.path),
                    },
                );
            }
        }
        map
    }

    /// DocumentId → document view for all registered documents.
    pub(crate) fn doc_infos_by_id(&self) -> FxHashMap<DocumentId, DocInfo> {
        let mut by_id = FxHashMap::default();
        for &id in self.by_uri.values() {
            if let Some(doc) = self.docs.get(id) {
                by_id.insert(
                    id,
                    DocInfo {
                        source: doc.source,
                        path: Arc::clone(&doc.path),
                    },
                );
            }
        }
        by_id
    }

    #[inline]
    pub(super) fn path_of(uri: &Uri) -> PathBuf {
        path_from_uri(uri)
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}
