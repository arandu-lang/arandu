//! Inlay type and parameter hints (LSP `textDocument/inlayHint`).

use arandu_base::LineIndex;
use arandu_middle::types::ArType;
use arandu_middle::SymbolKind;
use arandu_parser::{ExprId, ExprKind};
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};

use super::presentation::{display_type, typecheck};
use crate::conv::{offset_to_position, position_to_offset};

/// Inlay hints for unannotated `let` bindings and call parameter names inside `range`.
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

    // 1. Inlay type hints for unannotated local let bindings.
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

    // 2. Inlay parameter name hints for function and method call arguments.
    let program_res = arandu_query::passes::parse(&snap.db, source);
    if let Ok(program) = &**program_res {
        for i in 0..program.pool.exprs.len() {
            let expr_id = ExprId::new(i);
            let span = program.pool.expr_span(expr_id);
            if span.start > end || span.end < start {
                continue;
            }
            if let ExprKind::Call { callee, args, .. } = program.pool.expr(expr_id) {
                let callee_span = program.pool.expr_span(*callee);
                let callee_symbol_id = tc
                    .resolved
                    .expr_symbols
                    .get(callee.as_usize())
                    .copied()
                    .flatten()
                    .or_else(|| arandu_ide::presentation::symbol_at(&tc, callee_span.start));
                let Some(sym_id) = callee_symbol_id else {
                    continue;
                };
                let Some(symbol) = tc.symbols.try_get(sym_id) else {
                    continue;
                };
                if !matches!(
                    symbol.kind,
                    SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
                ) {
                    continue;
                }
                let param_names = arandu_ide::presentation::function_parameter_names(snap, source, symbol);
                if param_names.is_empty() {
                    continue;
                }
                let is_method_call = matches!(program.pool.expr(*callee), ExprKind::Field { .. });
                let param_offset = if is_method_call && param_names.first().is_some_and(|n| n == "self") {
                    1
                } else {
                    0
                };
                let arg_ids = program.pool.expr_list(*args);
                for (arg_idx, &arg_id) in arg_ids.iter().enumerate() {
                    let target_param_idx = arg_idx + param_offset;
                    let Some(param_name) = param_names.get(target_param_idx) else {
                        continue;
                    };
                    if param_name == "self" || param_name.is_empty() {
                        continue;
                    }
                    let arg_span = program.pool.expr_span(arg_id);
                    if has_explicit_label(text.as_bytes(), arg_span.start) {
                        continue;
                    }
                    if let ExprKind::Path { path } = program.pool.expr(arg_id) {
                        if path.len() == 1 && path[0].as_str() == param_name.as_str() {
                            continue;
                        }
                    }
                    hints.push(InlayHint {
                        position: offset_to_position(&index, arg_span.start),
                        label: InlayHintLabel::String(format!("{param_name}:")),
                        kind: Some(InlayHintKind::PARAMETER),
                        text_edits: None,
                        tooltip: None,
                        padding_left: None,
                        padding_right: Some(true),
                        data: None,
                    });
                }
            }
        }
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

/// True when the argument at `start` is preceded by an explicit argument label `name:`.
fn has_explicit_label(text: &[u8], start: u32) -> bool {
    let mut i = start as usize;
    while i > 0 && matches!(text[i - 1], b' ' | b'\t' | b'\n' | b'\r') {
        i -= 1;
    }
    i > 0 && text[i - 1] == b':'
}

fn label_text(label: &InlayHintLabel) -> &str {
    match label {
        InlayHintLabel::String(text) => text,
        InlayHintLabel::LabelParts(_) => "",
    }
}
