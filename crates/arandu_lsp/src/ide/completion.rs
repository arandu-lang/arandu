//! LSP adapter over the shared, editor-agnostic completion engine.
//!
//! All ranking, scope-awareness and member/type inference live in `arandu_ide`.
//! This module only translates a byte offset and maps neutral items onto the
//! `lsp_types` protocol shape.

use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat, Position};

use super::presentation::markdown_documentation;
use crate::conv::position_to_offset;

#[cfg(test)]
pub(crate) use arandu_ide::completion::import_path_completions;

#[must_use]
pub fn completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Vec<CompletionItem> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    arandu_ide::completions(snap, source, text, offset)
        .into_iter()
        .map(to_lsp)
        .collect()
}

fn to_lsp(item: arandu_ide::CompletionItem) -> CompletionItem {
    CompletionItem {
        label: item.label,
        kind: Some(kind_to_lsp(item.kind)),
        detail: item.detail,
        documentation: item.documentation.map(markdown_documentation),
        insert_text: item.insert_text,
        insert_text_format: item.is_snippet.then_some(InsertTextFormat::SNIPPET),
        filter_text: item.filter_text,
        sort_text: item.sort_text,
        ..CompletionItem::default()
    }
}

fn kind_to_lsp(kind: arandu_ide::CompletionKind) -> CompletionItemKind {
    use arandu_ide::CompletionKind as Kind;
    match kind {
        Kind::Keyword => CompletionItemKind::KEYWORD,
        Kind::Function => CompletionItemKind::FUNCTION,
        Kind::Method => CompletionItemKind::METHOD,
        Kind::Struct => CompletionItemKind::STRUCT,
        Kind::Enum => CompletionItemKind::ENUM,
        Kind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        Kind::Interface => CompletionItemKind::INTERFACE,
        Kind::Constant => CompletionItemKind::CONSTANT,
        Kind::Field => CompletionItemKind::FIELD,
        Kind::Variable => CompletionItemKind::VARIABLE,
        Kind::Module => CompletionItemKind::MODULE,
        Kind::Property => CompletionItemKind::PROPERTY,
        Kind::TypeParameter => CompletionItemKind::TYPE_PARAMETER,
        Kind::Text => CompletionItemKind::TEXT,
    }
}
