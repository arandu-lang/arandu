//! LSP server runtime shared by the stdio executable and protocol fuzzing.
//!
//! The connection is a pair of message channels, so callers can exercise the
//! exact dispatcher and worker pool with either real stdio or an in-memory LSP
//! client.

mod capabilities;
mod conv;
mod diagnostics;
mod dispatcher;
mod handlers;
mod ide;
mod logging;
mod pool;
mod state;
mod uri_util;
mod vfs;
mod workspace;

use dispatcher::JobResult;
use lsp_server::{Connection, Message, Request, RequestId};
use pool::WorkerPool;
use state::ServerState;
use std::error::Error;

/// Runs the LSP protocol and workspace dispatcher on `connection` until exit.
pub fn run(connection: Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
    let initialized = capabilities::initialize_connection(&connection)?;
    let mut state = ServerState::new();
    let pool = WorkerPool::new(4)?;
    let (job_tx, job_rx) = crossbeam_channel::bounded::<JobResult>(dispatcher::JOB_RESULT_CAPACITY);
    if initialized.work_done_progress {
        connection.sender.send(Message::Request(Request::new(
            RequestId::from(dispatcher::WORKSPACE_PROGRESS_REQUEST_ID.to_owned()),
            "window/workDoneProgress/create".into(),
            serde_json::json!({ "token": dispatcher::WORKSPACE_PROGRESS_TOKEN }),
        )))?;
    }
    dispatcher::send_server_status(&connection, "indexing", "Indexing workspace")?;
    let workspace_rx = workspace::spawn_workspace_discovery(&pool, initialized.workspace_roots);
    dispatcher::event_loop(&connection, &mut state, &pool, job_tx, job_rx, workspace_rx)?;
    // Close lsp-server's sender before the stdio owner joins its writer thread.
    drop(connection);
    Ok(())
}
