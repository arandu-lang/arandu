//! Pure document formatting adapter (`arandu_fmt` -> LSP text edits).

use arandu_base::LineIndex;
use lsp_types::TextEdit as LspTextEdit;

use crate::conv::{offset_to_position, position_to_offset};

/// Format entire document (F3a) → minimal, non-overlapping LSP text edits.
#[must_use]
pub fn format_document(text: &str) -> Vec<LspTextEdit> {
    let edits = arandu_fmt::format_edits(text);
    if edits.is_empty() {
        return Vec::new();
    }
    let index = LineIndex::new(text);
    edits
        .into_iter()
        .map(|e| {
            let start = offset_to_position(&index, e.start);
            let end = offset_to_position(&index, e.end);
            LspTextEdit {
                range: lsp_types::Range { start, end },
                new_text: e.new_text,
            }
        })
        .collect()
}

/// Format only the top-level items intersecting `range` (F3b).
///
/// Returns [`arandu_fmt::format_edits_in_range`] hunks converted to LSP
/// positions, so unrelated regions keep their original layout.
#[must_use]
pub fn format_range(text: &str, range: lsp_types::Range) -> Vec<LspTextEdit> {
    if text.is_empty() {
        return Vec::new();
    }
    let index = LineIndex::new(text);
    let start = position_to_offset(&index, range.start, text);
    let end = position_to_offset(&index, range.end, text);
    let edits = arandu_fmt::format_edits_in_range(text, start, end);
    edits
        .into_iter()
        .map(|e| {
            let start = offset_to_position(&index, e.start);
            let end = offset_to_position(&index, e.end);
            LspTextEdit {
                range: lsp_types::Range { start, end },
                new_text: e.new_text,
            }
        })
        .collect()
}
