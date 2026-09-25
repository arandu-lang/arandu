//! Server event loop and job-result plumbing.
//!
//! Owns the explicit `select_biased!` loop (protocol messages, worker job
//! results, workspace discovery events, VFS debounce deadline) and the typed
//! [`JobResult`] channel that carries outcomes from worker threads back to the
//! main thread. Stale results are dropped here: publication happens only when
//! the document is alive and the analysis revision still matches.
//!
//! Interactive scheduling lives here too: semantic requests are spawned at
//! [`Priority::Interactive`] with a per-request [`JobKey`] so clients can
//! cancel via `$/cancelRequest`, and saturation is answered with
//! `ServerCancelled` instead of unbounded backlog.

use crate::diagnostics::{publish_diagnostics, spawn_open_diagnostics};
use crate::handlers;
use crate::pool::{CancellationToken, JobKey, Priority, WorkerPool};
use crate::state::{DocInfo, ServerState};
use crate::workspace::WorkspaceEvent;
use arandu_query::{AnalysisRevision, AnalysisSnapshot, DocumentId};
use crossbeam_channel::{never, select_biased, Receiver, Sender};
use lsp_server::{Connection, Message, Notification, RequestId, Response};
use lsp_types::notification::{Notification as _, Progress};
use lsp_types::{ProgressParams, ProgressParamsValue, ProgressToken, WorkDoneProgress};
use rustc_hash::FxHashMap;
use std::error::Error;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

pub(crate) const WORKSPACE_PROGRESS_TOKEN: &str = "arandu-workspace-index";
pub(crate) const WORKSPACE_PROGRESS_REQUEST_ID: &str = "arandu-workspace-progress-create";

pub(crate) const LSP_CONTENT_MODIFIED: i32 = -32801;
pub(crate) const LSP_REQUEST_CANCELLED: i32 = -32800;
pub(crate) const LSP_SERVER_CANCELLED: i32 = -32802;
pub(crate) const JSON_RPC_INTERNAL_ERROR: i32 = -32603;
// Includes running jobs and results awaiting publication, unlike pool capacity.
const MAX_PENDING_REQUESTS: usize = 64;
/// Bound of the worker → event-loop result channel.
///
/// Producers far outnumber the single consumer under edit bursts; without a
/// bound the queue grows with every superseded result. Workers send with
/// blocking `send` (backpressure: the loop always drains), while the loop
/// itself never sends into this channel — it stages rejections in
/// [`ServerState::deferred_rejections`] instead.
pub(crate) const JOB_RESULT_CAPACITY: usize = MAX_PENDING_REQUESTS;

#[cfg(test)]
mod tests;

/// Outcomes sent from worker threads back to the main loop.
pub(crate) enum JobResult {
    WorkspaceReload(Result<Box<crate::workspace::WorkspaceProject>, String>),
    Diagnostics {
        uri: lsp_types::Uri,
        doc_id: DocumentId,
        version: Option<i32>,
        revision: AnalysisRevision,
        fingerprint: [u8; 32],
        diags: Vec<lsp_types::Diagnostic>,
    },
    JsonResponse {
        id: RequestId,
        revision: AnalysisRevision,
        value: serde_json::Value,
    },
    JsonError {
        id: RequestId,
        revision: AnalysisRevision,
        code: i32,
        message: String,
    },
    Failed {
        id: Option<RequestId>,
        revision: AnalysisRevision,
    },
    Cancelled {
        id: RequestId,
    },
    Rejected {
        id: RequestId,
    },
}

enum Event {
    Protocol(Result<Message, crossbeam_channel::RecvError>),
    Job(Result<JobResult, crossbeam_channel::RecvError>),
    Workspace(Result<WorkspaceEvent, crossbeam_channel::RecvError>),
    Timeout,
}

fn next_event(
    connection: &Connection,
    jobs: &Receiver<JobResult>,
    workspace: &Receiver<WorkspaceEvent>,
    requests_pending: bool,
    timeout: Duration,
) -> Event {
    // Keep discovery in its bounded producer channel until every admitted
    // request has delivered a terminal response. Worker retirement is too
    // early: its result may still be waiting in `jobs` at the old revision.
    let paused = never();
    let workspace = if requests_pending { &paused } else { workspace };
    select_biased! {
        recv(connection.receiver) -> message => Event::Protocol(message),
        recv(jobs) -> job => Event::Job(job),
        recv(workspace) -> event => Event::Workspace(event),
        default(timeout) => Event::Timeout,
    }
}

pub(crate) fn event_loop(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: Sender<JobResult>,
    job_rx: Receiver<JobResult>,
    mut workspace_rx: Receiver<WorkspaceEvent>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let mut workspace_progress_started = false;
    let mut workspace_done = false;
    loop {
        apply_deferred_reload(connection, state, pool, &job_tx)?;
        drain_deferred_rejections(connection, state, pool, &job_tx)?;
        let timeout = state
            .vfs
            .next_deadline()
            .unwrap_or(Duration::from_secs(3600));

        match next_event(
            connection,
            &job_rx,
            &workspace_rx,
            !state.pending_requests.is_empty(),
            timeout,
        ) {
            Event::Protocol(msg) => {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Request(req) => {
                        if connection.handle_shutdown(&req)? {
                            return Ok(());
                        }
                        let mut ctx = handlers::HandlerCtx {
                            connection,
                            state,
                            pool,
                            job_tx: &job_tx,
                        };
                        handlers::dispatch_request(&mut ctx, req)?;
                    }
                    Message::Notification(not) => {
                        let mut ctx = handlers::HandlerCtx {
                            connection,
                            state,
                            pool,
                            job_tx: &job_tx,
                        };
                        handlers::dispatch_notification(&mut ctx, not)?;
                    }
                    Message::Response(response)
                        if response.id
                            == RequestId::from(WORKSPACE_PROGRESS_REQUEST_ID.to_owned())
                            && response.response_result.is_ok() =>
                    {
                        send_workspace_progress(
                            connection,
                            WorkDoneProgress::Begin(lsp_types::WorkDoneProgressBegin {
                                title: "Indexing Arandu workspace".into(),
                                cancellable: Some(false),
                                message: Some("Discovering packages and source files".into()),
                                percentage: None,
                            }),
                        )?;
                        workspace_progress_started = true;
                        if workspace_done {
                            finish_workspace_progress(connection)?;
                            workspace_progress_started = false;
                        }
                    }
                    Message::Response(_) => {}
                }
            }
            Event::Job(job) => {
                if let Ok(job) = job {
                    handle_job_result(connection, state, pool, &job_tx, job)?;
                }
            }
            Event::Workspace(event) => match event {
                Ok(WorkspaceEvent::Stdlib(stdlib)) => {
                    for file in stdlib.files {
                        crate::workspace::register_workspace_file(state, file);
                    }
                    state.host.db().set_stdlib_root(stdlib.root);
                }
                Ok(WorkspaceEvent::Project(project)) => {
                    let mut project = *project;
                    for file in project.module_files.drain(..) {
                        crate::workspace::register_workspace_file(state, file);
                    }
                    state
                        .configure_package(project)
                        .map_err(std::io::Error::other)?;
                }
                Ok(WorkspaceEvent::File(file)) => {
                    crate::workspace::register_workspace_file(state, file);
                }
                Ok(WorkspaceEvent::NoManifest) => {
                    let message = if state.host.db().stdlib_root().is_some() {
                        "No arandu.toml found; analyzing files with the toolchain stdlib. Run `arandu_cli init` in a package folder to enable package imports and tests."
                    } else {
                        "No arandu.toml found; single-file analysis is available, but the toolchain stdlib could not be located. Set ARANDU_STDLIB to its directory."
                    };
                    send_server_status(connection, "single-file", message)?;
                }
                Ok(WorkspaceEvent::Error(error)) => {
                    send_server_status(connection, "error", &error)?;
                }
                Ok(WorkspaceEvent::Done) => {
                    spawn_open_diagnostics(state, pool, &job_tx);
                    workspace_done = true;
                    if workspace_progress_started {
                        finish_workspace_progress(connection)?;
                        workspace_progress_started = false;
                    }
                    let message = if state.host.db().stdlib_root().is_none() {
                        "Workspace indexed; toolchain stdlib unavailable"
                    } else if state.package.is_some() {
                        "Workspace ready"
                    } else {
                        "Single-file analysis ready; no arandu.toml found"
                    };
                    send_server_status(connection, "ready", message)?;
                    workspace_rx = never();
                }
                Err(_) => workspace_rx = never(),
            },
            Event::Timeout => {
                if state.vfs.has_pending() {
                    pool.cancel_requests();
                }
                let committed = state.flush_due();
                if !committed.is_empty() {
                    spawn_open_diagnostics(state, pool, &job_tx);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn send_workspace_progress(
    connection: &Connection,
    progress: WorkDoneProgress,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let params = ProgressParams {
        token: ProgressToken::String(WORKSPACE_PROGRESS_TOKEN.into()),
        value: ProgressParamsValue::WorkDone(progress),
    };
    connection
        .sender
        .send(Message::Notification(Notification::new(
            Progress::METHOD.into(),
            serde_json::to_value(params)?,
        )))?;
    Ok(())
}

pub(crate) fn send_server_status(
    connection: &Connection,
    state: &str,
    message: &str,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    connection
        .sender
        .send(Message::Notification(Notification::new(
            "arandu/status".into(),
            serde_json::json!({ "state": state, "message": message }),
        )))?;
    Ok(())
}

pub(crate) fn finish_workspace_progress(
    connection: &Connection,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    send_workspace_progress(
        connection,
        WorkDoneProgress::End(lsp_types::WorkDoneProgressEnd {
            message: Some("Workspace ready".into()),
        }),
    )
}

/// Makes pending VFS edits visible before a semantic request is scheduled.
///
/// If anything commits, diagnostics are re-spawned for those documents so a
/// request never analyzes a buffer older than the client's view.
pub(crate) fn flush_for_request(
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
) {
    if state.vfs.has_pending() {
        pool.cancel_requests();
    }
    let committed = state.flush_all();
    if !committed.is_empty() {
        spawn_open_diagnostics(state, pool, job_tx);
    }
}

/// Schedules goto-definition on an interactive worker.
///
/// Clones the URI/FileId maps up front; the closure only touches the captured
/// snapshot, never live server state.
pub(crate) fn spawn_goto(
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    uri: lsp_types::Uri,
    pos: lsp_types::Position,
) {
    if reject_saturated_request(state, &req_id) {
        return;
    }
    let snap = state.snapshot();
    let revision = snap.revision;
    let by_uri = state.by_uri.clone();
    let by_file_id = state.by_file_id.clone();
    let docs = state.doc_infos_by_id();
    let tx = job_tx.clone();
    let request_key = JobKey::Request(req_id.clone());
    let rejected_id = req_id.clone();
    if pool
        .spawn(
            Priority::Interactive,
            Some(request_key),
            move |cancellation| {
                cancellation.link_query(snap.db.query_cancellation_token());
                if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| {
                    arandu_query::catch_query_cancellation(|| {
                        let location = handlers::goto_on_snapshot(
                            &snap,
                            &by_uri,
                            &by_file_id,
                            &docs,
                            &uri,
                            pos,
                        );
                        match location {
                            Some(loc) => {
                                serde_json::to_value(lsp_types::GotoDefinitionResponse::Scalar(loc))
                                    .unwrap_or(serde_json::Value::Null)
                            }
                            None => serde_json::Value::Null,
                        }
                    })
                })) {
                    Ok(Ok(value)) => {
                        if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                            return;
                        }
                        let _ = tx.send(JobResult::JsonResponse {
                            id: req_id,
                            revision,
                            value,
                        });
                    }
                    Ok(Err(_)) => {
                        let _ = send_cancelled_if_needed(&tx, &req_id, &cancellation);
                    }
                    Err(payload) => {
                        crate::logging::log_panic("goto request", &payload);
                        let _ = tx.send(JobResult::Failed {
                            id: Some(req_id),
                            revision,
                        });
                    }
                }
            },
        )
        .is_err()
    {
        // Event-loop thread: stage instead of sending into the bounded
        // channel it must also drain (see `JOB_RESULT_CAPACITY`).
        state.deferred_rejections.push_back(rejected_id);
    } else {
        state.pending_requests.insert(rejected_id);
    }
}

/// Runs `f` over the snapshot on an interactive worker and replies with JSON.
pub(crate) fn spawn_json<F>(
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    f: F,
) where
    F: FnOnce(&AnalysisSnapshot, &FxHashMap<String, DocInfo>) -> serde_json::Value + Send + 'static,
{
    if reject_saturated_request(state, &req_id) {
        return;
    }
    let snap = state.snapshot();
    let revision = snap.revision;
    let docs = state.doc_info_map();
    let tx = job_tx.clone();
    let request_key = JobKey::Request(req_id.clone());
    let rejected_id = req_id.clone();
    if pool
        .spawn(
            Priority::Interactive,
            Some(request_key),
            move |cancellation| {
                cancellation.link_query(snap.db.query_cancellation_token());
                if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| {
                    arandu_query::catch_query_cancellation(|| f(&snap, &docs))
                })) {
                    Ok(Ok(value)) => {
                        if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                            return;
                        }
                        let _ = tx.send(JobResult::JsonResponse {
                            id: req_id,
                            revision,
                            value,
                        });
                    }
                    Ok(Err(_)) => {
                        let _ = send_cancelled_if_needed(&tx, &req_id, &cancellation);
                    }
                    Err(payload) => {
                        crate::logging::log_panic("interactive request", &payload);
                        let _ = tx.send(JobResult::Failed {
                            id: Some(req_id),
                            revision,
                        });
                    }
                }
            },
        )
        .is_err()
    {
        state.deferred_rejections.push_back(rejected_id);
    } else {
        state.pending_requests.insert(rejected_id);
    }
}

/// Like [`spawn_json`], but `f` may produce a protocol error response.
pub(crate) fn spawn_json_result<F>(
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    f: F,
) where
    F: FnOnce(
            &AnalysisSnapshot,
            &FxHashMap<String, DocInfo>,
        ) -> Result<serde_json::Value, (i32, String)>
        + Send
        + 'static,
{
    if reject_saturated_request(state, &req_id) {
        return;
    }
    let snap = state.snapshot();
    let revision = snap.revision;
    let docs = state.doc_info_map();
    let tx = job_tx.clone();
    let request_key = JobKey::Request(req_id.clone());
    let rejected_id = req_id.clone();
    if pool
        .spawn(
            Priority::Interactive,
            Some(request_key),
            move |cancellation| {
                cancellation.link_query(snap.db.query_cancellation_token());
                if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| {
                    arandu_query::catch_query_cancellation(|| f(&snap, &docs))
                })) {
                    Ok(Ok(Ok(value))) => {
                        if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                            return;
                        }
                        let _ = tx.send(JobResult::JsonResponse {
                            id: req_id,
                            revision,
                            value,
                        });
                    }
                    Ok(Ok(Err((code, message)))) => {
                        let _ = tx.send(JobResult::JsonError {
                            id: req_id,
                            revision,
                            code,
                            message,
                        });
                    }
                    Ok(Err(_)) => {
                        let _ = send_cancelled_if_needed(&tx, &req_id, &cancellation);
                    }
                    Err(payload) => {
                        crate::logging::log_panic("result request", &payload);
                        let _ = tx.send(JobResult::Failed {
                            id: Some(req_id),
                            revision,
                        });
                    }
                }
            },
        )
        .is_err()
    {
        state.deferred_rejections.push_back(rejected_id);
    } else {
        state.pending_requests.insert(rejected_id);
    }
}

/// On cancellation, reports `RequestCancelled` to the client and returns true.
pub(crate) fn send_cancelled_if_needed(
    tx: &Sender<JobResult>,
    id: &RequestId,
    cancellation: &CancellationToken,
) -> bool {
    if !cancellation.claim_cancelled() {
        return false;
    }
    let _ = tx.send(JobResult::Cancelled { id: id.clone() });
    true
}

fn reject_saturated_request(state: &mut ServerState, id: &RequestId) -> bool {
    if state.pending_requests.len() < MAX_PENDING_REQUESTS {
        return false;
    }
    // Event-loop thread: never send into the bounded job channel it drains
    // itself; the rejection is delivered by `drain_deferred_rejections`.
    state.deferred_rejections.push_back(id.clone());
    true
}

/// Delivers rejections staged by the event-loop thread.
///
/// Called at the top of every loop iteration so a saturated request still
/// receives its terminal response without the loop ever blocking on its own
/// bounded channel.
pub(crate) fn drain_deferred_rejections(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    while let Some(id) = state.deferred_rejections.pop_front() {
        handle_job_result(connection, state, pool, job_tx, JobResult::Rejected { id })?;
    }
    Ok(())
}

fn apply_deferred_reload(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    if state.pending_requests.is_empty() {
        if let Some(reload) = state.deferred_reload.take() {
            handle_job_result(
                connection,
                state,
                pool,
                job_tx,
                JobResult::WorkspaceReload(reload),
            )?;
        }
    }
    Ok(())
}

fn handle_job_result(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    job: JobResult,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let terminal = match &job {
        JobResult::JsonResponse { id, .. }
        | JobResult::JsonError { id, .. }
        | JobResult::Failed { id: Some(id), .. }
        | JobResult::Cancelled { id }
        | JobResult::Rejected { id } => Some(id),
        JobResult::WorkspaceReload(_)
        | JobResult::Diagnostics { .. }
        | JobResult::Failed { id: None, .. } => None,
    };
    if let Some(id) = terminal {
        let was_pending = state.pending_requests.remove(id);
        if !was_pending && !matches!(&job, JobResult::Rejected { .. }) {
            // An explicit $/cancelRequest may already have sent the terminal
            // response. Suppress success/error produced by the retiring worker.
            return Ok(());
        }
    }
    match job {
        JobResult::WorkspaceReload(reload) if !state.pending_requests.is_empty() => {
            // Background commits obey the same writer barrier as discovery.
            // Retain only the latest completed plan while requests are active.
            state.deferred_reload = Some(reload);
        }
        JobResult::WorkspaceReload(Ok(project)) => {
            let mut project = *project;
            for file in project.module_files.drain(..) {
                crate::workspace::register_workspace_file(state, file);
            }
            state
                .configure_package(project)
                .map_err(std::io::Error::other)?;
            spawn_open_diagnostics(state, pool, job_tx);
            send_server_status(connection, "ready", "Package graph refreshed")?;
        }
        JobResult::WorkspaceReload(Err(error)) => {
            send_server_status(connection, "error", &error)?;
        }
        JobResult::Diagnostics {
            uri,
            doc_id,
            version,
            revision,
            fingerprint,
            diags,
        } => {
            if state.docs.get(doc_id).is_none() {
                return Ok(());
            }
            if revision != state.revision() {
                return Ok(());
            }
            if state.last_diag_fp.get(&doc_id) == Some(&(fingerprint, version)) {
                return Ok(());
            }
            if version != state.version(doc_id) {
                return Ok(());
            }
            publish_diagnostics(connection, uri, diags, version)?;
            state.last_diag_fp.insert(doc_id, (fingerprint, version));
        }
        JobResult::JsonResponse {
            id,
            revision,
            value,
        } => {
            if revision != state.revision() {
                connection.sender.send(Message::Response(Response::new_err(
                    id,
                    LSP_CONTENT_MODIFIED,
                    "document changed while the request was running".into(),
                )))?;
                return Ok(());
            }
            connection
                .sender
                .send(Message::Response(Response::new_ok(id, value)))?;
        }
        JobResult::JsonError {
            id,
            revision,
            code,
            message,
        } => {
            let (code, message) = if revision == state.revision() {
                (code, message)
            } else {
                (
                    LSP_CONTENT_MODIFIED,
                    "document changed while the request was running".into(),
                )
            };
            connection
                .sender
                .send(Message::Response(Response::new_err(id, code, message)))?;
        }
        JobResult::Failed { id, revision } => {
            // The captured snapshot is dropped with the failed job. Never publish a
            // diagnostic or successful response from that analysis.
            if let Some(id) = id {
                let (code, message) = if revision == state.revision() {
                    (
                        JSON_RPC_INTERNAL_ERROR,
                        "analysis worker failed; request snapshot was discarded",
                    )
                } else {
                    (
                        LSP_CONTENT_MODIFIED,
                        "document changed while the failed request was running",
                    )
                };
                connection.sender.send(Message::Response(Response::new_err(
                    id,
                    code,
                    message.into(),
                )))?;
            }
        }
        JobResult::Cancelled { id } => {
            connection.sender.send(Message::Response(Response::new_err(
                id,
                LSP_REQUEST_CANCELLED,
                "request cancelled by client".into(),
            )))?;
        }
        JobResult::Rejected { id } => {
            connection.sender.send(Message::Response(Response::new_err(
                id,
                LSP_SERVER_CANCELLED,
                "interactive scheduler is saturated; retry the request".into(),
            )))?;
        }
    }
    Ok(())
}
