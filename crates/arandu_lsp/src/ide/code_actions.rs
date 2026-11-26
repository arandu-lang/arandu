//! Code action generation from structured compiler diagnostics.

use std::collections::HashMap;

use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionResponse, NumberOrString, Range,
    TextEdit as LspTextEdit, Uri, WorkspaceEdit,
};

use super::types::DiagnosticData;
use crate::conv::{offset_to_position, position_to_offset};

/// Code actions backed by structured compiler replacements carried in
/// `Diagnostic.data` plus snapshot-backed missing-import quick-fixes for
/// `N001`/`N002` diagnostics; messages are presentation only and are never parsed.
#[must_use]
#[allow(clippy::mutable_key_type)] // WorkspaceEdit keys are `Uri` in lsp-types 0.97
pub fn code_actions_with_snapshot(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    uri: &Uri,
    context: &lsp_types::CodeActionContext,
) -> CodeActionResponse {
    let mut out = code_actions(uri, context);
    let index = LineIndex::new(text);

    for d in &context.diagnostics {
        let is_unresolved = matches!(
            &d.code,
            Some(NumberOrString::String(code)) if code == "N001" || code == "N002"
        );
        if !is_unresolved {
            continue;
        }
        let start = position_to_offset(&index, d.range.start, text) as usize;
        let end = position_to_offset(&index, d.range.end, text) as usize;
        if start >= end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        let ident = text[start..end].trim();
        if ident.is_empty()
            || !ident
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            continue;
        }
        for candidate in arandu_ide::auto_import_candidates_for_name(snap, source, text, ident) {
            let mut changes: HashMap<Uri, Vec<LspTextEdit>> = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![LspTextEdit {
                    range: Range {
                        start: offset_to_position(&index, candidate.edit.start),
                        end: offset_to_position(&index, candidate.edit.end),
                    },
                    new_text: candidate.edit.new_text,
                }],
            );
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!(
                    "Import `{}` from `{}`",
                    candidate.label, candidate.module_path
                ),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![d.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..WorkspaceEdit::default()
                }),
                ..CodeAction::default()
            }));
        }
    }

    out
}

/// Code actions backed by structured compiler replacements carried in
/// `Diagnostic.data`; messages are presentation only and are never parsed.
#[must_use]
#[allow(clippy::mutable_key_type)] // WorkspaceEdit keys are `Uri` in lsp-types 0.97
pub fn code_actions(_uri: &Uri, context: &lsp_types::CodeActionContext) -> CodeActionResponse {
    let mut out = Vec::new();
    for d in &context.diagnostics {
        let Some(data) = d
            .data
            .clone()
            .and_then(|value| serde_json::from_value::<DiagnosticData>(value).ok())
        else {
            continue;
        };
        for fix in data.fixes {
            let mut by_str: HashMap<String, Vec<LspTextEdit>> = HashMap::new();
            by_str.insert(
                fix.uri.as_str().to_string(),
                vec![LspTextEdit {
                    range: fix.range,
                    new_text: fix.new_text,
                }],
            );
            let changes: HashMap<Uri, Vec<LspTextEdit>> = by_str
                .into_iter()
                .filter_map(|(s, edits)| crate::uri_util::parse_uri(&s).map(|u| (u, edits)))
                .collect();
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: fix.title,
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![d.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..WorkspaceEdit::default()
                }),
                ..CodeAction::default()
            }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_query::AnalysisHost;
    use lsp_types::{Diagnostic, Position};

    #[test]
    fn unresolved_type_diagnostic_offers_missing_import_quickfix() {
        let mut host = AnalysisHost::default();
        let text = "module app.demo\n\nfunc main(): void {\n    let m: HashMap<int, int>\n}\n";
        let source = host.new_file("app/demo.aru".into(), text.to_string());
        let snap = host.snapshot();
        let uri = crate::uri_util::parse_uri("file:///app/demo.aru").unwrap();
        let diag = Diagnostic {
            range: Range {
                start: Position::new(3, 11),
                end: Position::new(3, 18),
            },
            code: Some(NumberOrString::String("N002".to_string())),
            message: "type 'HashMap' is not declared".to_string(),
            ..Diagnostic::default()
        };
        let context = lsp_types::CodeActionContext {
            diagnostics: vec![diag],
            ..lsp_types::CodeActionContext::default()
        };
        let actions = code_actions_with_snapshot(&snap, source, text, &uri, &context);
        let titles: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => Some(ca.title.as_str()),
                CodeActionOrCommand::Command(_) => None,
            })
            .collect();
        assert!(
            titles.contains(&"Import `HashMap` from `std.collections`"),
            "expected missing-import quick-fix, got: {titles:?}"
        );
    }
}
