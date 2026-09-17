//! Inlay type hints (LSP `textDocument/inlayHint`).

use arandu_base::LineIndex;
use arandu_middle::types::ArType;
use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};

use super::presentation::{display_type, typecheck};
use crate::conv::{offset_to_position, position_to_offset};

/// Type hints for unannotated `let` bindings inside `range`.
#[must_use]
pub fn inlay_hints(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    range: Range,
) -> Vec<InlayHint> {
    let index = LineIndex::new(text);
    let start = position_to_offset(&index, range.start, text);
    let end = position_to_offset(&index, range.end, text);
    let tc = typecheck(snap, source);

    let mut hints = Vec::new();
    for (key, &symbol_id) in &tc.resolved.definitions {
        if key.start > end || key.end < start {
            continue;
        }
        let Some(symbol) = tc.symbols.try_get(symbol_id) else {
            continue;
        };
        if symbol.kind != SymbolKind::Local {
            continue;
        }
        if explicitly_annotated(text.as_bytes(), key.end) {
            continue;
        }
        let Some(ty) = tc.type_info.decl_type(symbol_id) else {
            continue;
        };
        if matches!(
            ty,
            ArType::Void | ArType::Error | ArType::Err | ArType::IntLiteral | ArType::FloatLiteral
        ) {
            continue;
        }
        let label = display_type(&tc, &ty);
        if label.is_empty() {
            continue;
        }
        hints.push(InlayHint {
            position: offset_to_position(&index, key.end),
            label: InlayHintLabel::String(format!(": {label}")),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        });
    }
    hints.sort_by(|a, b| {
        use std::cmp::Ordering;
        match (a.position.line, a.position.character).cmp(&(b.position.line, b.position.character))
        {
            Ordering::Equal => label_text(&a.label).cmp(label_text(&b.label)),
            order => order,
        }
    });
    hints.dedup_by(|a, b| a.position == b.position && label_text(&a.label) == label_text(&b.label));
    hints
}

/// True when the declaration is followed by an explicit `: Type` annotation.
fn explicitly_annotated(text: &[u8], end: u32) -> bool {
    let mut i = end as usize;
    while i < text.len() {
        match text[i] {
            b' ' | b'\t' => i += 1,
            b':' => return true,
            _ => return false,
        }
    }
    false
}

fn label_text(label: &InlayHintLabel) -> &str {
    match label {
        InlayHintLabel::String(text) => text,
        InlayHintLabel::LabelParts(_) => "",
    }
}
