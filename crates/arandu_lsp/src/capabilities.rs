//! LSP lifecycle: initialize handshake and server capability advertisement.
//!
//! `initialize` must respond without scanning, reading or analyzing the
//! workspace (AGENTS.md): discovery happens later, in [`crate::workspace`].

use crate::ide;
use crate::uri_util::path_from_uri;
use lsp_server::Connection;
use lsp_types::{
    CodeActionOptions, CodeActionProviderCapability, CompletionOptions, FileOperationFilter,
    FileOperationPattern, FileOperationPatternKind, FileOperationRegistrationOptions,
    FoldingRangeProviderCapability, HoverProviderCapability, InitializeResult, InlayHintOptions,
    InlayHintServerCapabilities, OneOf, PositionEncodingKind, RenameOptions,
    SelectionRangeProviderCapability, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, SignatureHelpOptions,
    TextDocumentSyncCapability, TextDocumentSyncKind, TypeDefinitionProviderCapability,
    WorkDoneProgressOptions, WorkspaceFileOperationsServerCapabilities,
    WorkspaceServerCapabilities,
};
use std::error::Error;
use std::path::PathBuf;

pub(crate) struct InitializedContext {
    pub(crate) ctfe_limits: arandu_query::ctfe::CtfeLimits,
    pub(crate) workspace_roots: Vec<PathBuf>,
    pub(crate) work_done_progress: bool,
    pub(crate) trace_value: lsp_types::TraceValue,
}

pub(crate) fn initialize_connection(
    connection: &Connection,
) -> Result<InitializedContext, Box<dyn Error + Sync + Send>> {
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let init: lsp_types::InitializeParams = serde_json::from_value(initialize_params)?;
    let ctfe_limits = match parse_ctfe_limits(init.initialization_options.as_ref()) {
        Ok(limits) => limits,
        Err(message) => {
            connection.sender.send(lsp_server::Message::Response(
                lsp_server::Response::new_err(
                    initialize_id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    message.clone(),
                ),
            ))?;
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, message).into());
        }
    };
    let trace_value = init.trace.unwrap_or(lsp_types::TraceValue::Off);
    crate::logging::set_trace(trace_value);
    let mut workspace_roots = Vec::new();
    if let Some(folders) = init.workspace_folders.as_ref() {
        workspace_roots.extend(folders.iter().filter_map(|folder| {
            let root = path_from_uri(&folder.uri);
            (!root.as_os_str().is_empty()).then_some(root)
        }));
    } else {
        // root_uri deprecated in favor of workspace_folders; still used by older clients.
        #[allow(deprecated)]
        if let Some(root_uri) = init.root_uri.as_ref() {
            let root = path_from_uri(root_uri);
            if !root.as_os_str().is_empty() {
                workspace_roots.push(root);
            }
        }
    }
    workspace_roots.sort();
    workspace_roots.dedup();

    let aru_file_operations = || FileOperationRegistrationOptions {
        filters: ["**/*.aru", "**/arandu.toml", "**/Arandu.toml"]
            .into_iter()
            .map(|glob| FileOperationFilter {
                scheme: Some("file".into()),
                pattern: FileOperationPattern {
                    glob: glob.into(),
                    matches: Some(FileOperationPatternKind::File),
                    options: None,
                },
            })
            .collect(),
    };
    let server_caps = ServerCapabilities {
        // UTF-16 is the mandatory LSP encoding. Advertise it explicitly even
        // when a client prefers optional encodings we do not implement yet.
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        definition_provider: Some(OneOf::Left(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![".".into(), ":".into()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![",".into()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: ide::semantic_tokens_legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
            InlayHintOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                resolve_provider: None,
            },
        ))),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
            resolve_provider: None,
        })),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: None,
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                did_create: Some(aru_file_operations()),
                did_rename: Some(aru_file_operations()),
                did_delete: Some(aru_file_operations()),
                ..WorkspaceFileOperationsServerCapabilities::default()
            }),
        }),
        ..ServerCapabilities::default()
    };
    let init_result = InitializeResult {
        capabilities: server_caps,
        server_info: Some(ServerInfo {
            name: "arandu-lsp".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    };
    let work_done_progress = init
        .capabilities
        .window
        .as_ref()
        .and_then(|window| window.work_done_progress)
        .unwrap_or(false);
    connection.initialize_finish(initialize_id, serde_json::to_value(init_result)?)?;
    Ok(InitializedContext {
        ctfe_limits,
        workspace_roots,
        work_done_progress,
        trace_value,
    })
}

/// Optional initializationOptions.ctfe fields inherit the shared defaults.
fn parse_ctfe_limits(
    options: Option<&serde_json::Value>,
) -> Result<arandu_query::ctfe::CtfeLimits, String> {
    let defaults = arandu_query::ctfe::CtfeLimits::default().budget();
    let Some(value) = options.and_then(|options| options.get("ctfe")) else {
        return Ok(arandu_query::ctfe::CtfeLimits::default());
    };
    let object = value
        .as_object()
        .ok_or("initializationOptions.ctfe must be an object")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "fuel" | "frames" | "values") {
            return Err(format!("unknown initializationOptions.ctfe field: {key}"));
        }
    }
    let ceiling = |name: &str, default: u64| -> Result<u64, String> {
        match object.get(name) {
            None => Ok(default),
            Some(value) => value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                format!("initializationOptions.ctfe.{name} must be a positive integer")
            }),
        }
    };
    let frames = u32::try_from(ceiling("frames", u64::from(defaults.frames))?)
        .map_err(|_| "initializationOptions.ctfe.frames exceeds the supported integer range")?;
    arandu_query::ctfe::CtfeLimits::new(
        ceiling("fuel", defaults.fuel)?,
        frames,
        ceiling("values", defaults.values)?,
    )
    .map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_server::{Message, Request, RequestId};
    use std::time::{Duration, Instant};

    #[test]
    fn initialize_finishes_before_workspace_discovery() {
        let (server, client) = Connection::memory();
        let root =
            crate::uri_util::uri_from_path(std::path::Path::new("/workspace/with-many-files"))
                .or_else(|| crate::uri_util::parse_uri("file:///workspace/with-many-files"))
                .expect("workspace URI");
        let request = Request::new(
            RequestId::from(1),
            "initialize".into(),
            serde_json::json!({
                "processId": null,
                "capabilities": {},
                "workspaceFolders": [{ "uri": root, "name": "fixture" }]
            }),
        );

        let server_thread = std::thread::spawn(move || initialize_connection(&server));
        let started = Instant::now();
        client
            .sender
            .send(Message::Request(request))
            .expect("send initialize");
        let response = client
            .receiver
            .recv_timeout(Duration::from_millis(250))
            .expect("initialize must satisfy the cold p95 budget");
        assert!(matches!(
            response,
            Message::Response(lsp_server::Response {
                response_result: Ok(_),
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_millis(250));
        client
            .sender
            .send(Message::Notification(lsp_server::Notification::new(
                "initialized".into(),
                serde_json::json!({}),
            )))
            .expect("send initialized");

        let initialized = server_thread
            .join()
            .expect("initialize thread")
            .expect("initialize result");
        assert_eq!(initialized.workspace_roots.len(), 1);
        assert!(!initialized.work_done_progress);
    }
}
