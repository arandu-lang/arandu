//! Rename, formatting and code action handlers.

use super::HandlerCtx;
use crate::dispatcher;
use crate::ide;
use lsp_server::{ErrorCode, RequestId};
use std::sync::Arc;

pub(super) fn rename(ctx: &mut HandlerCtx<'_>, id: RequestId, params: lsp_types::RenameParams) {
    let uri = params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    let new_name = params.new_name;
    dispatcher::spawn_json_result(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return Ok(serde_json::Value::Null);
        };
        let text = info.source.text(&snap.db);
        let documents = docs
            .iter()
            .filter_map(|(uri, info)| {
                Some(ide::DocSnap {
                    source: info.source,
                    path: Arc::clone(&info.path),
                    uri: crate::uri_util::parse_uri(uri)?,
                })
            })
            .collect::<Vec<_>>();
        match ide::rename_edits(snap, info.source, text, pos, &documents, &new_name) {
            Ok(edit) => Ok(serde_json::to_value(edit).unwrap_or(serde_json::Value::Null)),
            Err(error) => Err((ErrorCode::InvalidParams as i32, error.message())),
        }
    });
}

pub(super) fn prepare_rename(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::TextDocumentPositionParams,
) {
    let uri = params.text_document.uri;
    let pos = params.position;
    dispatcher::spawn_json_result(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return Ok(serde_json::Value::Null);
        };
        let text = info.source.text(&snap.db);
        match ide::prepare_rename(snap, info.source, text, pos) {
            Ok(prepared) => Ok(serde_json::to_value(prepared).unwrap_or(serde_json::Value::Null)),
            Err(error) => Err((ErrorCode::InvalidParams as i32, error.message())),
        }
    });
}

pub(super) fn formatting(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::DocumentFormattingParams,
) {
    let uri = params.text_document.uri;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let edits = ide::format_document(text);
        serde_json::to_value(edits).unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn range_formatting(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::DocumentRangeFormattingParams,
) {
    let uri = params.text_document.uri;
    let range = params.range;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let edits = ide::format_range(text, range);
        serde_json::to_value(edits).unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn code_actions(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::CodeActionParams,
) {
    let uri = params.text_document.uri;
    let context = params.context;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |_snap, docs| {
        let Some(_info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let actions = ide::code_actions(&uri, &context);
        serde_json::to_value(actions).unwrap_or(serde_json::Value::Null)
    });
}
