//! DX.6 / P4 — synchronous LSP: main + VFS debounce + worker pool + full IDE caps.
//!
//! Protocol: `lsp-server` + `lsp-types` (no async on the analysis path).
//!
//! This crate root only wires the phases together:
//! - [`capabilities`] owns the initialize handshake,
//! - [`workspace`] discovers packages/files in the background,
//! - [`dispatcher`] runs the event loop and job-result plumbing,
//! - [`handlers`] routes each protocol message to feature code,
//! - [`diagnostics`] computes and publishes diagnostics from snapshots.

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

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    // The VS Code extension validates the discovered server with --version
    // before starting the client, matching the rust-analyzer contract.
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("arandu-lsp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let (connection, io_threads) = Connection::stdio();
    run(connection)?;
    io_threads.join()?;
    Ok(())
}

fn run(connection: Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
    let initialized = capabilities::initialize_connection(&connection)?;
    let mut state = ServerState::new();
    let pool = WorkerPool::new(4)?;
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<JobResult>();
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
