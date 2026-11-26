#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CodeAction, CodeActionContext, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    Diagnostic, DidOpenTextDocumentParams, InitializeParams, InitializedParams,
    PartialResultParams, Position, Range, TextDocumentIdentifier, TextDocumentItem, TextEdit, Uri,
    WorkDoneProgressParams,
};
use serde_json::{json, Value};
use std::str::FromStr;
use std::thread;

fn spawn_memory_lsp() -> (Connection, thread::JoinHandle<()>) {
    let (client, server) = Connection::memory();
    let handle = thread::spawn(move || {
        let _ = arandu_lsp::run(server);
    });
    (client, handle)
}

fn send_request(client: &Connection, id: i32, method: &str, params: Value) -> Response {
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(id),
            method: method.to_string(),
            params,
        }))
        .unwrap();
    loop {
        match client.receiver.recv().unwrap() {
            Message::Response(resp) if resp.id == RequestId::from(id) => return resp,
            _ => {}
        }
    }
}

fn open_and_collect_diagnostics(client: &Connection, uri: &str, text: &str) -> Vec<Diagnostic> {
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Uri::from_str(uri).unwrap(),
                    language_id: "arandu".to_string(),
                    version: 1,
                    text: text.to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    loop {
        match client.receiver.recv().unwrap() {
            Message::Notification(note) if note.method == "textDocument/publishDiagnostics" => {
                let params: lsp_types::PublishDiagnosticsParams =
                    serde_json::from_value(note.params).unwrap();
                if params.uri.as_str() == uri {
                    return params.diagnostics;
                }
            }
            _ => {}
        }
    }
}

fn initialize_server(client: &Connection) -> lsp_types::InitializeResult {
    let resp = send_request(
        client,
        1,
        "initialize",
        serde_json::to_value(InitializeParams::default()).unwrap(),
    );
    let init: lsp_types::InitializeResult =
        serde_json::from_value(resp.response_result.unwrap()).unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::to_value(InitializedParams {}).unwrap(),
        }))
        .unwrap();
    loop {
        if let Message::Notification(note) = client.receiver.recv().unwrap() {
            if note.method == "arandu/status"
                && note.params.get("state").and_then(Value::as_str) == Some("ready")
            {
                break;
            }
        }
    }
    init
}

fn shutdown_server(client: &Connection, handle: thread::JoinHandle<()>) {
    let _ = send_request(client, 999, "shutdown", json!(null));
    let _ = client.sender.send(Message::Notification(Notification {
        method: "exit".to_string(),
        params: json!(null),
    }));
    let _ = handle.join();
}

#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit(text: &str, edit: &lsp_types::WorkspaceEdit, uri: &Uri) -> String {
    let edits: Vec<&TextEdit> = edit
        .changes
        .as_ref()
        .and_then(|c| c.get(uri))
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    assert!(!edits.is_empty(), "expected non-empty edits for {uri:?}");
    // Apply single edit using line/utf-16 coordinates.
    let e = edits[0];
    let to_byte = |pos: Position| -> usize {
        let mut offset = 0usize;
        for (line, segment) in text.split_inclusive('\n').enumerate() {
            if line == pos.line as usize {
                let mut col_u16 = 0u32;
                for (bidx, ch) in segment.char_indices() {
                    if col_u16 >= pos.character {
                        return offset + bidx;
                    }
                    col_u16 += ch.len_utf16() as u32;
                }
                return offset + segment.len();
            }
            offset += segment.len();
        }
        text.len()
    };
    let s = to_byte(e.range.start);
    let en = to_byte(e.range.end);
    format!("{}{}{}", &text[..s], e.new_text, &text[en..])
}

fn request_code_actions(
    client: &Connection,
    req_id: i32,
    uri_str: &str,
    diagnostics: Vec<Diagnostic>,
    only: Option<Vec<CodeActionKind>>,
) -> Vec<CodeAction> {
    let uri = Uri::from_str(uri_str).unwrap();
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        context: CodeActionContext {
            diagnostics,
            only,
            trigger_kind: None,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp = send_request(
        client,
        req_id,
        "textDocument/codeAction",
        serde_json::to_value(params).unwrap(),
    );
    let actions: Vec<CodeActionOrCommand> =
        serde_json::from_value(resp.response_result.unwrap()).unwrap();
    actions
        .into_iter()
        .filter_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) => Some(ca),
            CodeActionOrCommand::Command(_) => None,
        })
        .collect()
}

#[test]
fn server_advertises_organize_imports_and_executes_all_structured_quickfixes() {
    let (client, handle) = spawn_memory_lsp();
    let init = initialize_server(&client);
    match init.capabilities.code_action_provider {
        Some(lsp_types::CodeActionProviderCapability::Options(opts)) => {
            let kinds = opts.code_action_kinds.unwrap_or_default();
            assert!(kinds.contains(&CodeActionKind::QUICKFIX));
            assert!(kinds.contains(&CodeActionKind::SOURCE_ORGANIZE_IMPORTS));
        }
        other => panic!("expected CodeActionOptions, got {other:?}"),
    }

    // 1) source.organizeImports
    let org_uri = "file:///tmp/org_demo.aru";
    let org_text = "\
module app.org

import std.io as io
import std.fs as files
import std.mem as mem

func main(): void {
    let _ = files.exists(\"Cargo.toml\")
    io.println(\"ok\")
}
";
    let _ = open_and_collect_diagnostics(&client, org_uri, org_text);
    let org_actions = request_code_actions(
        &client,
        2,
        org_uri,
        vec![],
        Some(vec![CodeActionKind::SOURCE_ORGANIZE_IMPORTS]),
    );
    let org_action = org_actions
        .iter()
        .find(|a| a.kind == Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS))
        .expect("expected source.organizeImports action");
    let org_result = apply_workspace_edit(
        org_text,
        org_action.edit.as_ref().unwrap(),
        &Uri::from_str(org_uri).unwrap(),
    );
    assert!(org_result.contains("import std.{fs as files, io}"));
    assert!(!org_result.contains("mem"));

    // 2) T024NonExhaustiveMatch quickfix
    let match_uri = "file:///tmp/match_fix.aru";
    let match_text = "\
module app.match_fix

enum Color { Red, Green, Blue }

func code(c: Color): int {
    match c {
        Color.Red => { return 1 }
    }
    return 0
}
";
    let match_diags = open_and_collect_diagnostics(&client, match_uri, match_text);
    let match_actions = request_code_actions(&client, 3, match_uri, match_diags, None);
    let match_fix = match_actions
        .iter()
        .find(|a| a.kind == Some(CodeActionKind::QUICKFIX) && a.title.contains("todo()"))
        .expect("expected match arm todo() quickfix");
    let patched_match = apply_workspace_edit(
        match_text,
        match_fix.edit.as_ref().unwrap(),
        &Uri::from_str(match_uri).unwrap(),
    );
    assert!(patched_match.contains("Color.Green => todo(),"));
    assert!(patched_match.contains("Color.Blue => todo(),"));

    // 3) T025InterfaceNotSatisfied quickfix
    let iface_uri = "file:///tmp/iface_fix.aru";
    let iface_text = "\
module app.iface_fix

interface Greeter {
    func greet(self: ref Self): str
}

struct Widget {
    id: int,
}

func run<T: Greeter>(x: ref T): str {
    return x.greet()
}

func main(): str {
    let w = Widget { id: 1 }
    return run(ref w)
}
";
    let iface_diags = open_and_collect_diagnostics(&client, iface_uri, iface_text);
    let iface_actions = request_code_actions(&client, 4, iface_uri, iface_diags, None);
    let iface_fix = iface_actions
        .iter()
        .find(|a| a.kind == Some(CodeActionKind::QUICKFIX) && a.title.contains("Widget"))
        .expect("expected interface stub quickfix");
    let patched_iface = apply_workspace_edit(
        iface_text,
        iface_fix.edit.as_ref().unwrap(),
        &Uri::from_str(iface_uri).unwrap(),
    );
    assert!(patched_iface.contains("func Widget.greet("));
    assert!(patched_iface.contains("todo()"));

    // 4) mut on local struct field assignment & .toString() on str -> String
    let mut_str_uri = "file:///tmp/mut_str_fix.aru";
    let mut_str_text = "\
module app.mut_str_fix

struct Boxed { x: int }

func takesOwned(s: String): void {}

func main(): void {
    let b = Boxed { x: 1 }
    b.x = 2
    let raw: str = \"hello\"
    takesOwned(raw)
}
";
    let mut_str_diags = open_and_collect_diagnostics(&client, mut_str_uri, mut_str_text);
    let ms_actions = request_code_actions(&client, 5, mut_str_uri, mut_str_diags.clone(), None);
    let mut_fix = ms_actions
        .iter()
        .find(|a| a.title.contains("mutable"))
        .expect("expected `mut` quickfix on immutable local field assignment");
    let patched_mut = apply_workspace_edit(
        mut_str_text,
        mut_fix.edit.as_ref().unwrap(),
        &Uri::from_str(mut_str_uri).unwrap(),
    );
    assert!(patched_mut.contains("let mut b = Boxed { x: 1 }"));

    let to_str_fix = ms_actions
        .iter()
        .find(|a| a.title.contains(".toString()"))
        .unwrap_or_else(|| {
            panic!(
                "expected `.toString()` quickfix on str -> String\ndiags: {mut_str_diags:#?}\nactions: {ms_actions:#?}"
            )
        });
    let patched_tostr = apply_workspace_edit(
        mut_str_text,
        to_str_fix.edit.as_ref().unwrap(),
        &Uri::from_str(mut_str_uri).unwrap(),
    );
    assert!(patched_tostr.contains("takesOwned(raw.toString())"));

    shutdown_server(&client, handle);
}
