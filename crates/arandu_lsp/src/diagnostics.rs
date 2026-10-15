//! Diagnostic pipeline: computation on worker snapshots, staleness filtering
//! and publication to the client.
//!
//! Workers only analyze immutable snapshots; the main thread publishes results
//! and drops anything stale (dead document, old revision, unchanged
//! fingerprint, superseded version).

use crate::conv::span_to_range;
use crate::ide;
use crate::pool::{JobKey, Priority, WorkerPool};
use crate::state::ServerState;
use crate::uri_util::{parse_uri, uri_from_path};
use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, DocumentId, SourceFile};
use crossbeam_channel::Sender;
use lsp_server::{Connection, Message, Notification};
use lsp_types::notification::{Notification as _, PublishDiagnostics};
use lsp_types::{
    CodeDescription, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag,
    Location, NumberOrString, PublishDiagnosticsParams, Uri,
};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Spawns one background diagnostics job for an already-committed document.
pub(crate) fn spawn_diagnostics(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<crate::dispatcher::JobResult>,
    uri: Uri,
    doc_id: DocumentId,
) {
    // The latest client version advances before its debounced text reaches
    // Salsa. A workspace refresh must not label the old snapshot with it.
    if state.vfs.pending_text(uri.as_str()).is_some() {
        return;
    }
    let Some(doc) = state.docs.get(doc_id) else {
        return;
    };
    let source = doc.source;
    let version = state.version(doc_id);
    let snap = state.snapshot();
    let revision = snap.revision;
    let tx = job_tx.clone();
    let _ = pool.spawn(
        Priority::Background,
        Some(JobKey::Diagnostics(doc_id)),
        move |cancellation| {
            cancellation.link_query(snap.db.query_cancellation_token());
            if cancellation.is_cancelled() {
                return;
            }
            match catch_unwind(AssertUnwindSafe(|| {
                arandu_query::catch_query_cancellation(|| compute_diagnostics(&snap, source))
            })) {
                Ok(Ok((diags, fingerprint))) => {
                    if cancellation.is_cancelled() {
                        return;
                    }
                    let _ = tx.send(crate::dispatcher::JobResult::Diagnostics {
                        uri,
                        doc_id,
                        version,
                        revision,
                        fingerprint,
                        diags,
                    });
                }
                Ok(Err(_)) => {}
                Err(payload) => {
                    crate::logging::log_panic("diagnostics", &payload);
                    let _ = tx.send(crate::dispatcher::JobResult::Failed { id: None, revision });
                }
            }
        },
    );
}

/// Re-runs diagnostics for every open document (after workspace events).
pub(crate) fn spawn_open_diagnostics(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<crate::dispatcher::JobResult>,
) {
    use crate::uri_util::parse_uri as parse;
    let open: Vec<_> = state
        .open_uris
        .iter()
        .filter_map(|uri| {
            let parsed = parse(uri)?;
            let id = state.by_uri.get(uri).copied()?;
            Some((parsed, id))
        })
        .collect();
    for (uri, id) in open {
        spawn_diagnostics(state, pool, job_tx, uri, id);
    }
}

pub(crate) fn compute_diagnostics(
    snap: &AnalysisSnapshot,
    source: SourceFile,
) -> (Vec<Diagnostic>, [u8; 32]) {
    let text = source.text(&snap.db);
    let index = LineIndex::new(text);
    let source_uri = uri_from_path(source.path(&snap.db).as_ref());
    let ide_diags = arandu_query::file_ide_diagnostics(&snap.db, source);
    let fp = arandu_query::ide_diags_fingerprint(ide_diags);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(fp.as_bytes());
    let diags = ide_diags
        .iter()
        .map(|d| {
            let span = arandu_base::Span::new(d.file_id, d.start, d.end);
            let primary_location = diagnostic_location(snap, span).or_else(|| {
                source_uri.clone().map(|uri| Location {
                    uri,
                    range: span_to_range(&index, span),
                })
            });
            let range = primary_location
                .as_ref()
                .map_or_else(lsp_types::Range::default, |location| location.range);
            let severity = match d.severity {
                0 => DiagnosticSeverity::ERROR,
                1 => DiagnosticSeverity::WARNING,
                2 => DiagnosticSeverity::INFORMATION,
                _ => DiagnosticSeverity::HINT,
            };
            let related_information: Vec<_> = d
                .labels
                .iter()
                .filter_map(|label| {
                    diagnostic_location(
                        snap,
                        arandu_base::Span::new(label.file_id, label.start, label.end),
                    )
                    .map(|location| DiagnosticRelatedInformation {
                        location,
                        message: label.message.clone(),
                    })
                })
                .collect();
            let fixes = d
                .hints
                .iter()
                .filter_map(|hint| {
                    let replacement = hint.replacement.as_ref()?;
                    let location = diagnostic_location(
                        snap,
                        arandu_base::Span::new(
                            replacement.file_id,
                            replacement.start,
                            replacement.end,
                        ),
                    )?;
                    Some(ide::DiagnosticFixData {
                        title: hint.message.clone(),
                        uri: location.uri,
                        range: location.range,
                        new_text: replacement.new_text.clone(),
                    })
                })
                .collect();
            let data = ide::DiagnosticData {
                primary_label: d.primary_label.as_deref().map(str::to_owned),
                notes: d.notes.clone(),
                hints: d.hints.iter().map(|hint| hint.message.clone()).collect(),
                fixes,
            };
            let mut message = d.message.clone();
            if let Some(primary_label) = d.primary_label.as_deref() {
                message.push_str("\n\n");
                message.push_str(primary_label);
            }
            for note in &d.notes {
                message.push_str("\n\nnote: ");
                message.push_str(note);
            }
            for hint in &d.hints {
                message.push_str("\n\nhint: ");
                message.push_str(&hint.message);
            }
            let code_description = (!d.code.starts_with("ICE"))
                .then(|| {
                    parse_uri(&format!(
                        "https://github.com/arandu-lang/arandu/blob/main/docs/errors/{}.md",
                        d.code
                    ))
                    .map(|href| CodeDescription { href })
                })
                .flatten();
            let tags = matches!(d.code.as_str(), "W001" | "W002" | "W003" | "W005" | "W007")
                .then(|| vec![DiagnosticTag::UNNECESSARY]);
            Diagnostic {
                range,
                severity: Some(severity),
                code: Some(NumberOrString::String(d.code.clone())),
                code_description,
                message,
                source: Some("arandu".into()),
                related_information: (!related_information.is_empty())
                    .then_some(related_information),
                tags,
                data: serde_json::to_value(data).ok(),
            }
        })
        .collect();
    (diags, bytes)
}

fn diagnostic_location(snap: &AnalysisSnapshot, span: arandu_base::Span) -> Option<Location> {
    let source = snap.db.source_file_by_id(span.file_id)?;
    let uri = uri_from_path(source.path(&snap.db).as_ref())?;
    let index = LineIndex::new(source.text(&snap.db));
    Some(Location {
        uri,
        range: span_to_range(&index, span),
    })
}

pub(crate) fn publish_diagnostics(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    let not = Notification::new(
        PublishDiagnostics::METHOD.to_string(),
        serde_json::to_value(params)?,
    );
    connection.sender.send(Message::Notification(not))?;
    Ok(())
}
