//! Navigation handlers: goto definition, references, highlights, symbols,
//! folding and selection ranges.

use super::HandlerCtx;
use crate::conv::{position_to_offset, span_to_range};
use crate::dispatcher;
use crate::ide;
use crate::state::{DocInfo, ServerState};
use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, ArandCompilerDb, DocumentId, LspSymbolId};
use lsp_server::RequestId;
use rustc_hash::FxHashMap;
use std::sync::Arc;

pub(super) fn goto_definition(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::GotoDefinitionParams,
) {
    let uri = params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;
    dispatcher::spawn_goto(ctx.state, ctx.pool, ctx.job_tx, id, uri, pos);
}

/// Goto type definition (LSP `textDocument/typeDefinition`): navigates to the
/// definition of the value's underlying named type, or to the type symbol
/// itself when the cursor sits on a type usage.
pub(super) fn type_definition(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::GotoDefinitionParams,
) {
    let uri = params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let location = type_definition_on_snapshot(snap, docs, info, &uri, pos);
        serde_json::to_value(location).unwrap_or(serde_json::Value::Null)
    });
}

fn type_definition_on_snapshot(
    snap: &AnalysisSnapshot,
    docs: &FxHashMap<String, DocInfo>,
    info: &DocInfo,
    uri: &lsp_types::Uri,
    position: lsp_types::Position,
) -> Option<lsp_types::Location> {
    use arandu_base::Span;

    let text = info.source.text(&snap.db);
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let tc = arandu_query::passes::type_check(&snap.db, info.source);
    let program = arandu_query::passes::parse(&snap.db, info.source);
    let sym_id = ServerState::symbol_at(tc, offset).or_else(|| {
        program
            .as_ref()
            .as_ref()
            .ok()
            .and_then(|program| ide::expr_symbol_at(program, tc, offset))
    })?;
    let type_id = ide::type_definition_symbol(tc, sym_id)?;
    let def_span: Span = arandu_query::passes::symbol_span(&snap.db, type_id);

    let (def_uri, def_text) = if def_span.file_id == *info.source.file_id(&snap.db) {
        let uri = crate::uri_util::parse_uri(uri.as_str())?;
        (uri, text.to_string())
    } else if let Some((doc_uri, doc)) = docs
        .iter()
        .find(|(_, doc)| *doc.source.file_id(&snap.db) == def_span.file_id)
    {
        let doc_uri = crate::uri_util::parse_uri(doc_uri)?;
        let doc_text = doc.source.text(&snap.db).as_ref().to_string();
        (doc_uri, doc_text)
    } else {
        let p = snap.db.file_path(def_span.file_id);
        if p.as_os_str().is_empty() {
            return None;
        }
        let uri = crate::uri_util::uri_from_path(p.as_ref())?;
        let doc_text = std::fs::read_to_string(p.as_ref()).ok()?;
        (uri, doc_text)
    };
    let def_index = LineIndex::new(&def_text);
    Some(lsp_types::Location {
        uri: def_uri,
        range: span_to_range(&def_index, def_span),
    })
}

pub(super) fn references(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::ReferenceParams,
) {
    let uri = params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let locs = ide::references(snap, info.source, text, pos, &uri);
        serde_json::to_value(locs).unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn document_highlight(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::DocumentHighlightParams,
) {
    let uri = params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let highlights = ide::document_highlights(snap, info.source, text, pos);
        serde_json::to_value(highlights).unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn folding_range(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::FoldingRangeParams,
) {
    let uri = params.text_document.uri;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        serde_json::to_value(ide::folding_ranges(snap, info.source, text))
            .unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn selection_range(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::SelectionRangeParams,
) {
    let uri = params.text_document.uri;
    let positions = params.positions;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        serde_json::to_value(ide::selection_ranges(snap, info.source, text, &positions))
            .unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn document_symbols(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::DocumentSymbolParams,
) {
    let uri = params.text_document.uri;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let text = info.source.text(&snap.db);
        let syms = ide::document_symbols(snap, info.source, text, &uri);
        serde_json::to_value(lsp_types::DocumentSymbolResponse::Flat(syms))
            .unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn workspace_symbols(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::WorkspaceSymbolParams,
) {
    let query = params.query;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let list: Vec<ide::DocSnap> = docs
            .iter()
            .filter_map(|(uri_s, info)| {
                let uri = crate::uri_util::parse_uri(uri_s)
                    .or_else(|| crate::uri_util::uri_from_path(&info.path))?;
                Some(ide::DocSnap {
                    source: info.source,
                    path: Arc::clone(&info.path),
                    uri,
                })
            })
            .collect();
        let syms = ide::workspace_symbols(snap, &list, &query);
        serde_json::to_value(syms).unwrap_or(serde_json::Value::Null)
    });
}

/// Resolves the definition location for `position` against an immutable
/// snapshot. Falls back to reading a non-registered file from disk when the
/// definition lives outside the workspace registry (LSP layer — IO allowed).
pub(crate) fn goto_on_snapshot(
    snap: &AnalysisSnapshot,
    by_uri: &FxHashMap<String, DocumentId>,
    by_file_id: &FxHashMap<u32, DocumentId>,
    docs: &FxHashMap<DocumentId, DocInfo>,
    uri: &lsp_types::Uri,
    position: lsp_types::Position,
) -> Option<lsp_types::Location> {
    use arandu_base::Span;

    let id = *by_uri.get(uri.as_str())?;
    let info = docs.get(&id)?;
    let text = info.source.text(&snap.db);
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let tc = arandu_query::passes::type_check(&snap.db, info.source);
    let program = arandu_query::passes::parse(&snap.db, info.source);
    let sym_id = ServerState::symbol_at(tc, offset).or_else(|| {
        program
            .as_ref()
            .as_ref()
            .ok()
            .and_then(|program| ide::expr_symbol_at(program, tc, offset))
    })?;
    let lsp_sym = LspSymbolId::new(sym_id, snap.revision);
    let sym_id = lsp_sym.resolve(snap)?;
    let _symbol = tc.symbols.try_get(sym_id)?;
    let def_span: Span = arandu_query::passes::symbol_span(&snap.db, sym_id);
    let def_uri = uri_for_file_id(by_file_id, docs, &snap.db, def_span.file_id)?;
    let def_text = if def_span.file_id == *info.source.file_id(&snap.db) {
        text.clone()
    } else if let Some(&def_id) = by_file_id.get(&def_span.file_id) {
        let d = docs.get(&def_id)?;
        d.source.text(&snap.db).clone()
    } else {
        let p = snap.db.file_path(def_span.file_id);
        Arc::from(std::fs::read_to_string(p.as_ref()).ok()?.as_str())
    };
    let def_index = LineIndex::new(&def_text);
    Some(lsp_types::Location {
        uri: def_uri,
        range: span_to_range(&def_index, def_span),
    })
}

fn uri_for_file_id(
    by_file_id: &FxHashMap<u32, DocumentId>,
    docs: &FxHashMap<DocumentId, DocInfo>,
    db: &arandu_query::DatabaseImpl,
    file_id: u32,
) -> Option<lsp_types::Uri> {
    if let Some(&id) = by_file_id.get(&file_id) {
        if let Some(doc) = docs.get(&id) {
            return crate::uri_util::uri_from_path(doc.path.as_ref());
        }
    }
    let path = db.file_path(file_id);
    if path.as_os_str().is_empty() {
        return None;
    }
    crate::uri_util::uri_from_path(path.as_ref())
}
