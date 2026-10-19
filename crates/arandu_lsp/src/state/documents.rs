//! Document lifecycle, open overlays, disk replacements, and file renames.

use std::path::PathBuf;
use std::sync::Arc;

use arandu_query::DocumentId;
use lsp_types::Uri;

use super::path::registry_path_key;
use super::types::ServerState;

impl ServerState {
    /// Open document or apply committed text (after VFS flush).
    pub fn open_or_commit(&mut self, uri: &Uri, text: String) -> DocumentId {
        let path = Self::path_of(uri);
        let uri_s = uri.as_str().to_string();
        if let Some(&id) = self.by_uri.get(&uri_s) {
            if let Some(doc) = self.docs.get_mut(id) {
                let source = doc.source;
                let fid = *source.file_id(self.host.db());
                self.host.set_text(source, Arc::from(text));
                let path_key = registry_path_key(&path);
                if !self.host.db().is_registered(&path_key) {
                    self.host.register_source_file(path_key, source);
                }
                self.register_package_aliases(&path, source);
                self.by_file_id.insert(fid, id);
                return id;
            }
            self.by_uri.remove(&uri_s);
        }
        // DatabaseImpl is the sole FileId allocator. A second LSP-side counter
        // can collide with lazily loaded stdlib/package files and corrupt goto.
        let source = self.host.new_file(registry_path_key(&path), text);
        if self.host.db().stdlib_root().as_ref().is_some_and(|root| {
            super::path::normalize_path_soft(&path)
                .starts_with(super::path::normalize_path_soft(root))
        }) {
            self.host.db_mut().trust_stdlib_source(source);
        }
        let file_id = *source.file_id(self.host.db());
        self.register_package_aliases(&path, source);
        let id = self.docs.open(path, source);
        self.by_uri.insert(uri_s, id);
        self.by_file_id.insert(file_id, id);
        id
    }

    pub fn mark_open(&mut self, uri: &Uri) {
        self.open_uris.insert(uri.as_str().to_string());
    }

    #[must_use]
    pub fn is_open(&self, uri: &Uri) -> bool {
        self.open_uris.contains(uri.as_str())
    }

    /// Close an editor overlay. Restore the authoritative disk contents when
    /// the workspace file still exists; otherwise remove its registration.
    pub fn close_uri(&mut self, uri: &Uri) {
        self.open_uris.remove(uri.as_str());
        self.discard_pending(uri);
        let path = Self::path_of(uri);
        if path.extension().and_then(|ext| ext.to_str()) == Some("aru") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                self.replace_closed_overlay(uri, path, text);
                return;
            }
        }
        self.remove_uri(uri);
    }

    pub(super) fn replace_closed_overlay(&mut self, uri: &Uri, path: PathBuf, text: String) {
        let uri_s = uri.as_str().to_string();
        let Some(old_id) = self.by_uri.get(&uri_s).copied() else {
            self.open_or_commit(uri, text);
            return;
        };
        let Some(old_doc) = self.docs.get(old_id).cloned() else {
            self.by_uri.remove(&uri_s);
            self.open_or_commit(uri, text);
            return;
        };
        let source = old_doc.source;
        let file_id = *source.file_id(self.host.db());
        self.host.set_text(source, Arc::from(text));
        self.docs.close(old_id);
        let disk_id = self.docs.open(path, source);
        self.by_uri.insert(uri_s, disk_id);
        self.by_file_id.insert(file_id, disk_id);
        self.versions.remove(&old_id);
        self.last_diag_fp.remove(&old_id);
        self.last_item_diag_fp
            .retain(|&(doc, _, _), _| doc != old_id);
    }

    /// Remove a workspace source. An open overlay remains locally usable but
    /// is unregistered so imports cannot resolve a file deleted on disk.
    pub fn remove_uri(&mut self, uri: &Uri) {
        let uri_s = uri.as_str();
        self.discard_pending(uri);
        let path = Self::path_of(uri);
        let path_key = registry_path_key(&path);
        let id = self.by_uri.get(uri_s).copied();
        let source_id = id
            .and_then(|id| self.docs.get(id))
            .map(|doc| *doc.source.file_id(self.host.db()));
        self.unregister_package_aliases(&path, source_id);
        if self.host.db().is_registered(&path_key) {
            self.host.unregister_source_file(&path_key);
        }
        if let Some(id) = id {
            self.versions.remove(&id);
            self.last_diag_fp.remove(&id);
            self.last_item_diag_fp.retain(|&(doc, _, _), _| doc != id);
        }
        if !self.open_uris.contains(uri_s) {
            let Some(id) = self.by_uri.remove(uri_s) else {
                return;
            };
            if let Some(doc) = self.docs.get(id) {
                let fid = doc.source.file_id(self.host.db());
                self.by_file_id.remove(fid);
            }
            self.docs.close(id);
        }
    }

    /// Reload a closed `.aru` file after a client filesystem notification.
    pub fn reload_uri_from_disk(&mut self, uri: &Uri) -> Option<DocumentId> {
        if self.is_open(uri) {
            return self.by_uri.get(uri.as_str()).copied();
        }
        let path = Self::path_of(uri);
        if path.extension().and_then(|ext| ext.to_str()) != Some("aru") {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        Some(self.open_or_commit(uri, text))
    }

    /// Move a known source to a fresh path/identity after a filesystem rename.
    pub fn rename_uri(&mut self, old_uri: &Uri, new_uri: &Uri) -> Option<DocumentId> {
        let was_open = self.open_uris.remove(old_uri.as_str());
        let overlay_text = was_open.then(|| self.text_for_change(old_uri));
        self.remove_uri(old_uri);
        let text = overlay_text.or_else(|| {
            let path = Self::path_of(new_uri);
            std::fs::read_to_string(path).ok()
        })?;
        let id = self.open_or_commit(new_uri, text);
        if was_open {
            self.mark_open(new_uri);
        }
        Some(id)
    }
}
