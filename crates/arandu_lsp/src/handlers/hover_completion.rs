//! Hover, completion and signature help handlers.

use super::HandlerCtx;
use crate::dispatcher;
use crate::ide;
use lsp_server::RequestId;

pub(super) fn hover(ctx: &mut HandlerCtx<'_>, id: RequestId, params: lsp_types::HoverParams) {
    let uri = params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        match ide::hover(snap, info.source, text, pos) {
            Some(h) => serde_json::to_value(h).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    });
}

pub(super) fn completion(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::CompletionParams,
) {
    let uri = params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let items = ide::completions(snap, info.source, text, pos);
        serde_json::to_value(lsp_types::CompletionResponse::Array(items))
            .unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn signature_help(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::SignatureHelpParams,
) {
    let uri = params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        match ide::signature_help(snap, info.source, text, pos) {
            Some(h) => serde_json::to_value(h).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    });
}

pub(super) fn inlay_hints(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::InlayHintParams,
) {
    let uri = params.text_document.uri;
    let range = params.range;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let hints = ide::inlay_hints(snap, info.source, text, range);
        serde_json::to_value(hints).unwrap_or(serde_json::Value::Null)
    });
}
