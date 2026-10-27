#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RFC 0042 phase 2: the AST of one item, lowered from its CST subtree, has
//! the positions of the file's AST shifted to the item start.

use arandu_base::Span;
use arandu_parser::syntax::lower_syntax_to_program_recovering;
use arandu_parser::{Program, parse_syntax};
use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crate should be under workspace/crates")
        .to_path_buf()
}

fn collect_aru_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_aru_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "aru") {
            files.push(path);
        }
    }
}

fn corpus() -> Vec<(PathBuf, String)> {
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in ["examples", "stdlib", "tests"] {
        collect_aru_files(&root.join(dir), &mut files);
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|path| Some((path.clone(), fs::read_to_string(&path).ok()?)))
        .collect()
}

fn pool_spans(program: &Program) -> Vec<Span> {
    let pool = &program.pool;
    let mut spans: Vec<Span> = pool
        .expr_spans
        .iter()
        .chain(&pool.stmt_spans)
        .chain(&pool.type_expr_spans)
        .chain(&pool.decl_spans)
        .copied()
        .collect();
    spans.sort();
    spans
}

#[test]
fn item_trees_lower_to_the_file_ast_shifted_to_the_item_start() {
    const FILE_ID: u32 = 7;
    let mut items = 0;
    let mut compared = 0;
    let mut fresh_equal = 0;
    for (path, source) in corpus() {
        let tree = parse_syntax(&source);
        let file = lower_syntax_to_program_recovering(&tree, FILE_ID);
        let clean = file.diagnostics.is_empty();
        let ranges = tree.item_ranges();
        let mut file_spans = pool_spans(&file.program);

        for (index, &(start, end)) in ranges.iter().enumerate() {
            items += 1;
            let item_tree = tree.item_tree(index).expect("item tree");
            assert_eq!(item_tree.text(), &source[start as usize..end as usize]);
            let local = lower_syntax_to_program_recovering(&item_tree, FILE_ID);
            if !clean {
                continue;
            }
            assert!(
                local.diagnostics.is_empty(),
                "{}: item {index}: {:?}",
                path.display(),
                local.diagnostics
            );
            // Same AST as lexing the item text on its own: the rebased tokens
            // and green subtree are a faithful image of the item. Only when the
            // fresh CST also makes the whole text one item: leading doc
            // comments of a file are root trivia, so they may fall outside.
            let item_text = &source[start as usize..end as usize];
            let fresh_tree = parse_syntax(item_text);
            if fresh_tree.item_ranges() == [(0, end - start)] {
                let fresh = lower_syntax_to_program_recovering(&fresh_tree, FILE_ID);
                assert!(
                    local.program.pool == fresh.program.pool
                        && local.program.decls == fresh.program.decls,
                    "{}: item {index} differs from a fresh parse of its text",
                    path.display()
                );
                fresh_equal += 1;
            }
            // Every node position, shifted by the item start, is a position
            // of the file's AST. Synthetic nodes carry the `Span::new(0, 0, 0)`
            // placeholder, which is not a position and is never shifted.
            let shifted: Vec<Span> = pool_spans(&local.program)
                .into_iter()
                .map(|span| {
                    if span.file_id == FILE_ID {
                        Span::new(span.file_id, span.start + start, span.end + start)
                    } else {
                        assert_eq!(span, Span::new(0, 0, 0), "{}", path.display());
                        span
                    }
                })
                .collect();
            for span in shifted {
                let Ok(found) = file_spans.binary_search(&span) else {
                    panic!(
                        "{}: item {index}: {span:?} ({:?}) is not a file position",
                        path.display(),
                        &source[span.start as usize..span.end as usize]
                    );
                };
                file_spans.remove(found);
            }
            compared += 1;
        }
        if clean {
            // Every top-level node of the file belongs to exactly one item.
            assert!(
                file_spans.iter().all(|span| !ranges
                    .iter()
                    .any(|&(s, e)| s <= span.start && span.end <= e)),
                "{}: file positions left unmatched inside items: {file_spans:?}",
                path.display()
            );
        }
    }
    assert!(
        compared > 500 && fresh_equal > compared / 2,
        "compared {compared} of {items} items, {fresh_equal} against a fresh parse"
    );
}

#[test]
fn item_tree_out_of_range_is_none() {
    let tree = parse_syntax("func main(): int {\n    return 0\n}\n");
    assert!(tree.item_tree(0).is_some());
    assert!(tree.item_tree(1).is_none());
}
