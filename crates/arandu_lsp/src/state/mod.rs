//! Server state: AnalysisHost + DocumentStore + VFS + URI maps.

pub mod discovery;
pub mod documents;
pub mod package;
pub mod path;
pub mod types;
pub mod vfs_ops;

pub use discovery::discover_aru_files;
pub use types::{DocInfo, ServerState};

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::path::package_relative_path;
    use super::path::registry_path_key;
    use super::*;
    use crate::uri_util::{parse_uri, uri_from_path};
    use crate::vfs::Vfs;
    use arandu_query::ManifestData;
    use lsp_types::Uri;
    use std::sync::Arc;
    use std::time::Duration;

    fn file_url(name: &str) -> Uri {
        uri_from_path(std::path::Path::new(&format!("/tmp/{name}")))
            .or_else(|| parse_uri(&format!("file:///tmp/{name}")))
            .expect("uri")
    }

    #[cfg(windows)]
    #[test]
    fn missing_verbatim_path_keeps_package_relative_identity() {
        let src = std::path::Path::new(r"\\?\D:\a\Arandu-Lang\src");
        let removed = std::path::Path::new(r"D:\a\Arandu-Lang\src\util.aru");
        assert_eq!(
            package_relative_path(removed, src).as_deref(),
            Some("util.aru")
        );
    }

    #[test]
    fn workspace_discovery_is_sorted_bounded_and_skips_build_trees() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create source fixture");
        std::fs::create_dir_all(root.join("target")).expect("create ignored fixture");
        std::fs::write(root.join("src/z.aru"), "func z() {}").expect("write z");
        std::fs::write(root.join("src/a.aru"), "func a() {}").expect("write a");
        std::fs::write(root.join("target/ignored.aru"), "func ignored() {}")
            .expect("write ignored");

        let files = discover_aru_files(std::slice::from_ref(&root));
        let names: Vec<_> = files
            .iter()
            .filter_map(|(path, _)| path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(names, vec!["a.aru", "z.aru"]);

        std::fs::remove_dir_all(root).expect("remove discovery fixture");
    }

    #[test]
    fn queue_change_does_not_bump_revision_until_flush() {
        let mut st = ServerState::new();
        // Instant flush for test.
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("a.aru");
        let id = st.open_or_commit(&uri, "func main() {}".into());
        let r0 = st.revision();

        st.queue_change(&uri, "func main() { let x = 1; }".into());
        assert_eq!(
            st.revision(),
            r0,
            "pending VFS must not touch AnalysisRevision"
        );

        let committed = st.flush_all();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].1, id);
        assert_ne!(st.revision(), r0, "commit must advance revision");
    }

    #[test]
    fn n_changes_one_commit_one_revision_bump() {
        let mut st = ServerState::new();
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("b.aru");
        st.open_or_commit(&uri, "func main() {}".into());
        let r0 = st.revision();

        st.queue_change(&uri, "v1".into());
        st.queue_change(&uri, "v2".into());
        st.queue_change(&uri, "v3".into());
        assert_eq!(st.vfs.pending_count(), 1);

        let committed = st.flush_all();
        assert_eq!(committed.len(), 1);
        // One flush of one file → one set_text → one bump from r0.
        assert_eq!(st.revision().as_u64(), r0.as_u64() + 1);
    }

    #[test]
    fn text_for_change_prefers_latest_pending_vfs_text() {
        let mut st = ServerState::new();
        let uri = file_url("rapid.aru");
        st.open_or_commit(&uri, "committed".into());

        st.queue_change(&uri, "pending-1".into());
        assert_eq!(st.text_for_change(&uri), "pending-1");

        st.queue_change(&uri, "pending-2".into());
        assert_eq!(st.text_for_change(&uri), "pending-2");
    }

    #[test]
    fn package_create_registers_import_alias_and_invalidates_missing_import() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-package-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let src = root.join("src");
        std::fs::create_dir_all(&src).expect("create fixture");
        let discovered_src = std::fs::canonicalize(&src).expect("canonical source root");
        let main_path = src.join("main.aru");
        let main_text = concat!(
            "module editor_gold\n",
            "import editor_gold.util as util\n",
            "import std.path as path\n",
            "func main(): int {\n",
            "    if path.Path.from(\"\").isEmpty() { return util.answer() }\n",
            "    return 0\n",
            "}\n",
        );
        std::fs::write(&main_path, main_text).expect("write entry");
        let mut state = ServerState::new();
        let stdlib_root =
            arandu_query::resolve_stdlib_root(arandu_query::StdlibResolveOpts::default())
                .ok()
                .map(|stdlib| stdlib.path);
        state
            .configure_package(crate::workspace::WorkspaceProject {
                manifest_path: root.join("Arandu.toml"),
                manifest_data: ManifestData::legacy(
                    "editor_gold".into(),
                    "0.1.0".into(),
                    "src/main.aru".into(),
                ),
                manifest_hash: "fixture".into(),
                package_src: discovered_src,
                entries: vec!["main.aru".into()],
                stdlib_root: stdlib_root.clone(),
                module_plan: None,
                module_files: Vec::new(),
            })
            .expect("configure package");
        let stdlib_path = stdlib_root.expect("workspace stdlib").join("std/path.aru");
        let stdlib_uri = uri_from_path(&stdlib_path).expect("stdlib URI");
        let stdlib_document = state.open_or_commit(
            &stdlib_uri,
            std::fs::read_to_string(&stdlib_path).expect("read stdlib module"),
        );
        let main_uri = uri_from_path(&main_path).expect("main URI");
        let _discovered_id = state.open_or_commit(&main_uri, main_text.into());
        let main_id = state.open_or_commit(&main_uri, main_text.into());
        state.mark_open(&main_uri);
        let main = state.docs.get(main_id).expect("main document").source;
        let _ = arandu_query::passes::module_signatures(state.host.db(), main);
        assert!(arandu_query::passes::type_check(state.host.db(), main)
            .diagnostics
            .iter()
            .any(|diag| matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)));
        let stdlib_file = state
            .docs
            .get(stdlib_document)
            .expect("stdlib module registered before initial typecheck")
            .source;
        let stdlib_file_id = *stdlib_file.file_id(state.host.db());

        let util_path = src.join("util.aru");
        std::fs::write(
            &util_path,
            "/// Package answer.\npublic func answer(): int { return 42 }\n",
        )
        .expect("write module");
        let util_uri = uri_from_path(&util_path).expect("util URI");
        state
            .reload_uri_from_disk(&util_uri)
            .expect("register module");
        let util_id = state
            .by_uri
            .get(util_uri.as_str())
            .and_then(|&id| state.docs.get(id))
            .map(|doc| *doc.source.file_id(state.host.db()))
            .expect("created module id");
        assert_ne!(stdlib_file_id, util_id, "FileId allocator must be global");
        assert_eq!(
            state
                .host
                .db()
                .source_file_by_id(stdlib_file_id)
                .map(|file| *file.file_id(state.host.db())),
            Some(stdlib_file_id),
            "creating a workspace file must not replace stdlib reverse identity"
        );
        assert!(state.host.db().is_registered("editor_gold/util.aru"));
        assert!(state.refresh_package_listing());
        assert!(
            arandu_middle::db::SourceDatabase::resolve_module_path(
                state.host.db(),
                "editor_gold/util.aru"
            )
            .is_some(),
            "import registry must resolve the created module"
        );
        let resolved = arandu_query::passes::resolve(state.host.db(), main);
        assert!(
            resolved
                .diagnostics
                .iter()
                .all(|diag| !matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "stale resolve diagnostics: {:?}",
            resolved.diagnostics
        );
        let signatures = arandu_query::passes::module_signatures(state.host.db(), main);
        assert!(
            signatures
                .diagnostics
                .iter()
                .all(|diag| !matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "stale signature diagnostics: {:?}",
            signatures.diagnostics
        );
        let file_view = arandu_query::passes::file_typeck_view(state.host.db(), main);
        assert!(
            file_view
                .diagnostics
                .iter()
                .all(|diag| !matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "stale file view diagnostics: {:?}",
            file_view.diagnostics
        );
        let diagnostics = &arandu_query::passes::type_check(state.host.db(), main).diagnostics;
        assert!(diagnostics.is_empty(), "stale diagnostics: {diagnostics:?}");
        let tc = arandu_query::passes::type_check(state.host.db(), main);
        assert!(
            tc.symbols.lookup_module_member("util", "answer").is_some(),
            "created module members must be available to completion"
        );
        let call = main_text.find("util.answer").expect("util call");
        let items = crate::ide::completions(
            &state.snapshot(),
            main,
            main_text,
            crate::conv::offset_to_position(
                &arandu_base::LineIndex::new(main_text),
                u32::try_from(call + "util.".len()).expect("fixture offset"),
            ),
        );
        assert!(
            items.iter().any(|item| item.label == "answer"),
            "completion must include created module member: {items:?}"
        );
        let answer_offset = u32::try_from(call + "util.".len() + 2).expect("answer offset");
        let program = arandu_query::passes::parse(state.host.db(), main);
        let symbol = crate::ide::expr_symbol_at(
            program.as_ref().as_ref().expect("parsed entry"),
            tc,
            answer_offset,
        )
        .expect("symbol at imported member");
        let definition = arandu_query::passes::symbol_span(state.host.db(), symbol);
        assert_ne!(
            definition.file_id,
            *main.file_id(state.host.db()),
            "imported member must retain its definition file identity"
        );

        let helper_path = src.join("helper.aru");
        std::fs::rename(&util_path, &helper_path).expect("rename module fixture");
        let helper_uri = uri_from_path(&helper_path).expect("helper URI");
        state
            .rename_uri(&util_uri, &helper_uri)
            .expect("apply module rename");
        assert!(state.refresh_package_listing());
        assert!(!state.host.db().is_registered("editor_gold/util.aru"));
        assert!(!state.host.db().is_registered("util.aru"));
        assert!(
            arandu_query::passes::type_check(state.host.db(), main)
                .diagnostics
                .iter()
                .any(|diag| matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "renaming an imported module must invalidate its importers"
        );

        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn closed_document_id_is_stale() {
        let mut st = ServerState::new();
        let uri = file_url("c.aru");
        let id = st.open_or_commit(&uri, "func main() {}".into());
        assert!(st.docs.get(id).is_some());
        st.close_uri(&uri);
        assert!(st.docs.get(id).is_none());
        assert!(!st.by_uri.contains_key(uri.as_str()));
    }

    #[test]
    fn close_discards_pending_edit_without_reopening_document() {
        let mut st = ServerState::new();
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("closed-pending.aru");
        let id = st.open_or_commit(&uri, "func main() {}".into());
        st.queue_change(&uri, "func main() { let stale = 1; }".into());

        st.close_uri(&uri);

        assert!(st.flush_all().is_empty());
        assert!(st.docs.get(id).is_none());
        assert!(!st.by_uri.contains_key(uri.as_str()));
    }

    #[test]
    fn close_stales_overlay_and_restores_disk_source() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-close-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create close fixture");
        let path = root.join("close.aru");
        std::fs::write(&path, "func disk(): int { return 1 }").expect("write disk fixture");
        let uri = uri_from_path(&path).expect("file URI");
        let mut st = ServerState::new();
        let overlay = st.open_or_commit(&uri, "func overlay(): int { return 2 }".into());
        st.mark_open(&uri);
        {
            let snap = st.snapshot();
            let source = st.docs.get(overlay).expect("overlay document").source;
            assert!(
                crate::ide::workspace_symbols(
                    &snap,
                    &[crate::ide::DocSnap {
                        source,
                        path: Arc::new(path),
                        uri: uri.clone(),
                    }],
                    "overlay",
                )
                .iter()
                .any(|symbol| symbol.name == "overlay"),
                "workspace symbols must observe the open overlay"
            );
        }

        st.close_uri(&uri);

        assert!(
            st.docs.get(overlay).is_none(),
            "closed overlay ID must be stale"
        );
        let disk_id = st.by_uri[uri.as_str()];
        assert_ne!(disk_id, overlay);
        let disk = st.docs.get(disk_id).expect("known disk source");
        assert_eq!(
            disk.source.text(st.host.db()).as_ref(),
            "func disk(): int { return 1 }"
        );
        assert!(!st.is_open(&uri));
        std::fs::remove_dir_all(root).expect("remove close fixture");
    }

    #[test]
    fn delete_and_rename_never_reuse_file_identity() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-rename-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create rename fixture");
        let old_path = root.join("old.aru");
        let new_path = root.join("new.aru");
        std::fs::write(&old_path, "func value(): int { return 1 }").expect("write old");
        let old_uri = uri_from_path(&old_path).expect("old URI");
        let new_uri = uri_from_path(&new_path).expect("new URI");
        let mut st = ServerState::new();
        let old_doc = st.reload_uri_from_disk(&old_uri).expect("load old");
        let old_file = *st
            .docs
            .get(old_doc)
            .expect("old document")
            .source
            .file_id(st.host.db());
        std::fs::rename(&old_path, &new_path).expect("rename fixture");

        let new_doc = st.rename_uri(&old_uri, &new_uri).expect("apply rename");
        let new_file = *st
            .docs
            .get(new_doc)
            .expect("new document")
            .source
            .file_id(st.host.db());
        assert!(st.docs.get(old_doc).is_none());
        assert!(new_file > old_file, "FileId allocation must be monotonic");
        assert!(!st.host.db().is_registered(&registry_path_key(&old_path)));
        assert!(st.host.db().is_registered(&registry_path_key(&new_path)));

        st.remove_uri(&new_uri);
        assert!(st.docs.get(new_doc).is_none());
        assert!(!st.host.db().is_registered(&registry_path_key(&new_path)));
        std::fs::remove_dir_all(root).expect("remove rename fixture");
    }

    #[test]
    fn s2_endurance_batches_edits_and_discards_stale_snapshots() {
        const BATCHES: u64 = 100;
        const CHANGES_PER_BATCH: u64 = 20;

        let mut st = ServerState::new();
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("s2-endurance.aru");
        let id = st.open_or_commit(&uri, "func main(): int { return 0 }".into());
        let initial_revision = st.revision().as_u64();

        for batch in 1..=BATCHES {
            for change in 1..=CHANGES_PER_BATCH {
                let value = batch * CHANGES_PER_BATCH + change;
                st.queue_change(&uri, format!("func main(): int {{ return {value} }}"));
            }
            assert_eq!(
                st.vfs.pending_count(),
                1,
                "full-text changes for one document must coalesce"
            );

            let stale_revision = st.revision();
            let source = st.docs.get(id).expect("live document").source;
            let snapshot = st.snapshot();
            let worker = std::thread::spawn(move || {
                let diagnostics = arandu_query::file_ide_diagnostics(&snapshot.db, source);
                (snapshot.revision, diagnostics.len())
            });
            let (worker_revision, _) = worker.join().expect("snapshot worker must finish");
            assert_eq!(worker_revision, stale_revision);

            let committed = st.flush_all();
            assert_eq!(committed, vec![(uri.clone(), id)]);
            assert_ne!(st.revision(), stale_revision);
        }

        assert_eq!(
            st.revision().as_u64(),
            initial_revision + BATCHES,
            "2,000 on-type changes must become exactly 100 Salsa commits"
        );
        assert_eq!(st.docs.len(), 1, "edits must not leak document identities");
        assert_eq!(st.by_uri.len(), 1);
        assert_eq!(st.by_file_id.len(), 1);

        st.close_uri(&uri);
        assert!(st.docs.get(id).is_none(), "closed ID must remain stale");
        let reopened = st.open_or_commit(&uri, "func main(): int { return 7 }".into());
        assert_ne!(
            reopened, id,
            "reopen must allocate a new DocumentId generation"
        );
    }
}
