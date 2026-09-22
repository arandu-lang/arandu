//! CST-first pipeline via [`rowan`] (P5 + F1 structured green + event sink).
//!
//! 1. Lex → **RD with [`events`]** → green (`SOURCE_FILE` / `FUNC_ITEM` / `BLOCK` / `STMT`).
//! 2. Fallback heuristic green if events are unbalanced.
//! 3. Lower: green-guided walk (RD at each item seek) with full-RD fallback; see [`lower`].
//! 4. Subtree reparse via [`reparse_subtree`] for local ITEM edits.

mod build;
pub mod events;
pub mod hand;
mod highlight;
mod incremental;
mod kind;
pub mod lower;

pub use build::{
    SyntaxTree, build_item_green, classify_item_kind, find_top_level_item_spans,
    lower_syntax_to_program, lower_syntax_to_program_rd_only, lower_syntax_to_program_recovering,
    lower_syntax_to_program_recovering_rd_only, map_token_kind, parse_syntax, parse_syntax_arc,
    parse_syntax_with_item_spans, text_range,
};
pub use events::{ParseEvent, build_green_from_events, events_balanced};
pub use highlight::{for_each_highlight_token, highlight_spans};
pub use incremental::{
    apply_text_edit, reparse_edit, reparse_subtree, single_contiguous_edit,
    splice_tokens_for_item_edit,
};
pub use kind::{AranduLanguage, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
pub use lower::{
    GreenStructure, block_stmt_count, first_func_item, func_body_block, inspect_green_structure,
    is_fully_typed_toplevel, lower_from_green,
};

use crate::{ParseError, Program};

/// Single entry: CST-first then lower to AST (no independent dual parse).
pub fn parse_from_cst(source: &str) -> Result<Program, ParseError> {
    parse_from_cst_with_file_id(source, 0)
}

/// CST-first parse with file id for spans.
pub fn parse_from_cst_with_file_id(source: &str, file_id: u32) -> Result<Program, ParseError> {
    let tree = parse_syntax(source);
    lower_syntax_to_program(&tree, file_id)
}

/// CST + AST together (same path as [`parse_from_cst`]; name kept for call sites).
pub fn parse_dual(source: &str) -> (Result<Program, ParseError>, SyntaxTree) {
    let tree = parse_syntax(source);
    let ast = lower_syntax_to_program(&tree, 0);
    (ast, tree)
}

pub fn parse_dual_with_file_id(
    source: &str,
    file_id: u32,
) -> (Result<Program, ParseError>, SyntaxTree) {
    let tree = parse_syntax(source);
    let ast = lower_syntax_to_program(&tree, file_id);
    (ast, tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_funcs(beta: i32) -> String {
        format!(
            "func alpha(): int {{\n    return 1\n}}\n\nfunc beta(): int {{\n    return {beta}\n}}\n"
        )
    }

    fn main_func() -> &'static str {
        "func main(): int {\n    return 1\n}\n"
    }

    fn main_func_oneline() -> &'static str {
        "func main(): int { return 1 }\n"
    }

    #[test]
    fn cst_first_builds_item_nodes_without_ast() {
        let src = two_funcs(2);
        let tree = parse_syntax(&src);
        let items = tree.items();
        assert!(
            items.len() >= 2,
            "expected ≥2 top-level items from heuristic, got {}",
            items.len()
        );
        assert!(
            items.iter().all(|n| n.kind() == SyntaxKind::FUNC_ITEM),
            "expected FUNC_ITEM nodes, got {:?}",
            items.iter().map(|n| n.kind()).collect::<Vec<_>>()
        );
        let texts = tree.item_texts();
        assert!(texts[0].contains("alpha"), "first: {}", texts[0]);
        assert!(texts.iter().any(|t| t.contains("beta")), "beta missing");
        let blocks = tree.item_blocks();
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| b.is_some()), "each func needs BLOCK");
    }

    #[test]
    fn lower_from_cst_matches_program() {
        let src = two_funcs(2);
        let tree = parse_syntax(&src);
        let prog = lower_syntax_to_program(&tree, 0).expect("lower");
        assert!(prog.decls.len() >= 2);
    }

    #[test]
    fn lower_flat_main_ok() {
        let src = main_func();
        let tree = parse_syntax(src);
        let prog = lower_syntax_to_program(&tree, 100).expect("lower");
        assert!(!prog.decls.is_empty());
        assert!(crate::parse(src).is_ok());
    }

    #[test]
    fn lower_oneline_main_ok_optional_semi_before_rbrace() {
        let src = main_func_oneline();
        let tree = parse_syntax(src);
        let prog = lower_syntax_to_program(&tree, 0).expect("lower oneline");
        assert!(!prog.decls.is_empty());
        assert!(crate::parse(src).is_ok(), "one-line body must parse");
    }

    #[test]
    fn lower_reuses_cst_tokens_without_independent_relex_path() {
        // Documented contract: lower shares Arc tokens (no full-stream Vec clone).
        let src = main_func_oneline();
        let tree = parse_syntax(src);
        let via_lower = lower_syntax_to_program(&tree, 0).expect("lower");
        let via_stream = crate::parse_token_stream(
            tree.text(),
            std::sync::Arc::clone(tree.tokens_arc()),
            0,
            Vec::new(),
        );
        assert!(
            via_stream.diagnostics.is_empty(),
            "{:?}",
            via_stream.diagnostics
        );
        assert_eq!(via_lower.decls.len(), via_stream.program.decls.len());
    }

    #[test]
    fn subtree_reparse_preserves_sibling_item_text() {
        let src1 = two_funcs(2);
        let t1 = parse_syntax(&src1);
        let texts1 = t1.item_texts();
        assert!(texts1.len() >= 2);

        let needle = "return 2";
        let start = src1.find(needle).expect("needle") as u32;
        let end = start + needle.len() as u32;
        let (src2, t2) = reparse_subtree(&t1, start, end, "return 99");
        assert!(src2.contains("return 99"));
        let texts2 = t2.item_texts();
        assert!(texts2.len() >= 2);
        assert_eq!(
            texts1[0].trim(),
            texts2[0].trim(),
            "alpha ITEM text must survive beta-only subtree reparse"
        );
        assert_ne!(texts1[1].trim(), texts2[1].trim());
    }

    #[test]
    fn subtree_reparse_reuses_sibling_green_identity() {
        let src1 = two_funcs(2);
        let t1 = parse_syntax(&src1);
        let alpha1 = t1.items()[0].green().to_owned();

        let needle = "return 2";
        let start = src1.find(needle).expect("needle") as u32;
        let end = start + needle.len() as u32;
        let (_src2, t2) = reparse_subtree(&t1, start, end, "return 99");
        let alpha2 = t2.items()[0].green().to_owned();

        // replace_child clones Arc for untouched children → same green identity.
        let p1: *const rowan::GreenNodeData = &*alpha1;
        let p2: *const rowan::GreenNodeData = &*alpha2;
        assert_eq!(
            p1, p2,
            "unedited ITEM green must be reused (pointer identity)"
        );
    }

    #[test]
    fn subtree_reparse_matches_cold_tree_when_item_boundaries_change() {
        for (before, after) in [
            (
                "public func value(): int { return 1 }\n",
                "import graph.main as root\npublic func value(): int { return root.main() }\n",
            ),
            ("func a() {}\nfunc b() {}\n", "func a() {\nfunc b() {}\n"),
            ("func a() {}\nfunc b() {}\n", "func a() {/*\nfunc b() {}\n"),
        ] {
            let old = parse_syntax(before);
            let (start, end, replacement) = single_contiguous_edit(before, after).expect("edit");
            let (_, edited) = reparse_subtree(&old, start, end, &replacement);
            let cold = parse_syntax(after);
            assert_eq!(
                edited.root().green(),
                cold.root().green(),
                "CST differs after {after:?}"
            );
            assert_eq!(
                edited.tokens(),
                cold.tokens(),
                "token stream differs after {after:?}"
            );
        }
    }

    #[test]
    fn flat_syntax_covers_full_source() {
        let src = main_func();
        let tree = parse_syntax(src);
        assert_eq!(tree.root().text().to_string(), src);
    }

    #[test]
    fn highlight_spans_mark_keywords() {
        let tree = parse_syntax(main_func());
        let spans = highlight_spans(&tree);
        assert!(
            spans.iter().any(|(_, _, c)| *c == "keyword"),
            "expected keyword highlights: {spans:?}"
        );
    }
}
