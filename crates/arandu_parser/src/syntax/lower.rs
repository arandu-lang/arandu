//! Green-guided lower: walk typed top-level items; prefer hand-lower, RD fallback.
//!
//! Hand-lower implementation lives in [`super::hand`]. This module owns the walk
//! over green `*_ITEM` nodes, structure inspection helpers, and integration tests.

use super::SyntaxTree;
use super::hand;
use super::kind::{SyntaxKind, SyntaxNode};
use crate::parser::{ParseError, ParseOutput, Parser};
use crate::{Program, TopLevelDecl};
use std::sync::Arc;

/// Summary of structured green content (no heap AST).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GreenStructure {
    pub top_level_items: usize,
    pub func_items: usize,
    pub struct_items: usize,
    pub blocks: usize,
    pub stmts: usize,
    pub typed_items: usize,
}

/// Walk the CST and count structured nodes.
#[must_use]
pub fn inspect_green_structure(tree: &SyntaxTree) -> GreenStructure {
    let mut s = GreenStructure::default();
    for item in tree.items() {
        s.top_level_items += 1;
        if item.kind() != SyntaxKind::ITEM {
            s.typed_items += 1;
        }
        match item.kind() {
            SyntaxKind::FUNC_ITEM => s.func_items += 1,
            SyntaxKind::STRUCT_ITEM => s.struct_items += 1,
            _ => {}
        }
        for n in item.descendants() {
            match n.kind() {
                SyntaxKind::BLOCK => s.blocks += 1,
                SyntaxKind::STMT => s.stmts += 1,
                _ => {}
            }
        }
    }
    s
}

/// True when every top-level child is a typed `*_ITEM` (not generic `ITEM`).
#[must_use]
pub fn is_fully_typed_toplevel(tree: &SyntaxTree) -> bool {
    let items = tree.items();
    !items.is_empty() && items.iter().all(|n| n.kind() != SyntaxKind::ITEM)
}

/// First `FUNC_ITEM` node, if any.
#[must_use]
pub fn first_func_item(tree: &SyntaxTree) -> Option<SyntaxNode> {
    tree.items()
        .into_iter()
        .find(|n| n.kind() == SyntaxKind::FUNC_ITEM)
}

/// Body `BLOCK` of a function item node.
#[must_use]
pub fn func_body_block(func: &SyntaxNode) -> Option<SyntaxNode> {
    func.children().find(|n| n.kind() == SyntaxKind::BLOCK)
}

/// Count `STMT` children inside a `BLOCK`.
#[must_use]
pub fn block_stmt_count(block: &SyntaxNode) -> usize {
    block
        .children()
        .filter(|n| n.kind() == SyntaxKind::STMT)
        .count()
}

/// Green-guided lower: hand-lower each item when possible, else RD at item seek.
///
/// Falls back to full linear `parse_token_stream` if the walk cannot complete cleanly.
pub fn lower_from_green(tree: &SyntaxTree, file_id: u32) -> Result<Program, ParseError> {
    let output = lower_from_green_recovering(tree, file_id);
    if let Some(err) = output.diagnostics.into_iter().next() {
        Err(err)
    } else {
        Ok(output.program)
    }
}

/// Recovering green-guided lower (keeps diagnostics).
#[must_use]
pub fn lower_from_green_recovering(tree: &SyntaxTree, file_id: u32) -> ParseOutput {
    let mut lex_diags: Vec<ParseError> = tree
        .lex_diagnostics()
        .iter()
        .copied()
        .map(|err| ParseError::from_lex(err, file_id))
        .collect();

    let items = tree.items();
    if items.is_empty() {
        return crate::parser::parse_token_stream(
            tree.text(),
            Arc::clone(tree.tokens_arc()),
            file_id,
            lex_diags,
        );
    }

    let mut parser = Parser::new(tree.text(), Arc::clone(tree.tokens_arc())).with_file_id(file_id);
    let start = parser.mark();
    let mut module = None;
    let mut imports = Vec::new();
    let mut decls = Vec::new();
    let mut walk_ok = true;

    for item in &items {
        let off = u32::from(item.text_range().start());
        parser.seek_to_item_start(off);
        parser.skip_semicolons();
        parser.collect_doc_comments();

        match item.kind() {
            SyntaxKind::MODULE_ITEM => {
                if let Some(m) =
                    hand::try_hand_lower_module(tree.text(), tree.tokens(), item, file_id)
                {
                    let docs = parser.take_pending_docs();
                    parser.attach_docs(docs, m.span);
                    seek_past(&mut parser, item);
                    module = Some(m);
                } else {
                    match parser.parse_module() {
                        Ok(m) => module = Some(m),
                        Err(err) => {
                            parser.report_error(err);
                            parser.synchronize_top_level();
                            walk_ok = false;
                        }
                    }
                }
            }
            SyntaxKind::IMPORT_ITEM => {
                if let Some(import) =
                    hand::try_hand_lower_import(tree.text(), tree.tokens(), item, file_id)
                {
                    let docs = parser.take_pending_docs();
                    parser.attach_docs(docs, import.span());
                    seek_past(&mut parser, item);
                    imports.push(import);
                } else {
                    match parser.parse_import() {
                        Ok(import) => imports.push(import),
                        Err(err) => {
                            parser.report_error(err);
                            parser.synchronize_top_level();
                            walk_ok = false;
                        }
                    }
                }
            }
            SyntaxKind::FUNC_ITEM
            | SyntaxKind::STRUCT_ITEM
            | SyntaxKind::ENUM_ITEM
            | SyntaxKind::INTERFACE_ITEM
            | SyntaxKind::CONST_ITEM
            | SyntaxKind::TYPE_ALIAS_ITEM
            | SyntaxKind::EXTERN_ITEM
            | SyntaxKind::IMPL_ITEM
            | SyntaxKind::SUBMODULE_ITEM
            | SyntaxKind::ITEM => {
                let checkpoint = parser.pool.checkpoint();
                if let Some(decl) = hand::try_hand_lower_top_level(
                    &mut parser.pool,
                    tree.text(),
                    tree.tokens(),
                    item,
                    file_id,
                ) {
                    let docs = parser.take_pending_docs();
                    parser.attach_docs(docs, decl.span());
                    seek_past(&mut parser, item);
                    let decl_id = parser.pool.alloc_decl(decl);
                    decls.push(decl_id);
                } else {
                    // Failed hand lowering may have allocated a partial body.
                    // Discard it before RD retries the same canonical tokens.
                    checkpoint.rollback(&mut parser.pool);
                    match parser.parse_top_level_decls() {
                        Ok(parsed_decls) => {
                            for decl in parsed_decls {
                                let decl_id = parser.pool.alloc_decl(decl);
                                decls.push(decl_id);
                            }
                        }
                        Err(err) => {
                            parser.report_error(err);
                            parser.synchronize_top_level();
                            walk_ok = false;
                        }
                    }
                }
            }
            _ => {
                walk_ok = false;
            }
        }
    }

    let decl_like = items
        .iter()
        .filter(|n| !matches!(n.kind(), SyntaxKind::MODULE_ITEM | SyntaxKind::IMPORT_ITEM))
        .count();
    let import_like = items
        .iter()
        .filter(|n| n.kind() == SyntaxKind::IMPORT_ITEM)
        .count();
    // An `impl` CST item deliberately lowers to one function declaration per
    // member, so declaration cardinality is not one-to-one for those items.
    let has_impl = items.iter().any(|n| n.kind() == SyntaxKind::IMPL_ITEM);
    let need_fallback = !walk_ok
        || !parser.diagnostics.is_empty()
        || (!has_impl && decls.len() != decl_like)
        || imports.len() != import_like
        || (items.iter().any(|n| n.kind() == SyntaxKind::MODULE_ITEM) && module.is_none());

    if need_fallback {
        return crate::parser::parse_token_stream(
            tree.text(),
            Arc::clone(tree.tokens_arc()),
            file_id,
            lex_diags,
        );
    }

    let program = Program {
        span: parser.span_from_mark(start),
        module,
        imports,
        interface_impls: std::mem::take(&mut parser.interface_impls),
        decls,
        docs: std::mem::take(&mut parser.docs),
        pool: std::mem::take(&mut parser.pool),
    };

    lex_diags.extend(std::mem::take(&mut parser.diagnostics));
    ParseOutput {
        program,
        diagnostics: lex_diags,
    }
}

#[inline]
fn seek_past(parser: &mut Parser<'_>, item: &SyntaxNode) {
    let end = u32::from(item.text_range().end());
    parser.seek_to_byte(end);
}

/// Number of top-level decls that are not `Error` shells.
#[must_use]
pub fn decl_count(program: &Program) -> usize {
    program
        .decls
        .iter()
        .filter(|id| !matches!(program.pool.decl(**id), TopLevelDecl::Error(_)))
        .count()
}

/// Re-export hand-lower entry points and coverage counters.
pub use super::hand::{
    count_hand_lowerable_decls, count_hand_lowerable_funcs, count_hand_lowerable_stmts,
    try_hand_lower_block, try_hand_lower_func_item, try_hand_lower_import, try_hand_lower_module,
    try_hand_lower_stmt, try_hand_lower_top_level,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stmt;
    use crate::syntax::parse_syntax;

    #[test]
    fn structured_func_has_block_and_stmts() {
        let tree = parse_syntax("func main(): int {\n    return 1\n}\n");
        let s = inspect_green_structure(&tree);
        assert_eq!(s.func_items, 1);
        assert_eq!(s.blocks, 1);
        assert!(
            s.stmts >= 1,
            "expected STMT under BLOCK for return, got {s:?}"
        );
        assert!(is_fully_typed_toplevel(&tree));
        let f = first_func_item(&tree).expect("func");
        let b = func_body_block(&f).expect("block");
        assert!(block_stmt_count(&b) >= 1);
    }

    #[test]
    fn two_funcs_two_blocks() {
        let src = "func a(): int { return 1 }\nfunc b(): int { return 2 }\n";
        let tree = parse_syntax(src);
        let s = inspect_green_structure(&tree);
        assert_eq!(s.func_items, 2);
        assert_eq!(s.blocks, 2);
        assert!(s.stmts >= 2);
    }

    #[test]
    fn struct_item_has_block() {
        let tree = parse_syntax("struct Point {\n    x: int\n}\n");
        let s = inspect_green_structure(&tree);
        assert_eq!(s.struct_items, 1);
        assert!(s.blocks >= 1);
    }

    #[test]
    fn green_guided_lower_matches_full_rd() {
        let src = "func alpha(): int {\n    return 1\n}\n\nfunc beta(): int {\n    return 2\n}\n";
        let tree = parse_syntax(src);
        let via_walk = lower_from_green(&tree, 0).expect("walk lower");
        let via_rd = crate::syntax::build::lower_syntax_to_program_rd_only(&tree, 0).expect("rd");
        assert_eq!(via_walk.decls.len(), via_rd.decls.len());
        assert_eq!(decl_count(&via_walk), 2);
    }

    #[test]
    fn green_guided_lower_module_import_func() {
        let src = "module m\nimport io as io\nfunc main(): int { return 0 }\n";
        let tree = parse_syntax(src);
        let prog = lower_from_green(&tree, 0).expect("lower");
        assert!(prog.module.is_some());
        assert_eq!(prog.imports.len(), 1);
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn hand_lower_return_stmt() {
        let src = "func main(): int {\n    return 1\n}\n";
        let tree = parse_syntax(src);
        let (total, hand) = count_hand_lowerable_stmts(&tree);
        assert!(total >= 1, "stmts={total}");
        assert!(hand >= 1, "hand-lowerable={hand} total={total}");
    }

    #[test]
    fn hand_lower_let_and_binary_return() {
        let src = "func main(): int {\n    let x = 1\n    let mut y = x + 2\n    return y * 3\n}\n";
        let tree = parse_syntax(src);
        let (total, hand) = count_hand_lowerable_stmts(&tree);
        assert!(total >= 3, "stmts={total}");
        assert_eq!(
            hand, total,
            "all stmts hand-lowerable, hand={hand} total={total}"
        );

        let (ftotal, fhand) = count_hand_lowerable_funcs(&tree);
        assert_eq!(ftotal, 1);
        assert_eq!(fhand, 1, "func should fully hand-lower");
    }

    #[test]
    fn hand_lower_typed_let_assign_call_if() {
        let src = r#"func main(): int {
    let x: int = 1
    x = x + 1
    foo(1, 2)
    if x > 0 {
        return x
    } else {
        return 0
    }
}
"#;
        let tree = parse_syntax(src);
        let (total, hand) = count_hand_lowerable_stmts(&tree);
        assert!(total >= 4, "stmts={total}");
        assert_eq!(
            hand, total,
            "all stmts hand-lowerable, hand={hand} total={total}"
        );
        let (ftotal, fhand) = count_hand_lowerable_funcs(&tree);
        assert_eq!((ftotal, fhand), (1, 1));

        let prog = lower_from_green(&tree, 0).expect("green lower");
        let TopLevelDecl::Func(f) = prog.pool.decl(prog.decls[0]) else {
            panic!("expected Func");
        };
        let stmts = prog.pool.stmt_list(f.body.statements);
        assert_eq!(stmts.len(), 4);
        assert_eq!(stmts.len(), 4);
        assert!(matches!(prog.pool.stmt(stmts[0]), Stmt::VarDecl { .. }));
        assert!(matches!(prog.pool.stmt(stmts[1]), Stmt::Set { .. }));
        assert!(matches!(prog.pool.stmt(stmts[2]), Stmt::Expr { .. }));
        assert!(matches!(prog.pool.stmt(stmts[3]), Stmt::If { .. }));
    }

    #[test]
    fn hand_lower_while_and_comparisons() {
        let src = r#"func main(): int {
    let n = 3
    while n > 0 {
        n = n - 1
    }
    return n
}
"#;
        let tree = parse_syntax(src);
        let (ftotal, fhand) = count_hand_lowerable_funcs(&tree);
        assert_eq!((ftotal, fhand), (1, 1));
        let prog = lower_from_green(&tree, 0).expect("lower");
        let TopLevelDecl::Func(f) = prog.pool.decl(prog.decls[0]) else {
            panic!("func");
        };
        assert!(
            prog.pool
                .stmt_list(f.body.statements)
                .iter()
                .any(|&id| matches!(prog.pool.stmt(id), Stmt::While { .. }))
        );
    }

    #[test]
    fn hand_lower_func_integrated_in_green_walk() {
        let src = "func add(a: int, b: int): int {\n    return a + b\n}\n";
        let tree = parse_syntax(src);
        let prog = lower_from_green(&tree, 0).expect("hand/green lower");
        assert_eq!(decl_count(&prog), 1);
        match prog.pool.decl(prog.decls[0]) {
            TopLevelDecl::Func(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(prog.pool.stmt_list(f.body.statements).len(), 1);
                assert!(matches!(
                    prog.pool.stmt(prog.pool.stmt_list(f.body.statements)[0]),
                    Stmt::Return { .. }
                ));
            }
            other => panic!("expected Func, got {other:?}"),
        }
    }

    #[test]
    fn hand_lower_func_matches_rd_decl_count() {
        let src = "func main(): int {\n    let x = 1 + 2\n    return x\n}\n";
        let tree = parse_syntax(src);
        let via_walk = lower_from_green(&tree, 0).expect("walk");
        let via_rd = crate::syntax::build::lower_syntax_to_program_rd_only(&tree, 0).expect("rd");
        assert_eq!(via_walk.decls.len(), via_rd.decls.len());
        assert_eq!(decl_count(&via_walk), 1);
        let (ftotal, fhand) = count_hand_lowerable_funcs(&tree);
        assert_eq!((ftotal, fhand), (1, 1));
    }

    #[test]
    fn hand_lower_struct_const_enum() {
        let src = r#"struct Point {
    x: int
    y: int
}
const Origin = 0
enum Color {
    Red
    Green
    Blue
}
"#;
        let tree = parse_syntax(src);
        let (total, hand) = count_hand_lowerable_decls(&tree);
        assert_eq!(total, 3, "expected 3 decl items");
        assert_eq!(hand, total, "all decls hand-lowerable hand={hand}");
        let prog = lower_from_green(&tree, 0).expect("lower");
        assert_eq!(decl_count(&prog), 3);
    }

    #[test]
    fn hand_lower_for_in_and_public_async() {
        let src = r#"public async func walk(items: int): int {
    for x in items {
        return x
    }
    return 0
}
"#;
        let tree = parse_syntax(src);
        let (ftotal, fhand) = count_hand_lowerable_funcs(&tree);
        assert_eq!((ftotal, fhand), (1, 1));
        let prog = lower_from_green(&tree, 0).expect("lower");
        let TopLevelDecl::Func(f) = prog.pool.decl(prog.decls[0]) else {
            panic!("func");
        };
        assert!(f.is_async);
        assert!(matches!(f.visibility, crate::Visibility::Public));
        assert!(
            prog.pool
                .stmt_list(f.body.statements)
                .iter()
                .any(|&id| matches!(prog.pool.stmt(id), Stmt::For { .. }))
        );
    }

    #[test]
    fn hand_lower_place_field_and_index() {
        let src = r#"func main(): int {
    p.x = 1
    a[0] = 2
    return a[0]
}
"#;
        let tree = parse_syntax(src);
        let (total, hand) = count_hand_lowerable_stmts(&tree);
        assert_eq!(hand, total, "hand={hand} total={total}");
        let (ftotal, fhand) = count_hand_lowerable_funcs(&tree);
        assert_eq!((ftotal, fhand), (1, 1));
    }

    #[test]
    fn event_green_has_expr_nodes() {
        let tree = parse_syntax("func main(): int { return 1 }\n");
        let exprs = tree
            .root()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::EXPR)
            .count();
        assert!(exprs >= 1, "expected EXPR nodes from event sink");
    }
}
