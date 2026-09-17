//! Per-message handlers.
//!
//! Mirrors rust-analyzer's layout: [`dispatch_request`] is the routing table
//! (one arm per LSP method, param extraction at the boundary) and each feature
//! module owns its handler logic. Handlers schedule work through
//! [`crate::dispatcher`] helpers and never touch live analysis state beyond
//! what an explicit flush commits.

mod hover_completion;
mod nav;
mod notification;
mod refactor;
mod tokens;

// Goto runs on a dedicated snapshot job; the dispatcher schedules it directly.
pub(crate) use nav::goto_on_snapshot;

use crate::dispatcher::{self, JobResult};
use crate::pool::WorkerPool;
use crate::state::ServerState;
use lsp_server::{Connection, ErrorCode, Notification, Request};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest,
    FoldingRangeRequest, Formatting, GotoDefinition, GotoTypeDefinition, HoverRequest,
    InlayHintRequest, PrepareRenameRequest, RangeFormatting, References, Rename, Request as _,
    SelectionRangeRequest, SemanticTokensFullRequest, SemanticTokensRangeRequest,
    SignatureHelpRequest, WorkspaceSymbolRequest,
};
use std::error::Error;

/// Everything a message handler may touch on the main thread.
pub(crate) struct HandlerCtx<'a> {
    pub(crate) connection: &'a Connection,
    pub(crate) state: &'a mut ServerState,
    pub(crate) pool: &'a WorkerPool,
    pub(crate) job_tx: &'a crossbeam_channel::Sender<JobResult>,
}

pub(crate) fn dispatch_request(
    ctx: &mut HandlerCtx<'_>,
    req: Request,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    // Ensure pending edits are visible for semantic requests.
    if matches!(
        req.method.as_str(),
        GotoDefinition::METHOD
            | GotoTypeDefinition::METHOD
            | HoverRequest::METHOD
            | Completion::METHOD
            | References::METHOD
            | DocumentHighlightRequest::METHOD
            | FoldingRangeRequest::METHOD
            | SelectionRangeRequest::METHOD
            | PrepareRenameRequest::METHOD
            | Rename::METHOD
            | SignatureHelpRequest::METHOD
            | DocumentSymbolRequest::METHOD
            | WorkspaceSymbolRequest::METHOD
            | SemanticTokensFullRequest::METHOD
            | SemanticTokensRangeRequest::METHOD
            | Formatting::METHOD
            | RangeFormatting::METHOD
            | CodeActionRequest::METHOD
            | InlayHintRequest::METHOD
    ) {
        dispatcher::flush_for_request(ctx.state, ctx.pool, ctx.job_tx);
    }

    match req.method.as_str() {
        GotoDefinition::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::GotoDefinitionParams>(GotoDefinition::METHOD)?;
            nav::goto_definition(ctx, id, params);
        }
        GotoTypeDefinition::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::GotoDefinitionParams>(GotoTypeDefinition::METHOD)?;
            nav::type_definition(ctx, id, params);
        }
        InlayHintRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::InlayHintParams>(InlayHintRequest::METHOD)?;
            hover_completion::inlay_hints(ctx, id, params);
        }
        HoverRequest::METHOD => {
            let (id, params) = req.extract::<lsp_types::HoverParams>(HoverRequest::METHOD)?;
            hover_completion::hover(ctx, id, params);
        }
        Completion::METHOD => {
            let (id, params) = req.extract::<lsp_types::CompletionParams>(Completion::METHOD)?;
            hover_completion::completion(ctx, id, params);
        }
        SignatureHelpRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::SignatureHelpParams>(SignatureHelpRequest::METHOD)?;
            hover_completion::signature_help(ctx, id, params);
        }
        References::METHOD => {
            let (id, params) = req.extract::<lsp_types::ReferenceParams>(References::METHOD)?;
            nav::references(ctx, id, params);
        }
        DocumentHighlightRequest::METHOD => {
            let (id, params) = req
                .extract::<lsp_types::DocumentHighlightParams>(DocumentHighlightRequest::METHOD)?;
            nav::document_highlight(ctx, id, params);
        }
        FoldingRangeRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::FoldingRangeParams>(FoldingRangeRequest::METHOD)?;
            nav::folding_range(ctx, id, params);
        }
        SelectionRangeRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::SelectionRangeParams>(SelectionRangeRequest::METHOD)?;
            nav::selection_range(ctx, id, params);
        }
        Rename::METHOD => {
            let (id, params) = req.extract::<lsp_types::RenameParams>(Rename::METHOD)?;
            refactor::rename(ctx, id, params);
        }
        PrepareRenameRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::TextDocumentPositionParams>(PrepareRenameRequest::METHOD)?;
            refactor::prepare_rename(ctx, id, params);
        }
        DocumentSymbolRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::DocumentSymbolParams>(DocumentSymbolRequest::METHOD)?;
            nav::document_symbols(ctx, id, params);
        }
        WorkspaceSymbolRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::WorkspaceSymbolParams>(WorkspaceSymbolRequest::METHOD)?;
            nav::workspace_symbols(ctx, id, params);
        }
        SemanticTokensFullRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::SemanticTokensParams>(SemanticTokensFullRequest::METHOD)?;
            tokens::semantic_tokens_full(ctx, id, params);
        }
        SemanticTokensRangeRequest::METHOD => {
            let (id, params) = req.extract::<lsp_types::SemanticTokensRangeParams>(
                SemanticTokensRangeRequest::METHOD,
            )?;
            tokens::semantic_tokens_range(ctx, id, params);
        }
        Formatting::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::DocumentFormattingParams>(Formatting::METHOD)?;
            refactor::formatting(ctx, id, params);
        }
        RangeFormatting::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::DocumentRangeFormattingParams>(RangeFormatting::METHOD)?;
            refactor::range_formatting(ctx, id, params);
        }
        CodeActionRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::CodeActionParams>(CodeActionRequest::METHOD)?;
            refactor::code_actions(ctx, id, params);
        }
        _ => {
            let resp = lsp_server::Response::new_err(
                req.id,
                ErrorCode::MethodNotFound as i32,
                format!("unknown request {}", req.method),
            );
            ctx.connection
                .sender
                .send(lsp_server::Message::Response(resp))?;
        }
    }
    Ok(())
}

pub(crate) fn dispatch_notification(
    ctx: &mut HandlerCtx<'_>,
    not: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    notification::handle(ctx, not)
}
