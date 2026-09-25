//! Document synchronization and file-event notification handlers.
//!
//! Ordering rules preserved from the monolith: every mutation path cancels
//! in-flight requests before touching state (`pool.cancel_requests()`), open
//! documents publish diagnostics immediately, and workspace-wide events refresh
//! the package listing then re-run diagnostics for open buffers.

use super::HandlerCtx;
use crate::conv::apply_lsp_range_edit;
use crate::diagnostics::{publish_diagnostics, spawn_diagnostics, spawn_open_diagnostics};
use crate::uri_util::{parse_uri, path_from_uri};
use lsp_server::Notification;
use lsp_types::notification::{
    Cancel, DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidCreateFiles,
    DidDeleteFiles, DidOpenTextDocument, DidRenameFiles, DidSaveTextDocument, Notification as _,
};
use lsp_types::{CancelParams, FileChangeType, NumberOrString};

pub(super) fn handle(
    ctx: &mut HandlerCtx<'_>,
    not: Notification,
) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
    let connection = ctx.connection;
    let state = &mut *ctx.state;
    let pool = ctx.pool;
    let job_tx = ctx.job_tx;

    match not.method.as_str() {
        "arandu/testCrash" if std::env::var_os("ARANDU_LSP_TEST_ALLOW_CRASH").is_some() => {
            std::process::exit(86);
        }
        Cancel::METHOD => {
            let params: CancelParams = not.extract(Cancel::METHOD)?;
            let id = match params.id {
                NumberOrString::Number(id) => lsp_server::RequestId::from(id),
                NumberOrString::String(id) => lsp_server::RequestId::from(id),
            };
            if pool.cancel_for_response(&crate::pool::JobKey::Request(id.clone())) {
                state.pending_requests.remove(&id);
                connection.sender.send(lsp_server::Message::Response(
                    lsp_server::Response::new_err(
                        id,
                        crate::dispatcher::LSP_REQUEST_CANCELLED,
                        "request cancelled by client".into(),
                    ),
                ))?;
            }
        }
        DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams =
                not.extract(DidOpenTextDocument::METHOD)?;
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            pool.cancel_requests();
            let id = state.open_or_commit(&uri, params.text_document.text);
            state.mark_open(&uri);
            state.set_version(id, version);
            spawn_open_diagnostics(state, pool, job_tx);
        }
        DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                not.extract(DidChangeTextDocument::METHOD)?;
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            // Incremental: apply each range change onto the current buffer text.
            let mut text = state.text_for_change(&uri);
            for change in params.content_changes {
                if let Some(range) = change.range {
                    text = apply_lsp_range_edit(&text, range, &change.text);
                } else {
                    // Full replace (clients may still send this).
                    text = change.text;
                }
            }
            let id = state
                .by_uri
                .get(uri.as_str())
                .copied()
                .unwrap_or_else(|| state.open_or_commit(&uri, text.clone()));
            state.set_version(id, version);
            state.queue_change(&uri, text);
        }
        DidSaveTextDocument::METHOD => {
            let params: lsp_types::DidSaveTextDocumentParams =
                not.extract(DidSaveTextDocument::METHOD)?;
            if let Some(text) = params.text {
                state.queue_change(&params.text_document.uri, text);
            }
            if state.vfs.has_pending() {
                pool.cancel_requests();
            }
            let committed = state.flush_all();
            if committed.is_empty() {
                if let Some(&id) = state.by_uri.get(params.text_document.uri.as_str()) {
                    spawn_diagnostics(state, pool, job_tx, params.text_document.uri, id);
                }
            } else {
                spawn_open_diagnostics(state, pool, job_tx);
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                not.extract(DidCloseTextDocument::METHOD)?;
            let uri = params.text_document.uri;
            // Closing restores disk contents or unregisters the source, both
            // of which mutate Salsa state. Retire snapshot-based requests
            // before that write so stale workers cannot hold the writer barrier.
            pool.cancel_requests();
            state.close_uri(&uri);
            publish_diagnostics(connection, uri, Vec::new(), None)?;
            spawn_open_diagnostics(state, pool, job_tx);
        }
        DidCreateFiles::METHOD => {
            let params: lsp_types::CreateFilesParams = not.extract(DidCreateFiles::METHOD)?;
            pool.cancel_requests();
            let manifest_changed = params.files.iter().any(|file| {
                parse_uri(&file.uri).is_some_and(|uri| is_manifest_path(&path_from_uri(&uri)))
            });
            for file in params.files {
                let Some(uri) = parse_uri(&file.uri) else {
                    continue;
                };
                let _ = state.reload_uri_from_disk(&uri);
            }
            refresh_workspace_after_file_event(state, pool, job_tx, manifest_changed);
        }
        DidRenameFiles::METHOD => {
            let params: lsp_types::RenameFilesParams = not.extract(DidRenameFiles::METHOD)?;
            pool.cancel_requests();
            let manifest_changed = params.files.iter().any(|file| {
                [file.old_uri.as_str(), file.new_uri.as_str()]
                    .into_iter()
                    .filter_map(parse_uri)
                    .any(|uri| is_manifest_path(&path_from_uri(&uri)))
            });
            for file in params.files {
                let (Some(old_uri), Some(new_uri)) =
                    (parse_uri(&file.old_uri), parse_uri(&file.new_uri))
                else {
                    continue;
                };
                let was_open = state.is_open(&old_uri);
                let renamed = state.rename_uri(&old_uri, &new_uri);
                publish_diagnostics(connection, old_uri, Vec::new(), None)?;
                if was_open {
                    if let Some(id) = renamed {
                        spawn_diagnostics(state, pool, job_tx, new_uri, id);
                    }
                }
            }
            refresh_workspace_after_file_event(state, pool, job_tx, manifest_changed);
        }
        DidDeleteFiles::METHOD => {
            let params: lsp_types::DeleteFilesParams = not.extract(DidDeleteFiles::METHOD)?;
            pool.cancel_requests();
            let manifest_changed = params.files.iter().any(|file| {
                parse_uri(&file.uri).is_some_and(|uri| is_manifest_path(&path_from_uri(&uri)))
            });
            for file in params.files {
                let Some(uri) = parse_uri(&file.uri) else {
                    continue;
                };
                state.remove_uri(&uri);
                publish_diagnostics(connection, uri, Vec::new(), None)?;
            }
            refresh_workspace_after_file_event(state, pool, job_tx, manifest_changed);
        }
        DidChangeWatchedFiles::METHOD => {
            let params: lsp_types::DidChangeWatchedFilesParams =
                not.extract(DidChangeWatchedFiles::METHOD)?;
            pool.cancel_requests();
            let manifest_changed = params
                .changes
                .iter()
                .any(|change| is_manifest_path(&path_from_uri(&change.uri)));
            for change in params.changes {
                if change.typ == FileChangeType::DELETED {
                    state.remove_uri(&change.uri);
                    publish_diagnostics(connection, change.uri, Vec::new(), None)?;
                } else {
                    let _ = state.reload_uri_from_disk(&change.uri);
                }
            }
            refresh_workspace_after_file_event(state, pool, job_tx, manifest_changed);
        }
        _ => {}
    }
    Ok(())
}

fn is_manifest_path(path: &std::path::Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(arandu_query::MANIFEST_FILENAME | arandu_query::LEGACY_MANIFEST_FILENAME)
    )
}

fn refresh_workspace_after_file_event(
    state: &mut crate::state::ServerState,
    pool: &crate::pool::WorkerPool,
    job_tx: &crossbeam_channel::Sender<crate::dispatcher::JobResult>,
    manifest_changed: bool,
) {
    if manifest_changed {
        if let Some(manifest_path) = state.package_manifest_path() {
            crate::workspace::spawn_package_reload(pool, job_tx.clone(), manifest_path);
        }
    } else {
        state.refresh_package_listing();
        spawn_open_diagnostics(state, pool, job_tx);
    }
}
