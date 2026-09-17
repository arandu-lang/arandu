#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::fs;
use std::path::PathBuf;

use arandu_parser::ast_pool::ExprKind;
use arandu_parser::{ParseErrorCode, parse, parse_to_string};

fn contains(outer_start: usize, outer_end: usize, inner_start: usize, inner_end: usize) -> bool {
    outer_start <= inner_start && inner_end <= outer_end
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crate should be under workspace/crates")
        .to_path_buf()
}

fn contract_source(name: &str) -> String {
    let source_path = workspace_root()
        .join("tests")
        .join("parser_contract")
        .join(format!("{name}.aru"));
    fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()))
}

fn assert_contract_ast(name: &str, expected: &str) {
    let actual = parse_to_string(&contract_source(name)).expect("parser should succeed");
    assert_eq!(actual.trim_end(), expected.trim_end());
}

fn assert_contract_rejects(name: &str, expected: ParseErrorCode) {
    let err = parse(&contract_source(name)).expect_err("parser should reject source");
    assert_eq!(err.code, expected);
}

#[test]
fn parse_error_reports_expected_tokens_and_found_token() {
    let source = "module tests.diagnostics\nfunc main( { }";
    let err = parse(source).expect_err("parser should reject malformed function parameter list");

    assert_eq!(err.code, ParseErrorCode::ExpectedToken);
    assert_eq!(err.found.as_ref(), "{");
    assert_eq!(err.message.as_ref(), "expected value identifier");
    assert!(err.expected.contains(&"value identifier"));
    let line_index = arandu_base::line_index::LineIndex::new(source);
    let (start_line, _) = line_index.line_col(err.span.start);
    assert_eq!(start_line, 2);
}

#[test]
fn import_aliases_reject_reserved_keywords() {
    let alias = "module tests.alias\nimport std.core.char as if\nfunc main(): void {}\n";
    assert!(
        parse(alias).is_err(),
        "reserved control-flow keyword became an alias"
    );

    let named = "module tests.alias\nfrom std.core.char import { lenUtf8 as return }\nfunc main(): void {}\n";
    assert!(
        parse(named).is_err(),
        "reserved control-flow keyword became a named alias"
    );
}

#[test]
fn canonical_borrow_syntax_is_uniform_in_parameters_returns_and_expressions() {
    let source = r#"module tests.borrow_syntax
func borrowValue(value: ref int): ref int {
    return value
}
func replace(value: mut ref int): void {
    let borrowed = mut ref value
}
func consume(value: own int): int {
    let borrowed = ref value
    return *borrowed
}
"#;

    let dump = parse_to_string(source).expect("canonical borrow syntax should parse");
    assert!(
        dump.contains("value Ref "),
        "missing shared reference type: {dump}"
    );
    assert!(
        dump.contains("value RefMut "),
        "missing exclusive reference type: {dump}"
    );
    assert!(
        dump.contains("(ref , Path"),
        "missing shared borrow expression: {dump}"
    );
    assert!(
        dump.contains("(mut ref , Path"),
        "missing exclusive borrow expression: {dump}"
    );
}

#[test]
fn ast_program_span_covers_source_before_eof() {
    let source = "module tests.spans\nfunc main() {\n    let value = add(1, 2)\n}\n";
    let program = parse(source).expect("parser should succeed");
    let line_index = arandu_base::line_index::LineIndex::new(source);
    let (start_line, start_col) = line_index.line_col(program.span.start);
    let (end_line, end_col) = line_index.line_col(program.span.end);
    assert_eq!(start_line, 1);
    assert_eq!(start_col, 1);
    assert_eq!(end_line, 4);
    assert_eq!(end_col, 2);
}

#[test]
fn ast_nested_expression_spans_are_contained_by_parent() {
    let program = parse("module tests.spans\nfunc main() {\n    let value = add(1 + 2, 3)\n}\n")
        .expect("parser should succeed");
    let func = match program.pool.decl(program.decls[0]) {
        arandu_parser::TopLevelDecl::Func(func) => func,
        other => panic!("expected func, got {other:?}"),
    };
    let stmt = program
        .pool
        .stmt(program.pool.stmt_list(func.body.statements)[0]);
    let arandu_parser::Stmt::VarDecl { value, .. } = stmt else {
        panic!("expected var decl, got {stmt:?}");
    };
    let value_id = *value;
    let call_span = program.pool.expr_span(value_id);
    let call_kind = program.pool.expr(value_id);
    let ExprKind::Call { args, .. } = call_kind else {
        panic!("expected call, got {call_kind:?}");
    };
    let arg_id = program.pool.expr_list(*args)[0];
    let inner_span = program.pool.expr_span(arg_id);
    assert!(contains(
        call_span.start as usize,
        call_span.end as usize,
        inner_span.start as usize,
        inner_span.end as usize
    ));
}

#[test]
fn ast_call_with_block_span_covers_trailing_block() {
    let source = "module tests.spans\nfunc main() {\n    route(\"/\") {\n        ok()\n    }\n}\n";
    let program = parse(source).expect("parser should succeed");
    let func = match program.pool.decl(program.decls[0]) {
        arandu_parser::TopLevelDecl::Func(func) => func,
        other => panic!("expected func, got {other:?}"),
    };
    let stmt = program
        .pool
        .stmt(program.pool.stmt_list(func.body.statements)[0]);
    let arandu_parser::Stmt::Expr { expr, .. } = stmt else {
        panic!("expected expr stmt, got {stmt:?}");
    };
    let expr_id = *expr;
    let call_kind = program.pool.expr(expr_id);
    let ExprKind::Call { trailing_block, .. } = call_kind else {
        panic!("expected call, got {call_kind:?}");
    };
    let span = program.pool.expr_span(expr_id);
    let block_id = trailing_block.expect("call should have block");
    let block = program.pool.block(block_id);
    let line_index = arandu_base::line_index::LineIndex::new(source);
    let (span_end_line, span_end_col) = line_index.line_col(span.end);
    let (block_end_line, block_end_col) = line_index.line_col(block.span.end);
    assert_eq!(span_end_line, block_end_line);
    assert_eq!(span_end_col, block_end_col);
}

#[test]
fn ast_multiline_string_span_covers_delimiters() {
    let source = "module tests.spans\nfunc main() {\n    let text = \"\"\"\nhello\n\"\"\"\n}\n";
    let program = parse(source).expect("parser should succeed");
    let func = match program.pool.decl(program.decls[0]) {
        arandu_parser::TopLevelDecl::Func(func) => func,
        other => panic!("expected func, got {other:?}"),
    };
    let stmt = program
        .pool
        .stmt(program.pool.stmt_list(func.body.statements)[0]);
    let arandu_parser::Stmt::VarDecl { value, .. } = stmt else {
        panic!("expected var decl, got {stmt:?}");
    };
    let value_id = *value;
    let span = program.pool.expr_span(value_id);
    let line_index = arandu_base::line_index::LineIndex::new(source);
    let (start_line, start_col) = line_index.line_col(span.start);
    let (end_line, end_col) = line_index.line_col(span.end);
    assert_eq!(start_line, 3);
    assert_eq!(start_col, 16);
    assert_eq!(end_line, 5);
    assert_eq!(end_col, 4);
}

#[test]
fn doc_comments_attach_to_documentable_nodes_and_preserve_order() {
    let program = parse(
        r#"module tests.docs

/// first
/// second
func main() {
    /// ignored inside block
    let value = 1
}

/// User docs
struct User {
    /// field docs
    name: str
}

enum Token {
    /// word docs
    Word(str)
}

extern "C" {
    /// puts docs
    func puts(text: ptr[u8]): int
}
"#,
    )
    .expect("parser should accept doc comments");

    assert_eq!(program.docs.len(), 6);
    assert_eq!(program.docs[0].text, "/// first");
    assert_eq!(program.docs[1].text, "/// second");
    assert_eq!(program.docs[0].target_span, program.docs[1].target_span);
    assert!(program.docs.iter().all(|doc| !doc.text.contains("ignored")));
}

#[test]
fn module_basic() {
    assert_contract_ast(
        "module_basic",
        "Program @1:1-1:28\n  Module @1:1-1:28 tests.contract.basic",
    );
}

#[test]
fn module_contextual_keyword() {
    assert_contract_ast(
        "module_contextual_keyword",
        "Program @1:1-1:36\n  Module @1:1-1:36 examples.stable.syntax.match",
    );
}

#[test]
fn import_module() {
    assert_contract_ast(
        "import_module",
        "Program @1:1-2:10\n  Module @1:1-1:30 tests.contract.imports\n  Import @2:1-2:10 io as io",
    );
}

/// Regression: comma-separated single-line struct bodies must parse. The field
/// loop in `parse_struct_decl` only skipped semicolons/newlines, so
/// `public struct R { x: int, y: int }` failed with P001 and the whole module
/// silently stopped exporting its members (pypor `counter.aru`/`walker.aru`).
#[test]
fn single_line_struct_fields_with_commas() {
    let program = parse(
        "module tests.struct_single_line\n\
         public struct Stats { code: uint, comment: uint, blank: uint }\n\
         public func zeroStats(): Stats { return Stats { code: 0, comment: 0, blank: 0 } }\n",
    )
    .expect("comma-separated single-line struct fields must parse");
    let fields = match &program.pool.decl(program.decls[0]) {
        arandu_parser::TopLevelDecl::Struct(struct_decl) => &struct_decl.fields,
        other => panic!("expected struct, got {other:?}"),
    };
    let names: Vec<_> = fields.iter().map(|field| field.name.as_str()).collect();
    assert_eq!(names, ["code", "comment", "blank"]);
}

#[test]
fn while_comparison_does_not_swallow_into_generic() {
    // Regression: `a < b` must stay a comparison, not open a phantom generic.
    // `find_matching_gt` used to cross statement boundaries, so a `>` inside
    // the loop body (or on a following statement) closed the generic and
    // swallowed code (pypor src/counter.aru misparsed as `<Generic>`).
    let dump = parse_to_string(
        "module tests.generic_regression\n\
         func f() {\n\
         \x20   while a < b {\n\
         \x20       x = c > (y)\n\
         \x20   }\n\
         }\n",
    )
    .expect("while condition comparison must parse");
    assert!(
        dump.contains("While @3:5-5:6 Condition @3:11-3:16 Binary @3:11-3:16(<, Path"),
        "expected `<` comparison in the while condition, got:\n{dump}"
    );
    assert!(
        !dump.contains("Generic @3"),
        "`a < b` must not be parsed as a generic type argument list:\n{dump}"
    );
}

#[test]
fn following_statement_gt_does_not_cross_boundary() {
    // A `>` in the following statement must not close a phantom generic
    // opened by `<` on the previous line (another pypor pattern).
    let dump = parse_to_string(
        "module tests.generic_regression\n\
         func f() {\n\
         \x20   let low = a < b\n\
         \x20   x = c > (y)\n\
         }\n",
    )
    .expect("parser should keep statements separate");
    assert!(
        dump.contains("Binary @3:15-3:20(<, Path @3:15-3:16(a), Path @3:19-3:20(b))"),
        "expected `<` binary on line 3:\n{dump}"
    );
    assert!(
        !dump.contains("Generic @3"),
        "`<` on line 3 must not open a generic:\n{dump}"
    );
}

#[test]
fn nested_generic_inside_group_keeps_each_delimiter_level() {
    let dump = parse_to_string(
        "func identity<T>(value: T): T { return value }\n\
         func main() {\n\
         \x20   let value = identity<(Box<Option<int>>)>(input)\n\
         }\n",
    )
    .expect("nested generic arguments inside a grouped type must parse");
    assert!(
        dump.contains("Generic") && dump.contains("Box") && dump.contains("Option"),
        "nested generic call lost its type arguments:\n{dump}"
    );
}

/// T3.5: multi-segment unquoted module alias (`import std.core.mem as mem`).
#[test]
fn import_module_alias_path() {
    assert_contract_ast(
        "import_module_alias_path",
        "Program @1:1-2:27\n  Module @1:1-1:30 tests.contract.imports\n  Import @2:1-2:27 std.core.mem as mem",
    );
}

/// T2.1: default type parameter (`A = GlobalAlloc`).
#[test]
fn generic_default_param() {
    assert_contract_ast(
        "generic_default_param",
        "Program @1:1-5:2\n  Module @1:1-1:31 tests.contract.generics\n  Struct @2:1-5:2 BoxG<@2:13-2:14 T, @2:16-2:31 A = Type @2:20-2:31 @2:20-2:31 GlobalAlloc>\n    Field @3:5-3:13 value Type @3:12-3:13 @3:12-3:13 T\n    Field @4:5-4:13 alloc Type @4:12-4:13 @4:12-4:13 A",
    );
}

/// Doc comments after a struct must attach to the **next** item, not poison the
/// previous item's hand-lower (root cause of gen_arena.aru parse failures).
#[test]
fn doc_comment_between_structs_parses() {
    let program = parse(
        r#"module t
import std.core.option as option
public struct GenRef {
    index: u32
    generation: u32
}
/// slot docs
public struct GenSlot<T> {
    value: option.Option<T>
    generation: u32
}
"#,
    )
    .expect("doc between structs + option.Option field must parse");
    assert_eq!(program.decls.len(), 2);
}

#[test]
fn import_named() {
    assert_contract_ast(
        "import_named",
        "Program @1:1-2:34\n  Module @1:1-1:30 tests.contract.imports\n  From ui Import @2:1-2:34 { @2:18-2:24 Button, @2:26-2:32 Window }",
    );
}

#[test]
fn import_alias() {
    assert_contract_ast(
        "import_alias",
        "Program @1:1-2:39\n  Module @1:1-1:30 tests.contract.imports\n  From ui Import @2:1-2:39 { @2:18-2:37 Window as AppWindow }",
    );
}

#[test]
fn import_external() {
    assert_contract_ast(
        "import_external",
        "Program @1:1-2:41\n  Module @1:1-1:30 tests.contract.imports\n  Import @2:1-2:41 \"github.com/empresa/auth\" as auth",
    );
}

#[test]
fn list_trailing_comma() {
    assert_contract_ast(
        "list_trailing_comma",
        "Program @1:1-2:27\n  Module @1:1-1:28 tests.contract.lists\n  From ui Import @2:1-2:27 { @2:18-2:24 Button }",
    );
}

#[test]
fn list_empty_allowed() {
    assert_contract_ast(
        "list_empty_allowed",
        "Program @1:1-2:16\n  Module @1:1-1:28 tests.contract.lists\n  Func @2:1-2:16 empty() -> void",
    );
}

#[test]
fn list_empty_forbidden() {
    assert_contract_rejects("list_empty_forbidden", ParseErrorCode::ExpectedToken);
}

#[test]
fn where_func() {
    assert_contract_ast(
        "where_func",
        "Program @1:1-4:2\n  Module @1:1-1:34 tests.contract.constraints\n  Func @2:1-4:2 identity<@2:15-2:16 T>(@2:18-2:26 value Type @2:25-2:26 @2:25-2:26 T) -> Type @2:29-2:30 @2:29-2:30 T where @2:37-2:47 T: @2:40-2:47 Display\n    Return @3:5-3:17 Path @3:12-3:17(value)",
    );
}

#[test]
fn where_struct() {
    assert_contract_ast(
        "where_struct",
        "Program @1:1-4:2\n  Module @1:1-1:34 tests.contract.constraints\n  Struct @2:1-4:2 Box<@2:12-2:13 T> where @2:21-2:31 T: @2:24-2:31 Display\n    Field @3:5-3:13 value Type @3:12-3:13 @3:12-3:13 T",
    );
}

#[test]
fn semicolon_before_rbrace() {
    assert_contract_ast(
        "semicolon_before_rbrace",
        "Program @1:1-4:2\n  Module @1:1-1:33 tests.contract.semicolons\n  Func @2:1-4:2 main() -> void\n    Return @3:5-3:13 Int @3:12-3:13(1)",
    );
}

#[test]
fn semicolon_before_else() {
    assert_contract_ast(
        "semicolon_before_else",
        "Program @1:1-9:2\n  Module @1:1-1:33 tests.contract.semicolons\n  Func @2:1-9:2 main() -> void\n    If @3:5-8:6 Condition @3:8-3:10 Path @3:8-3:10(ok)\n      Expr @4:9-4:23 Call @4:9-4:23(Path @4:9-4:16(println), [String @4:17-4:22(\"sim\")])\n    Else @6:10-8:6\n      Expr @7:9-7:23 Call @7:9-7:23(Path @7:9-7:16(println), [String @7:17-7:22(\"nao\")])",
    );
}

#[test]
fn generic_call_ambiguity() {
    assert_contract_ast(
        "generic_call_ambiguity",
        "Program @1:1-5:2\n  Module @1:1-1:32 tests.contract.lookahead\n  Func @2:1-5:2 main() -> void\n    Var @3:5-3:31 @3:9-3:11 ok = Call @3:14-3:31(Generic @3:14-3:27(Path @3:14-3:22(identity), <Type @3:23-3:26 int>), [Int @3:28-3:30(42)])\n    Var @4:5-4:28 @4:9-4:16 compare = Binary @4:19-4:28(>, Binary @4:19-4:24(<, Path @4:19-4:20(a), Path @4:23-4:24(b)), Path @4:27-4:28(c))",
    );
}

#[test]
fn variable_declaration_lookahead() {
    assert_contract_ast(
        "variable_declaration_lookahead",
        "Program @1:1-5:2\n  Module @1:1-1:32 tests.contract.lookahead\n  Func @2:1-5:2 main() -> void\n    Var @3:5-3:18 @3:9-3:14 value = Int @3:17-3:18(1)\n    Var @4:5-4:23 @4:9-4:19 typed Type @4:16-4:19 int = Int @4:22-4:23(2)",
    );
}

#[test]
fn where_on_new_line() {
    let _program = parse(
        "module test\nfunc identity<T>(value: T): T\nwhere T: Display {\n    return value\n}",
    )
    .expect("parser should accept where on a new line");
}

#[test]
fn from_on_new_line() {
    let _program = parse("module test\nfrom ui import {\n  Button\n}\n")
        .expect("parser should accept from on a new line");
}

#[test]
fn invalid_tuple_err_return_rejected() {
    assert_contract_rejects(
        "invalid_tuple_err_return",
        ParseErrorCode::InvalidResultReturn,
    );
}

#[test]
fn invalid_err_only_return_rejected() {
    assert_contract_rejects(
        "invalid_err_only_return",
        ParseErrorCode::InvalidResultReturn,
    );
}

#[test]
fn test_all_stdlib_files_parse_cleanly() {
    let stdlib_dir = workspace_root().join("stdlib");
    let mut files = Vec::new();
    fn find_aru_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    find_aru_files(&path, files);
                } else if path.extension().is_some_and(|ext| ext == "aru") {
                    files.push(path);
                }
            }
        }
    }
    find_aru_files(&stdlib_dir, &mut files);
    assert!(
        !files.is_empty(),
        "stdlib files not found in {}",
        stdlib_dir.display()
    );

    let mut failed = false;
    for file in files {
        let source = std::fs::read_to_string(&file)
            .unwrap_or_else(|err| panic!("failed to read stdlib file {}: {}", file.display(), err));
        if let Err(err) = arandu_parser::parse(&source) {
            println!("Stdlib file {} failed to parse: {:?}", file.display(), err);
            failed = true;
        }
    }
    assert!(
        !failed,
        "One or more stdlib files had syntax/parsing errors"
    );
}

/// Gold bar (P2 Camada B): the `arandu new` default template must parse cleanly
/// under the same CI radar as stdlib — first-run experience cannot be the place
/// users discover syntax breakage.
#[test]
fn test_new_project_template_parses_cleanly() {
    let template = workspace_root().join("examples/minimal/TEMPLATE_main.aru");
    let source = std::fs::read_to_string(&template).unwrap_or_else(|err| {
        panic!(
            "failed to read project template {}: {err}",
            template.display()
        )
    });
    arandu_parser::parse(&source).unwrap_or_else(|err| {
        panic!(
            "arandu new template failed to parse ({}): {err:?}",
            template.display()
        )
    });
}

#[test]
fn test_impl_block_parsing() {
    let source = r#"
    module test;

    struct Point {
        x: float;
        y: float;
    }

    impl Point {
        public func new(x: float, y: float): Point {
            return Point { x, y };
        }

        public func get_x(self: ref): float {
            return self.x
        }

        public func set_x(self: mut ref, x: float) {
            self.x = x
        }
    }
    "#;

    let program = arandu_parser::parse(source).expect("impl block should parse successfully");
    assert_eq!(program.decls.len(), 4, "struct plus every impl member");
    let names: Vec<_> = program
        .decls
        .iter()
        .filter_map(|id| match program.pool.decl(*id) {
            arandu_parser::TopLevelDecl::Func(func) => match &func.name {
                arandu_parser::FuncName::Method { name, .. } => Some(name.as_str()),
                arandu_parser::FuncName::Free { .. } => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(names, ["new", "get_x", "set_x"]);
}
