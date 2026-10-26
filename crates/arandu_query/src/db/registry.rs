use rustc_hash::FxHashMap;

use super::{FileId, SourceFile};

/// Per-file CST cache for incremental [`crate::passes::syntax_tree`] rebuilds.
#[derive(Default)]
pub(super) struct CstCache {
    /// Last successful tree per file (text must match before reuse).
    pub(super) by_file: FxHashMap<FileId, arandu_parser::SyntaxTree>,
}

/// Internal shared state for the file registry.
///
/// Two maps are kept in sync at every insertion point:
/// - `by_path`  — `String → SourceFile` for import path resolution (O(1) by path)
/// - `by_id`    — `FileId → SourceFile` for Salsa queries (O(1) by FileId)
///
/// Before this change both `source_text` and `file_path` performed an O(N)
/// linear scan over `by_path.values()` to find a file by its numeric ID.
#[derive(Clone)]
pub(super) struct FileRegistry {
    pub(super) by_path: FxHashMap<String, SourceFile>,
    pub(super) by_id: FxHashMap<FileId, SourceFile>,
    /// Monotonic id allocator (must not reuse after unregister — Salsa keeps old inputs).
    pub(super) next_id: FileId,
}

impl Default for FileRegistry {
    fn default() -> Self {
        Self {
            by_path: FxHashMap::default(),
            by_id: FxHashMap::default(),
            next_id: 100,
        }
    }
}

impl FileRegistry {
    /// Insert a file into both indexes simultaneously.
    pub(super) fn insert(&mut self, path: String, file_id: FileId, file: SourceFile) {
        self.by_path.insert(path, file);
        self.by_id.insert(file_id, file);
    }

    /// Next available FileId (starts at 100 to avoid collisions with test stubs).
    ///
    /// `u32::MAX` is never issued. A further allocation fails instead of wrapping
    /// back onto an id Salsa may still be holding.
    pub(super) fn next_id(&mut self) -> Option<FileId> {
        let next = self.next_id.checked_add(1)?;
        let id = self.next_id;
        self.next_id = next;
        Some(id)
    }

    pub(super) fn remove_path(&mut self, path: &str) -> Option<SourceFile> {
        let file = self.by_path.remove(path)?;
        // by_id cleaned by caller once file_id is known (needs Database).
        Some(file)
    }
}
