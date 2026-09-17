//! IDE analysis helpers over a frozen [`arandu_query::AnalysisSnapshot`] (P4).
//!
//! Pure queries on typeck/resolve results — no Salsa writes.

pub mod code_actions;
pub mod completion;
pub mod formatting;
pub mod hover;
pub mod inlay_hints;
pub mod navigation;
pub mod presentation;
pub mod rename;
pub mod semantic_tokens;
pub mod signature_help;
pub mod symbols;
pub mod types;

pub use code_actions::code_actions;
pub use completion::completions;
pub use formatting::{format_document, format_range};
pub use hover::hover;
pub use inlay_hints::inlay_hints;
pub use navigation::{
    document_highlights, folding_ranges, references, selection_ranges, type_definition_symbol,
};
pub use presentation::expr_symbol_at;
#[cfg(test)]
pub use presentation::{prefix_at, symbol_at, typecheck};
pub use rename::{prepare_rename, rename_edits};
pub use semantic_tokens::{semantic_tokens, semantic_tokens_legend, semantic_tokens_range};
pub use signature_help::signature_help;
pub use symbols::{document_symbols, workspace_symbols};
pub use types::{DiagnosticData, DiagnosticFixData, DocSnap};

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_base::LineIndex;
    use arandu_query::AnalysisHost;
    use lsp_types::{
        DocumentHighlightKind, Documentation, FoldingRangeKind, HoverContents, InlayHintKind,
        InsertTextFormat, MarkupContent, ParameterLabel, Position,
    };

    use crate::conv::{offset_to_position, utf16_len};

    #[test]
    fn annotation_completion_uses_canonical_names_and_snippets() {
        let text = "@Li\nextern \"C\" {}\n";
        let mut host = AnalysisHost::new();
        let file = host.new_file("annotation.aru".into(), text.into());
        let snap = host.snapshot();
        let items = completions(&snap, file, text, Position::new(0, 3));
        let link = items
            .iter()
            .find(|item| item.label == "Link")
            .expect("Link completion");
        assert_eq!(link.insert_text.as_deref(), Some("Link(\"${1:library}\")"));
        assert_eq!(link.insert_text_format, Some(InsertTextFormat::SNIPPET));
        assert!(items.iter().all(|item| item.label != "link"));
    }

    #[test]
    fn annotation_completion_filters_by_target() {
        let text = "@\nfunc critical() {}\n";
        let mut host = AnalysisHost::new();
        let file = host.new_file("annotation-target.aru".into(), text.into());
        let snap = host.snapshot();
        let items = completions(&snap, file, text, Position::new(0, 1));
        assert!(items.iter().any(|item| item.label == "NoFallback"));
        assert!(items.iter().any(|item| item.label == "Test"));
        assert!(items.iter().any(|item| item.label == "Benchmark"));
        assert!(items.iter().all(|item| item.label != "Link"));
        assert!(items.iter().all(|item| item.label != "Specialize"));
    }

    #[test]
    fn annotation_hover_exposes_only_the_canonical_contract() {
        let text = "@no_fallback\nfunc critical() {}\n";
        let mut host = AnalysisHost::new();
        let file = host.new_file("annotation-hover.aru".into(), text.into());
        let snap = host.snapshot();
        let hover = hover(&snap, file, text, Position::new(0, 4)).expect("annotation hover");
        let HoverContents::Markup(content) = hover.contents else {
            panic!("expected markdown hover");
        };
        assert!(content.value.contains("@NoFallback"));
        assert!(content.value.contains("deprecated"));
        assert!(!content.value.contains("AnnotationId"));
        assert!(!content.value.contains("no_fallback`"));
    }

    #[test]
    fn prefix_at_identifier() {
        assert_eq!(prefix_at("let foo_bar = 1", 11), "foo_bar");
        assert_eq!(prefix_at("io.", 3), "");
    }

    #[test]
    fn completions_include_func_and_keyword() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("h.aru".into(), "func main(): int { return 1 }\n".into());
        let snap = host.snapshot();
        let text = file.text(&snap.db);
        let items = completions(
            &snap,
            file,
            text,
            Position {
                line: 0,
                character: text.len() as u32,
            },
        );
        assert!(
            items.iter().any(|i| i.label == "func" || i.label == "main"),
            "expected keyword or main in completions, got {} items",
            items.len()
        );
    }

    #[test]
    fn unicode_position_resolves_symbol_after_astral_character() {
        let text = "/* 😀 */ func soma(value: int): int { return value } // ação\n";
        let mut host = AnalysisHost::new();
        let file = host.new_file("unicode.aru".into(), text.into());
        let snap = host.snapshot();
        let byte_offset = text.find("soma").expect("soma definition") + 1;
        let position = offset_to_position(
            &LineIndex::new(text),
            u32::try_from(byte_offset).expect("fixture offset fits u32"),
        );
        let tc = typecheck(&snap, file);
        assert!(
            symbol_at(
                &tc,
                u32::try_from(byte_offset).expect("fixture offset fits u32")
            )
            .is_some(),
            "resolved maps: definitions={:?}, values={:?}",
            tc.resolved.definitions,
            tc.resolved.value_refs
        );
        assert!(hover(&snap, file, text, position).is_some());
    }

    #[test]
    fn signature_help_is_safe_for_an_empty_document() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("empty.aru".into(), String::new());
        let snap = host.snapshot();
        assert!(signature_help(&snap, file, "", Position::new(0, 0)).is_none());
    }

    #[test]
    fn hover_completion_and_signature_share_user_facing_presentation() {
        let text = concat!(
            "/// Adds two values.\n",
            "/// Keeps integer precision.\n",
            "func add(left: int, right: int): int { return left + right }\n",
            "func main(): int { return add(1, 2) }\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("presentation.aru".into(), text.into());
        let snap = host.snapshot();
        let signature = "func add(left: int, right: int): int";
        let documentation = "Adds two values.\nKeeps integer precision.";
        let definition = text.find("add").expect("add definition");
        let call = text.rfind("add").expect("add call");

        let hover = hover(
            &snap,
            file,
            text,
            offset_to_position(&LineIndex::new(text), definition as u32),
        )
        .expect("hover for add");
        let HoverContents::Markup(hover) = hover.contents else {
            panic!("hover must use Markdown markup");
        };
        assert!(hover.value.contains(signature));
        assert!(hover.value.contains(documentation));
        assert!(!hover.value.contains("SymbolId"));

        let completion = completions(
            &snap,
            file,
            text,
            offset_to_position(&LineIndex::new(text), (definition + 2) as u32),
        )
        .into_iter()
        .find(|item| item.label == "add")
        .expect("add completion");
        assert_eq!(completion.detail.as_deref(), Some(signature));
        assert!(matches!(
            completion.documentation,
            Some(Documentation::MarkupContent(MarkupContent { value, .. }))
                if value == documentation
        ));

        let second_argument = call + "add(1, ".len();
        let help = signature_help(
            &snap,
            file,
            text,
            offset_to_position(&LineIndex::new(text), second_argument as u32),
        )
        .expect("signature help for add");
        let shown = &help.signatures[0];
        assert_eq!(shown.label, signature);
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(shown.active_parameter, Some(1));
        assert_eq!(
            shown.parameters.as_ref().expect("parameter labels")[1].label,
            ParameterLabel::Simple("right: int".into())
        );
        assert!(matches!(
            shown.documentation,
            Some(Documentation::MarkupContent(MarkupContent { ref value, .. }))
                if value == documentation
        ));
    }

    #[test]
    fn signature_context_ignores_nested_argument_commas() {
        let text = concat!(
            "func add(left: int, right: int): int { return left + right }\n",
            "func main(): int { return add(add(1, 2), 3) }\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("nested-signature.aru".into(), text.into());
        let snap = host.snapshot();
        let outer_second = text.rfind(", 3").expect("outer second argument") + 2;
        let context = signature_help::call_context(&snap, file, outer_second as u32)
            .expect("outer call context");
        assert_eq!(context.name, "add");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn import_path_completions_std() {
        let mut host = AnalysisHost::new();
        // Cursor after `import std.`
        let text = "import std.\n";
        let file = host.new_file("imp.aru".into(), text.into());
        let snap = host.snapshot();
        let items = completions(
            &snap,
            file,
            text,
            Position {
                line: 0,
                character: "import std.".len() as u32,
            },
        );
        assert!(
            items
                .iter()
                .any(|i| i.label == "core" || i.label == "alloc"),
            "expected std.core/alloc path segments, got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn import_path_completions_root() {
        let text = "import \n";
        let items = completion::import_path_completions(text, text.len() as u32 - 1, "")
            .expect("import root completions");
        assert!(
            items.iter().any(|i| i.label == "std"),
            "expected std root, got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn semantic_tokens_from_cst_nonempty() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("st.aru".into(), "func main(): int { return 1 }\n".into());
        let snap = host.snapshot();
        let tokens = semantic_tokens(&snap, file);
        assert!(
            !tokens.data.is_empty(),
            "expected semantic tokens from CST keywords/idents"
        );
    }

    #[test]
    fn test_semantic_tokens_exact_deltas() {
        // Resolve via CARGO_MANIFEST_DIR so CI/macOS/other checkouts work
        // (never hard-code a developer machine path).
        let filepath = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std/net.aru")
            .canonicalize()
            .expect("resolve stdlib/std/net.aru from workspace");
        let content = std::fs::read_to_string(&filepath).expect("read net.aru");
        let path_key = filepath.to_string_lossy().into_owned();

        let mut host = AnalysisHost::new();
        let file = host.new_file(path_key, content.clone());
        let snap = host.snapshot();

        let tokens = semantic_tokens(&snap, file);

        // Reconstruct absolute character offsets from LSP deltas and verify
        // they match the highlight spans from the query layer.
        let mut current_line = 0u32;
        let hls = arandu_query::file_highlights(&snap.db, file);
        assert_eq!(tokens.data.len(), hls.len());

        for (i, tok) in tokens.data.iter().enumerate() {
            if tok.delta_line > 0 {
                current_line += tok.delta_line;
            }

            let hl = hls[i];
            assert!(hl.end <= content.len() as u32);
            let substring = &content[hl.start as usize..hl.end as usize];

            // Semantic token lengths use negotiated UTF-16 code units, not bytes.
            assert_eq!(tok.length, utf16_len(substring));

            // Spot-check: `tcpListen` public decl in stdlib is a FUNCTION token.
            // Line is 0-based (LSP semantic tokens); file line 52 → index 51.
            if substring == "tcpListen" && current_line == 51 {
                assert_eq!(tok.token_type, 1); // FUNCTION
            }
        }
    }

    #[test]
    fn semantic_tokens_split_multiline_unicode_without_newlines() {
        let text = "/* ação\r\n😀 fim */";
        let highlight = arandu_query::HlToken {
            start: 0,
            end: u32::try_from(text.len()).expect("fixture length fits u32"),
            kind: arandu_query::HlKind::Comment,
            mods: 0,
        };

        let tokens = semantic_tokens::encode_highlights(&[highlight], text);
        assert_eq!(tokens.data.len(), 2);
        assert_eq!(tokens.data[0].delta_line, 0);
        assert_eq!(tokens.data[0].delta_start, 0);
        assert_eq!(tokens.data[0].length, utf16_len("/* ação"));
        assert_eq!(tokens.data[1].delta_line, 1);
        assert_eq!(tokens.data[1].delta_start, 0);
        assert_eq!(tokens.data[1].length, utf16_len("😀 fim */"));
    }

    #[test]
    fn document_symbols_does_not_panic() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("h.aru".into(), "func main(): int { return 1 }\n".into());
        let snap = host.snapshot();
        let text = file.text(&snap.db);
        let uri = crate::uri_util::parse_uri("file:///h.aru").expect("uri");
        let _syms = document_symbols(&snap, file, text, &uri);
    }

    #[test]
    fn folding_ranges_come_from_multiline_cst_blocks_and_comments() {
        let text = concat!(
            "/** first\nsecond */\n",
            "func main(): int {\n",
            "    if true {\n",
            "        return 1\n",
            "    }\n",
            "    return 0\n",
            "}\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("folding.aru".into(), text.into());
        let snap = host.snapshot();
        let ranges = folding_ranges(&snap, file, text);
        assert!(ranges.iter().any(|range| {
            range.kind == Some(FoldingRangeKind::Comment)
                && range.start_line == 0
                && range.end_line == 1
        }));
        assert!(ranges
            .iter()
            .any(|range| { range.kind.is_none() && range.start_line == 2 && range.end_line == 7 }));
        assert!(ranges
            .iter()
            .any(|range| { range.kind.is_none() && range.start_line == 3 && range.end_line == 5 }));
        assert!(ranges.iter().all(|range| range.start_line < range.end_line));
    }

    #[test]
    fn selection_ranges_are_strictly_nested_cst_ancestors() {
        let text = "func main(): int {\n    return 1 + 2\n}\n";
        let mut host = AnalysisHost::new();
        let file = host.new_file("selection.aru".into(), text.into());
        let snap = host.snapshot();
        let offset = text.find('1').expect("integer expression");
        let position = offset_to_position(
            &LineIndex::new(text),
            u32::try_from(offset).expect("fixture offset fits u32"),
        );
        let ranges = selection_ranges(&snap, file, text, &[position]);
        assert_eq!(ranges.len(), 1);
        let mut current = &ranges[0];
        let mut depth = 1;
        while let Some(parent) = current.parent.as_deref() {
            assert!(parent.range.start <= current.range.start);
            assert!(parent.range.end >= current.range.end);
            assert_ne!(parent.range, current.range);
            current = parent;
            depth += 1;
        }
        assert!(
            depth >= 4,
            "expected token, expr, stmt, block and item ranges"
        );
    }

    #[test]
    fn document_highlights_share_resolved_symbol_identity() {
        let text = concat!(
            "func add(value: int): int { return value }\n",
            "func main(): int { return add(1) }\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("highlights.aru".into(), text.into());
        let snap = host.snapshot();
        let definition = text.find("add").expect("function definition");
        let position = offset_to_position(
            &LineIndex::new(text),
            u32::try_from(definition).expect("fixture offset fits u32"),
        );
        let highlights = document_highlights(&snap, file, text, position);
        assert_eq!(highlights.len(), 2);
        assert!(highlights
            .iter()
            .all(|highlight| highlight.kind == Some(DocumentHighlightKind::TEXT)));
    }

    #[test]
    fn type_definition_symbol_resolves_value_to_named_type() {
        use arandu_middle::SymbolKind;
        use navigation::type_definition_symbol;

        let text = concat!(
            "struct Pair {\n",
            "    left: int\n",
            "    right: int\n",
            "}\n",
            "func make(): Pair {\n",
            "    return Pair { left: 1, right: 2 }\n",
            "}\n",
            "func main(): int {\n",
            "    let p = make()\n",
            "    return p.left\n",
            "}\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("typedef.aru".into(), text.into());
        let snap = host.snapshot();
        let tc = typecheck(&snap, file);
        let p_idx = text.find("let p").expect("binding") + 4;
        let offset = u32::try_from(p_idx).unwrap();
        let sym = symbol_at(&tc, offset).expect("binding symbol");
        let target = type_definition_symbol(&tc, sym).expect("underlying type symbol");
        let def = tc.symbols.try_get(target).expect("target symbol");
        assert_eq!(def.kind, SymbolKind::Struct);
        assert_eq!(def.name.as_str(), "Pair");
    }

    #[test]
    fn inlay_hints_show_inferred_types_for_unannotated_lets() {
        use lsp_types::{InlayHintLabel, Position, Range};

        let text = concat!(
            "struct Pair {\n",
            "    left: int\n",
            "    right: int\n",
            "}\n",
            "func make(): Pair {\n",
            "    return Pair { left: 1, right: 2 }\n",
            "}\n",
            "func main(): int {\n",
            "    let p = make()\n",
            "    return p.left\n",
            "}\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("inlay.aru".into(), text.into());
        let snap = host.snapshot();
        let range = Range::new(Position::new(0, 0), Position::new(11, 0));
        let hints = inlay_hints(&snap, file, text, range);
        let pair = hints
            .iter()
            .find(|hint| hint.position == Position::new(8, 9))
            .expect("hint after `p`");
        match &pair.label {
            InlayHintLabel::String(label) => assert_eq!(label, ": Pair"),
            InlayHintLabel::LabelParts(parts) => {
                panic!("expected plain label for Pair, got parts: {parts:?}")
            }
        }
        assert_eq!(pair.kind, Some(InlayHintKind::TYPE));
        assert!(hints.iter().all(|hint| hint.position.line == 8));
    }

    #[test]
    fn format_range_limits_edits_to_the_selected_item() {
        use crate::conv::position_to_offset;
        use arandu_base::LineIndex;

        let text = concat!(
            "struct Pair {\n",
            "left:int\n",
            "right:int\n",
            "}\n",
            "func main(): int {\n",
            "return 1   \n",
            "}\n",
        );
        let index = LineIndex::new(text);
        let range = lsp_types::Range::new(Position::new(0, 0), Position::new(3, 0));
        let edits = format_range(text, range);
        assert!(!edits.is_empty(), "struct needs reindentation");
        for edit in &edits {
            assert!(
                edit.range.start.line <= 3 && edit.range.end.line <= 3,
                "range formatting escaped the selected item: {edits:?}"
            );
        }
        let mut out = text.to_string();
        for edit in edits.iter().rev() {
            let start = position_to_offset(&index, edit.range.start, text) as usize;
            let end = position_to_offset(&index, edit.range.end, text) as usize;
            out.replace_range(start..end, &edit.new_text);
        }
        assert_eq!(
            out,
            concat!(
                "struct Pair {\n",
                "    left: int\n",
                "    right: int\n",
                "}\n",
                "func main(): int {\n",
                "return 1   \n",
                "}\n",
            )
        );
    }
}
