//! In-memory LSP protocol sessions shared by libFuzzer and the regression runner.

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MAX_INPUT_BYTES: usize = 4096;
const MAX_EDITS: usize = 24;
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(8);
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const SHRINK_ATTEMPT_BUDGET: usize = 6;
static NEXT_SESSION_WORKSPACE_ID: AtomicU64 = AtomicU64::new(0);

struct SessionDeadline(Instant);

impl SessionDeadline {
    fn new() -> Self {
        Self(Instant::now())
    }

    fn next_wait(&self) -> Duration {
        SESSION_TIMEOUT
            .saturating_sub(self.0.elapsed())
            .min(MESSAGE_TIMEOUT)
    }
}

struct SessionWorkspace {
    root_uri: String,
    manifest_uri: String,
    manifest_path: PathBuf,
    document_paths: [PathBuf; 3],
    document_uris: [String; 3],
    _temp_dir: SessionTempDir,
}

struct SessionTempDir(PathBuf);

impl Drop for SessionTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum ServerExit {
    Clean,
    Error(String),
    Panicked(String),
}

pub(super) fn run(data: &[u8]) {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    run_with_sequence_reduction(data, run_session_once);
}

fn run_with_sequence_reduction(data: &[u8], mut verify: impl FnMut(&[u8]) -> Result<(), String>) {
    let Err(original_failure) = verify(data) else {
        return;
    };
    // A failed teardown is an infrastructure failure. Retrying candidates
    // would spawn more server workers while the previous one may still live.
    if original_failure.starts_with("LSP server did not stop cleanly:") {
        panic!("LSP session fuzz failure: {original_failure}; shrinking skipped");
    }
    let original_class = failure_class(&original_failure).to_owned();
    let reduced = crate::shrinker::shrink_byte_sequence_with_budget(
        data,
        SHRINK_ATTEMPT_BUDGET,
        |candidate| {
            verify(candidate).is_err_and(|failure| failure_class(&failure) == original_class)
        },
    );
    let shrink_confirmed = reduced.reproduced
        && verify(&reduced.sequence)
            .is_err_and(|failure| failure_class(&failure) == original_class);
    let (sequence_label, sequence) = if shrink_confirmed {
        ("minimized_sequence_hex", reduced.sequence.as_slice())
    } else {
        ("original_sequence_hex", data)
    };
    let sequence_hex = sequence
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    panic!(
        "LSP session fuzz failure: {original_failure}; {sequence_label}={sequence_hex}; shrink_reproduced={}; shrink_confirmed={shrink_confirmed}; shrink_attempts={}; shrink_reductions={}",
        reduced.reproduced,
        reduced.attempts,
        reduced.reductions
    );
}

fn failure_class(message: &str) -> &str {
    if message.starts_with("LSP server did not stop cleanly:") {
        return message;
    }
    message
        .lines()
        .next()
        .unwrap_or(message)
        .split_once(": ")
        .map_or(message.lines().next().unwrap_or(message), |(class, _)| {
            class
        })
}

fn run_session_once(data: &[u8]) -> Result<(), String> {
    let (server, client) = Connection::memory();
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| arandu_lsp::run(server)));
        let exit = match result {
            Ok(Ok(())) => ServerExit::Clean,
            Ok(Err(error)) => ServerExit::Error(error.to_string()),
            Err(payload) => ServerExit::Panicked(panic_payload_message(payload)),
        };
        let _ = finished_tx.send(exit);
    });
    let protocol_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_protocol(&client, data)));

    if protocol_result.is_err() {
        let _ = client.sender.send(Message::Request(Request::new(
            RequestId::from(2),
            "shutdown".to_owned(),
            Value::Null,
        )));
    }
    let _ = client.sender.send(Message::Notification(Notification::new(
        "exit".to_owned(),
        Value::Null,
    )));
    drop(client);

    let server_exit = match finished_rx.recv_timeout(SERVER_SHUTDOWN_TIMEOUT) {
        Ok(exit) => exit,
        Err(error) => {
            // Rust cannot cancel a stuck worker thread. End this fuzz process
            // instead of retrying and accumulating additional LSP workers.
            drop(server_thread);
            return Err(format!(
                "LSP server did not stop cleanly: {error:?}; joined=false"
            ));
        }
    };
    let server_joined = server_thread.join().is_ok();
    if !server_joined {
        return Err("LSP server did not stop cleanly: joined=false".to_owned());
    }
    combine_session_outcome(server_exit, protocol_result)
}

fn server_exit_failure(exit: ServerExit) -> Option<String> {
    match exit {
        ServerExit::Clean => None,
        ServerExit::Error(error) => Some(format!("LSP server returned error: {error}")),
        ServerExit::Panicked(message) => Some(format!("LSP server panicked: {message}")),
    }
}

fn combine_session_outcome(
    server_exit: ServerExit,
    protocol_result: Result<(), Box<dyn std::any::Any + Send>>,
) -> Result<(), String> {
    let protocol_failure = protocol_result.err().map(panic_payload_message);
    let server_failure = server_exit_failure(server_exit);
    match (protocol_failure, server_failure) {
        (None, None) => Ok(()),
        (Some(protocol), None) => Err(protocol),
        (None, Some(server)) => Err(server),
        (Some(protocol), Some(server)) => {
            Err(format!("{protocol}; server teardown also failed: {server}"))
        }
    }
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("unknown panic while checking LSP session")
        .to_owned()
}

fn run_protocol(client: &Connection, data: &[u8]) {
    let deadline = SessionDeadline::new();
    let workspace = session_workspace();
    let mut package_name = "s2_small".to_owned();
    let mut current_texts = [
        main_text(&package_name, 0, 1),
        helper_text(&package_name, 0, 1),
        data_text(&package_name, 0, 1),
    ];

    let mut messages = Vec::new();

    send_request(
        client,
        1,
        "initialize",
        json!({
            "processId": null,
            "capabilities": { "general": { "positionEncodings": ["utf-16"] } },
            "workspaceFolders": [{ "uri": workspace.root_uri, "name": "arandusmith-session" }]
        }),
    );
    wait_response(client, &deadline, RequestId::from(1), false, &mut messages);
    send_notification(client, "initialized", json!({}));
    wait_workspace_ready(client, &deadline, &mut messages);
    for (uri, text) in workspace.document_uris.iter().zip(&current_texts) {
        send_notification(
            client,
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "arandu",
                    "version": 1,
                    "text": text
                }
            }),
        );
    }

    let mut versions = [1_i32; 3];
    let mut next_request_id = 100_i32;
    let mut completion_ids = BTreeSet::new();
    let mut hover_ids = BTreeSet::new();
    let mut final_completion_docs = BTreeMap::new();
    let mut completed_interactive_probe = false;
    let mut manifest_edited = false;
    for byte in data.iter().copied().take(MAX_EDITS) {
        if byte & 0x20 != 0 && !manifest_edited {
            package_name = "s2_renamed".to_owned();
            apply_package_structure(
                client,
                &deadline,
                &workspace,
                &package_name,
                &mut current_texts,
                &mut versions,
                &mut messages,
            );
            manifest_edited = true;
        }
        let document_index = usize::from((byte >> 1) % 3);
        versions[document_index] += 1;
        current_texts[document_index] = edit_text(
            byte,
            versions[document_index],
            document_index,
            &package_name,
        );
        send_notification(
            client,
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": workspace.document_uris[document_index],
                    "version": versions[document_index]
                },
                "contentChanges": [{
                    "text": current_texts[document_index]
                }]
            }),
        );

        // Hover runs against every edited revision, including transiently
        // incomplete syntax. Keep the requests outstanding while later edits
        // arrive so the session also exercises stale interactive work.
        let hover_id = next_request_id;
        next_request_id += 1;
        hover_ids.insert(hover_id);
        send_request(
            client,
            hover_id,
            "textDocument/hover",
            hover_params(
                &workspace.document_uris[document_index],
                &current_texts[document_index],
            ),
        );

        if byte & 1 == 0
            && (byte & 0x80 != 0 || (document_index == 1 && !completed_interactive_probe))
        {
            let id = next_request_id;
            next_request_id += 1;
            completion_ids.insert(id);
            send_request(
                client,
                id,
                "textDocument/completion",
                completion_params(
                    &workspace.document_uris[document_index],
                    document_index,
                    &current_texts[document_index],
                ),
            );
            if byte & 0x80 != 0 {
                send_notification(client, "$/cancelRequest", json!({ "id": id }));
            } else if document_index == 1 && !completed_interactive_probe {
                wait_response(client, &deadline, RequestId::from(id), false, &mut messages);
                completion_ids.remove(&id);
                completed_interactive_probe = true;
            }
        }

        if byte & 0x40 != 0 {
            let uri = &workspace.document_uris[document_index];
            send_notification(
                client,
                "textDocument/didClose",
                json!({ "textDocument": { "uri": uri } }),
            );
            // Keep versions unique across close/reopen cycles. Reusing a version
            // lets a diagnostic from the previous document generation masquerade
            // as the final result for the reopened buffer.
            send_notification(
                client,
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "arandu",
                        "version": versions[document_index],
                        "text": current_texts[document_index]
                    }
                }),
            );
        }
    }

    if package_name != "s2_small" {
        package_name = "s2_small".to_owned();
        apply_package_structure(
            client,
            &deadline,
            &workspace,
            &package_name,
            &mut current_texts,
            &mut versions,
            &mut messages,
        );
    }

    for (index, uri) in workspace.document_uris.iter().enumerate() {
        versions[index] += 1;
        let text = match index {
            0 => main_text(&package_name, 0, 42),
            1 => helper_text(&package_name, 0, 42),
            _ => data_text(&package_name, 0, 42),
        };
        current_texts[index] = text.clone();
        send_notification(
            client,
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": versions[index] },
                "contentChanges": [{ "text": text }]
            }),
        );
        send_notification(
            client,
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    // Final requests must start only after every final edit/save has committed;
    // a later save may legitimately cancel requests tied to an older revision.
    for (index, uri) in workspace.document_uris.iter().enumerate() {
        let completion_id = next_request_id;
        next_request_id += 1;
        completion_ids.insert(completion_id);
        final_completion_docs.insert(completion_id, index);
        send_request(
            client,
            completion_id,
            "textDocument/completion",
            completion_params(uri, index, &current_texts[index]),
        );
    }

    let mut pending = completion_ids;
    pending.extend(hover_ids);
    // A completion probe can receive hover responses while waiting for its own
    // response. Those messages are already recorded and must not remain marked
    // pending after the session switches to its final drain loop.
    let already_responded: BTreeSet<_> = messages
        .iter()
        .filter(|message| message.get("result").is_some() || message.get("error").is_some())
        .filter_map(|message| {
            message
                .get("id")
                .and_then(Value::as_i64)
                .and_then(|id| i32::try_from(id).ok())
        })
        .collect();
    pending.retain(|id| !already_responded.contains(id));
    let mut saw_final_diagnostics = [false; 3];
    while !pending.is_empty() || !saw_final_diagnostics.iter().all(|seen| *seen) {
        let message = client
            .receiver
            .recv_timeout(deadline.next_wait())
            .unwrap_or_else(|error| {
                let diagnostics = messages
                    .iter()
                    .filter(|message| {
                        message.get("method").and_then(Value::as_str)
                            == Some("textDocument/publishDiagnostics")
                    })
                    .map(|message| {
                        (
                            message.pointer("/params/uri").and_then(Value::as_str),
                            message.pointer("/params/version").cloned(),
                            message
                                .pointer("/params/diagnostics")
                                .and_then(Value::as_array)
                                .map(Vec::len),
                        )
                    })
                    .collect::<Vec<_>>();
                panic!(
                    "LSP session timed out ({error:?}); pending completions: {pending:?}, final diagnostics: {saw_final_diagnostics:?}, observed diagnostics: {diagnostics:?}"
                )
            });
        match message {
            Message::Response(response) => {
                let request_id = request_id_as_i32(&response.id);
                if pending.remove(&request_id) {
                    if let Some(document_index) = final_completion_docs.remove(&request_id) {
                        assert_final_completion(
                            &response,
                            document_index,
                            &current_texts[document_index],
                            &messages,
                        );
                    } else {
                        assert_response_ok_or_cancelled(&response);
                    }
                }
                messages.push(serde_json::to_value(response).expect("serialize response"));
            }
            Message::Notification(notification) => {
                for (index, uri) in workspace.document_uris.iter().enumerate() {
                    saw_final_diagnostics[index] |=
                        is_final_diagnostics(&notification, uri, versions[index]);
                }
                messages.push(serde_json::to_value(notification).expect("serialize notification"));
            }
            Message::Request(request) => respond_to_server_request(client, request),
        }
    }

    send_request(client, 2, "shutdown", Value::Null);
    wait_response(client, &deadline, RequestId::from(2), false, &mut messages);
    messages.extend(
        client.receiver.try_iter().map(|message| {
            serde_json::to_value(message).expect("serialize queued protocol message")
        }),
    );

    assert_final_diagnostics(&messages, versions, &workspace.document_uris);
}

fn session_workspace() -> SessionWorkspace {
    let workspace_id = NEXT_SESSION_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
    let package_root = std::env::temp_dir().join(format!(
        "arandu-smith-lsp-{}-{workspace_id}",
        std::process::id()
    ));
    let temp_dir = SessionTempDir(package_root.clone());
    let source_root = package_root.join("src");
    let manifest_path = package_root.join("Arandu.toml");
    fs::create_dir_all(&source_root).expect("create LSP fuzz package source root");
    fs::write(
        &manifest_path,
        "name = \"s2_small\"\nversion = \"0.0.1\"\nentry = \"src/main.aru\"\n",
    )
    .expect("write LSP fuzz package manifest");
    let document_paths = [
        source_root.join("main.aru"),
        source_root.join("math.aru"),
        source_root.join("data.aru"),
    ];
    fs::write(&document_paths[0], main_text("s2_small", 0, 1)).expect("write LSP fuzz main module");
    fs::write(&document_paths[1], helper_text("s2_small", 0, 1))
        .expect("write LSP fuzz math module");
    fs::write(&document_paths[2], data_text("s2_small", 0, 1)).expect("write LSP fuzz data module");
    SessionWorkspace {
        root_uri: path_uri(&package_root),
        manifest_uri: path_uri(&manifest_path),
        manifest_path,
        document_paths,
        document_uris: [
            path_uri(&source_root.join("main.aru")),
            path_uri(&source_root.join("math.aru")),
            path_uri(&source_root.join("data.aru")),
        ],
        _temp_dir: temp_dir,
    }
}

fn apply_package_structure(
    client: &Connection,
    deadline: &SessionDeadline,
    workspace: &SessionWorkspace,
    package_name: &str,
    current_texts: &mut [String; 3],
    versions: &mut [i32; 3],
    messages: &mut Vec<Value>,
) {
    let texts = [
        main_text(package_name, 0, 42),
        helper_text(package_name, 0, 42),
        data_text(package_name, 0, 42),
    ];
    for (index, ((uri, path), text)) in workspace
        .document_uris
        .iter()
        .zip(&workspace.document_paths)
        .zip(texts)
        .enumerate()
    {
        fs::write(path, &text).expect("write package-renamed LSP source");
        versions[index] += 1;
        current_texts[index] = text.clone();
        send_notification(
            client,
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": versions[index] },
                "contentChanges": [{ "text": text }]
            }),
        );
        send_notification(
            client,
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        );
    }
    fs::write(
        &workspace.manifest_path,
        format!("name = \"{package_name}\"\nversion = \"0.0.1\"\nentry = \"src/main.aru\"\n"),
    )
    .expect("write package-renamed LSP manifest");
    send_notification(
        client,
        "workspace/didChangeWatchedFiles",
        json!({
            "changes": [{ "uri": workspace.manifest_uri, "type": 2 }]
        }),
    );
    wait_package_reload(client, deadline, messages);
}

fn wait_package_reload(client: &Connection, deadline: &SessionDeadline, messages: &mut Vec<Value>) {
    loop {
        match receive(client, deadline) {
            Message::Notification(notification) if notification.method == "arandu/status" => {
                let state = notification
                    .params
                    .get("state")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let message = notification
                    .params
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                messages.push(serde_json::to_value(notification).expect("serialize status"));
                if state.as_deref() == Some("error") {
                    panic!("package manifest reload failed: {message:?}");
                }
                if state.as_deref() == Some("ready")
                    && message.as_deref() == Some("Package graph refreshed")
                {
                    return;
                }
            }
            Message::Notification(notification) => {
                messages.push(serde_json::to_value(notification).expect("serialize notification"));
            }
            Message::Response(response) => {
                messages.push(serde_json::to_value(response).expect("serialize response"));
            }
            Message::Request(request) => respond_to_server_request(client, request),
        }
    }
}

fn path_uri(path: &std::path::Path) -> String {
    let path_text = path.to_string_lossy();
    #[cfg(windows)]
    let normalized = path_text.strip_prefix(r"\\?\UNC\").map_or_else(
        || {
            path_text
                .strip_prefix(r"\\?\")
                .unwrap_or(&path_text)
                .replace('\\', "/")
        },
        |unc| format!("//{}", unc.replace('\\', "/")),
    );
    #[cfg(not(windows))]
    let normalized = path_text.into_owned();
    let mut encoded = String::with_capacity(normalized.len());
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'.' | b'_' | b'-' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("write URI escape");
        }
    }
    if normalized.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn wait_workspace_ready(
    client: &Connection,
    deadline: &SessionDeadline,
    messages: &mut Vec<Value>,
) {
    let mut observed_statuses = Vec::new();
    loop {
        let message = client
            .receiver
            .recv_timeout(deadline.next_wait())
            .unwrap_or_else(|error| {
                panic!(
                    "LSP workspace discovery timed out ({error:?}); statuses: {observed_statuses:?}"
                )
            });
        match message {
            Message::Notification(notification) if notification.method == "arandu/status" => {
                let status = notification
                    .params
                    .get("state")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                observed_statuses.push((status.clone(), notification.params.clone()));
                if status.as_deref() == Some("ready") {
                    let message = notification
                        .params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    assert!(
                        message == "Workspace ready"
                            || message == "Workspace indexed; toolchain stdlib unavailable",
                        "LSP session must load the package fixture before fuzzing imports: {notification:?}"
                    );
                    messages
                        .push(serde_json::to_value(notification).expect("serialize notification"));
                    return;
                }
                messages.push(serde_json::to_value(notification).expect("serialize notification"));
            }
            Message::Notification(notification) => {
                messages.push(serde_json::to_value(notification).expect("serialize notification"));
            }
            Message::Response(response) => {
                messages.push(serde_json::to_value(response).expect("serialize response"));
            }
            Message::Request(request) => respond_to_server_request(client, request),
        }
    }
}

fn send_request(client: &Connection, id: i32, method: &str, params: Value) {
    client
        .sender
        .send(Message::Request(Request::new(
            RequestId::from(id),
            method.to_owned(),
            params,
        )))
        .expect("send client request");
}

fn send_notification(client: &Connection, method: &str, params: Value) {
    client
        .sender
        .send(Message::Notification(Notification::new(
            method.to_owned(),
            params,
        )))
        .expect("send client notification");
}

fn completion_params(uri: &str, document_index: usize, text: &str) -> Value {
    let anchor = if document_index == 0 {
        "math."
    } else {
        "return "
    };
    let cursor = text
        .rfind(anchor)
        .map_or(text.len(), |start| start + anchor.len());
    let (line, character) = lsp_position(text, cursor);
    json!({
        "textDocument": { "uri": uri },
        "position": { "line": line, "character": character }
    })
}

fn hover_params(uri: &str, text: &str) -> Value {
    let cursor = text.rfind("answer").unwrap_or(text.len());
    let (line, character) = lsp_position(text, cursor);
    json!({
        "textDocument": { "uri": uri },
        "position": { "line": line, "character": character }
    })
}

fn lsp_position(text: &str, offset: usize) -> (usize, usize) {
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_prefix = prefix.rsplit('\n').next().unwrap_or_default();
    // LSP positions count UTF-16 code units, not UTF-8 bytes or Unicode
    // scalar values. The edit corpus puts a non-BMP crab before some cursors.
    (line, line_prefix.encode_utf16().count())
}

fn receive(client: &Connection, deadline: &SessionDeadline) -> Message {
    client
        .receiver
        .recv_timeout(deadline.next_wait())
        .expect("LSP server timed out or disconnected during the session")
}

fn wait_response(
    client: &Connection,
    deadline: &SessionDeadline,
    id: RequestId,
    allow_cancel: bool,
    messages: &mut Vec<Value>,
) {
    loop {
        match receive(client, deadline) {
            Message::Response(response) if response.id == id => {
                if allow_cancel {
                    assert_response_ok_or_cancelled(&response);
                } else {
                    assert!(
                        response.response_result.is_ok(),
                        "LSP request {id} failed: {response:?}"
                    );
                }
                messages.push(serde_json::to_value(response).expect("serialize response"));
                return;
            }
            Message::Response(response) => {
                messages.push(serde_json::to_value(response).expect("serialize response"));
            }
            Message::Notification(notification) => {
                messages.push(serde_json::to_value(notification).expect("serialize notification"));
            }
            Message::Request(request) => respond_to_server_request(client, request),
        }
    }
}

fn respond_to_server_request(client: &Connection, request: Request) {
    client
        .sender
        .send(Message::Response(Response::new_ok(request.id, Value::Null)))
        .expect("answer server initiated request");
}

fn request_id_as_i32(id: &RequestId) -> i32 {
    id.to_string()
        .parse()
        .expect("fuzzer-generated request IDs are integers")
}

fn assert_response_ok_or_cancelled(response: &lsp_server::Response) {
    if let Err(error) = &response.response_result {
        assert!(
            matches!(error.code, -32802..=-32800),
            "LSP returned an unexpected error: {response:?}"
        );
    }
}

fn assert_final_completion(
    response: &lsp_server::Response,
    document_index: usize,
    source: &str,
    prior_messages: &[Value],
) {
    let Ok(value) = &response.response_result else {
        panic!("final completion must use the current document revision: {response:?}");
    };
    let items = value
        .as_array()
        .expect("completion response must serialize as an item array");
    if document_index == 0 {
        assert!(
            items
                .iter()
                .any(|item| item.get("label").and_then(Value::as_str) == Some("answer")),
            "completion at `math.` must include the exported `answer` member: {value}; source={source:?}; prior diagnostics={:?}",
            prior_messages.iter().filter(|message| {
                message.get("method").and_then(Value::as_str)
                    == Some("textDocument/publishDiagnostics")
            }).collect::<Vec<_>>()
        );
    }
}

fn edit_text(byte: u8, version: i32, document_index: usize, package_name: &str) -> String {
    let variant = (byte >> 2) % 6;
    match document_index {
        0 => main_text(package_name, variant, version),
        1 => helper_text(package_name, variant, version),
        _ => data_text(package_name, variant, version),
    }
}

fn main_text(package_name: &str, variant: u8, value: i32) -> String {
    let body = match variant {
        0 => "return math.answer()".to_owned(),
        1 => "return\n".to_owned(),
        2 => "return math.missing()".to_owned(),
        3 => format!("/* ação 🦀 {value} */ return math.answer()"),
        4 => "let 1 = 2; return 0".to_owned(),
        _ => format!(
            "let mut remaining: int = {}\n    while remaining > 0 {{ remaining = remaining - 1 }}\n    return math.answer()",
            value % 5
        ),
    };
    format!(
        "module {package_name}\nimport {package_name}.math as math\npublic func rootValue(): int {{ return 1 }}\nfunc main(): int {{ {body} }}\n"
    )
}

fn helper_text(package_name: &str, variant: u8, value: i32) -> String {
    let imports = format!("import {package_name}.data as data\n");
    let body = match variant {
        0 => "return data.value()".to_owned(),
        1 => "return\n".to_owned(),
        2 => "return unresolved".to_owned(),
        3 => format!("/* ação 🦀 {value} */ return data.value()"),
        4 => "let 1 = 2; return 0".to_owned(),
        _ => format!(
            "let mut remaining: int = {}\n    while remaining > 0 {{ remaining = remaining - 1 }}\n    return data.value()",
            value % 5
        ),
    };
    format!("{imports}public func answer(): int {{ {body} }}\n")
}

fn data_text(package_name: &str, variant: u8, value: i32) -> String {
    let imports = if variant == 5 {
        // The root module is addressed by its package-relative path, just like
        // any other file in `src/`; this edge closes main → math → data → main.
        format!("import {package_name}.main as app\n")
    } else {
        String::new()
    };
    let body = match variant {
        0 => "return 42".to_owned(),
        1 => "return\n".to_owned(),
        2 => "return missingValue".to_owned(),
        3 => format!("/* dado 🦀 {value} */ return 42"),
        4 => "let 1 = 2; return 0".to_owned(),
        _ => format!(
            "let mut remaining: int = {}\n    while remaining > 0 {{ remaining = remaining - 1 }}\n    return 42",
            value % 5
        ),
    };
    format!("{imports}public func value(): int {{ {body} }}\n")
}

fn is_final_diagnostics(notification: &Notification, uri: &str, final_version: i32) -> bool {
    notification.method == "textDocument/publishDiagnostics"
        && notification.params.get("uri").and_then(Value::as_str) == Some(uri)
        && notification.params.get("version").and_then(Value::as_i64)
            == Some(i64::from(final_version))
}

fn assert_final_diagnostics(
    messages: &[Value],
    final_versions: [i32; 3],
    document_uris: &[String; 3],
) {
    let mut saw_final = [false; 3];
    for message in messages {
        if message.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics")
        {
            continue;
        }
        let Some(document_index) = document_uris.iter().position(|uri| {
            message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
        }) else {
            continue;
        };
        let Some(version) = message.pointer("/params/version").and_then(Value::as_i64) else {
            assert!(
                !saw_final[document_index],
                "versionless diagnostics arrived after the final document revision: {message}"
            );
            continue;
        };
        let diagnostics = message
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .expect("diagnostics carry an array");
        assert!(
            diagnostics.iter().all(|diagnostic| !diagnostic
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(|code| code.starts_with("ICE"))),
            "LSP published an ICE diagnostic: {message}"
        );
        if version == i64::from(final_versions[document_index]) {
            assert!(
                diagnostics.is_empty(),
                "valid final document has diagnostics: {message}"
            );
        }
        if saw_final[document_index] {
            assert_eq!(
                version,
                i64::from(final_versions[document_index]),
                "stale diagnostics after final revision"
            );
        }
        saw_final[document_index] |= version == i64::from(final_versions[document_index]);
    }
    assert!(
        saw_final.iter().all(|seen| *seen),
        "LSP never published final diagnostics for every document: {messages:?}"
    );
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn sequence_reduction_keeps_the_original_failure_class() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            super::run_with_sequence_reduction(&[77, 1, 2, 88, 99], |candidate| {
                if candidate.ends_with(&[88, 99]) {
                    Err("protocol assertion: stable defect".to_owned())
                } else {
                    Ok(())
                }
            });
        }))
        .expect_err("the failure oracle should be reported");
        let report = super::panic_payload_message(result);

        assert!(report.contains("minimized_sequence_hex=5863"), "{report}");
        assert!(report.contains("shrink_reproduced=true"), "{report}");
        assert!(report.contains("shrink_confirmed=true"), "{report}");
    }

    #[test]
    fn sequence_reduction_falls_back_when_final_replay_is_not_confirmed() {
        let mut calls = 0;
        let result = catch_unwind(AssertUnwindSafe(|| {
            super::run_with_sequence_reduction(&[77], |_| {
                calls += 1;
                if calls <= 3 {
                    Err("protocol assertion: transient defect".to_owned())
                } else {
                    Ok(())
                }
            });
        }))
        .expect_err("the original failure should still be reported");
        let report = super::panic_payload_message(result);

        assert!(report.contains("original_sequence_hex=4d"), "{report}");
        assert!(report.contains("shrink_reproduced=true"), "{report}");
        assert!(report.contains("shrink_confirmed=false"), "{report}");
    }

    #[test]
    fn server_panic_is_shrinkable_instead_of_misclassified_as_shutdown_timeout() {
        let panic_message = catch_unwind(AssertUnwindSafe(|| {
            super::run_with_sequence_reduction(&[1, 42, 3], |candidate| {
                if candidate.contains(&42) {
                    Err(super::server_exit_failure(super::ServerExit::Panicked(
                        "worker assertion".to_owned(),
                    ))
                    .expect("panic is a server failure"))
                } else {
                    Ok(())
                }
            });
        }))
        .expect_err("the reproducible server panic must be reported");
        let report = super::panic_payload_message(panic_message);

        assert!(report.contains("LSP server panicked: worker assertion"));
        assert!(report.contains("minimized_sequence_hex=2a"), "{report}");
        assert!(report.contains("shrink_confirmed=true"), "{report}");
    }

    #[test]
    fn server_teardown_error_does_not_mask_the_protocol_failure() {
        let result = super::combine_session_outcome(
            super::ServerExit::Error("channel closed unexpectedly".to_owned()),
            Err(Box::new(String::from("protocol assertion: stale response"))),
        );

        let failure = result.expect_err("both session failures should be retained");
        assert!(failure.starts_with("protocol assertion: stale response"));
        assert!(failure.contains("server teardown also failed"));
        assert!(failure.contains("channel closed unexpectedly"));
    }

    #[test]
    fn shutdown_timeout_skips_reduction_to_avoid_accumulating_server_threads() {
        let calls = std::cell::Cell::new(0);
        let panic_message = catch_unwind(AssertUnwindSafe(|| {
            super::run_with_sequence_reduction(&[7, 8], |_| {
                calls.set(calls.get() + 1);
                Err("LSP server did not stop cleanly: timed out".to_owned())
            });
        }))
        .expect_err("shutdown timeout must fail the session");
        let report = super::panic_payload_message(panic_message);

        assert_eq!(calls.get(), 1, "do not retry a possibly live server worker");
        assert!(report.contains("shrinking skipped"), "{report}");
    }

    #[test]
    fn seeded_lsp_session_survives_all_edit_classes() {
        const SEED: &[u8] = b"\x0e\x2e0123456789abcd\xc0\xe2\x8b\x0b";
        assert!(
            SEED.len() <= super::MAX_EDITS,
            "regression seed must not be truncated"
        );
        assert!(SEED.iter().any(|byte| byte & 0x20 != 0));
        assert!(SEED
            .iter()
            .any(|byte| { usize::from((byte >> 1) % 3) == 2 && (byte >> 2) % 6 == 5 }));
        assert_eq!(
            super::edit_text(0x2e, 2, 2, "s2_small"),
            super::data_text("s2_small", 5, 2)
        );
        super::run_session_once(SEED).unwrap_or_else(|failure| {
            panic!("seeded LSP session must survive all edit classes: {failure}")
        });
    }

    #[test]
    fn minimized_close_reopen_sequence_survives_in_flight_requests() {
        super::run_session_once(&[0xe2]).unwrap_or_else(|failure| {
            panic!("close/reopen must retire stale IDE work before Salsa mutation: {failure}")
        });
    }

    #[test]
    fn completion_cursor_after_non_bmp_text_uses_utf16_units() {
        let text = super::helper_text("s2_small", 3, 7);
        let params = super::completion_params("file:///math.aru", 1, &text);
        let cursor = text.rfind("return ").unwrap() + "return ".len();
        let line_prefix = text[..cursor].rsplit('\n').next().unwrap();
        let character = params.pointer("/position/character").unwrap().as_u64();

        assert_eq!(character, Some(line_prefix.encode_utf16().count() as u64));
        assert_ne!(
            character,
            Some(line_prefix.chars().count() as u64),
            "the crab emoji must distinguish UTF-16 units from Unicode scalars"
        );
    }

    #[test]
    fn hover_cursor_after_non_bmp_text_uses_utf16_units() {
        let text = super::main_text("s2_small", 3, 7);
        let params = super::hover_params("file:///main.aru", &text);
        let cursor = text.rfind("answer").unwrap();
        let line_prefix = text[..cursor].rsplit('\n').next().unwrap();
        let character = params.pointer("/position/character").unwrap().as_u64();

        assert_eq!(character, Some(line_prefix.encode_utf16().count() as u64));
        assert_ne!(
            character,
            Some(line_prefix.chars().count() as u64),
            "the crab emoji must distinguish UTF-16 units from Unicode scalars"
        );
    }
}
