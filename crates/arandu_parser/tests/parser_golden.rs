#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::fs;

use arandu_parser::{ParseErrorCode, parse_to_string};
use arandu_test_support::{assert_golden_text, read_golden_text, workspace_root};

fn assert_golden(name: &str) {
    let source = read_golden_text("parser", name, "aru");
    let actual = parse_to_string(&source).expect("parser should succeed");
    assert_golden_text("parser", name, "ast", &actual);
}

fn assert_parses_example(path: &str) {
    let root = workspace_root();
    let source_path = root.join(path);
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
    arandu_parser::parse(&source)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", source_path.display()));
}

fn assert_parses_source(source: &str) {
    arandu_parser::parse(source).expect("parser should succeed");
}

fn assert_rejects_source(source: &str, expected: ParseErrorCode) {
    let err = arandu_parser::parse(source).expect_err("parser should reject source");
    assert_eq!(err.code, expected);
}

#[test]
fn parses_hello_fixture() {
    assert_golden("hello");
}

#[test]
fn parses_variables_fixture() {
    assert_golden("variables");
}

#[test]
fn parses_functions_fixture() {
    assert_golden("functions");
}

#[test]
fn parses_expressions_fixture() {
    assert_golden("expressions");
}

#[test]
fn parses_function_modifiers_fixture() {
    assert_golden("function_modifiers");
}

#[test]
fn parses_visibility_modifiers_and_keeps_default_module_visibility() {
    let source = "internal func package_api() {}\nprivate struct Hidden { private value: int }\nfunc local() {}\n";
    let dump = parse_to_string(source).expect("visibility declarations should parse");

    assert!(dump.contains("internal package_api"), "{dump}");
    assert!(dump.contains(" private Hidden"), "{dump}");
    assert!(dump.contains("private value Type"), "{dump}");
    assert!(dump.contains("local() -> void"), "{dump}");
    assert!(!dump.contains("module local"), "{dump}");
}

#[test]
fn parses_sealed_interface_modifier() {
    let program = arandu_parser::parse("sealed interface Closed { func value(self): int }")
        .expect("sealed interface should parse");
    let interface = program
        .decls
        .iter()
        .find_map(|decl| match program.pool.decl(*decl) {
            arandu_parser::TopLevelDecl::Interface(interface) => Some(interface),
            _ => None,
        })
        .expect("interface declaration");
    assert!(interface.sealed);
    assert!(
        program
            .dump("sealed interface Closed { func value(self): int }")
            .contains("sealed")
    );
}

#[test]
fn parses_named_import_fixture() {
    assert_golden("named_import");
}

#[test]
fn parses_interpolated_string_fixture() {
    assert_golden("interpolated_string");
}

#[test]
fn parses_places_fixture() {
    assert_golden("places");
}

#[test]
fn parses_compound_assignments_fixture() {
    assert_golden("compound_assignments");
}

#[test]
fn parses_type_qualified_call_fixture() {
    assert_golden("type_qualified_call");
}

#[test]
fn parses_method_self_fixture() {
    assert_golden("method_self");
}

#[test]
fn parses_declarations_struct_fixture() {
    assert_golden("declarations_struct");
}

#[test]
fn parses_declarations_enum_fixture() {
    assert_golden("declarations_enum");
}

#[test]
fn parses_declarations_interface_fixture() {
    assert_golden("declarations_interface");
}

#[test]
fn parses_declarations_extern_fixture() {
    assert_golden("declarations_extern");
}

#[test]
fn parses_generics_full_fixture() {
    assert_golden("generics_full");
}

#[test]
fn parses_struct_literal_fixture() {
    assert_golden("struct_literal");
}

#[test]
fn parses_match_patterns_fixture() {
    assert_golden("match_patterns");
}

#[test]
fn parses_attributes_fixture() {
    assert_golden("attributes");
}

#[test]
fn parses_multi_binding_fixture() {
    assert_golden("multi_binding");
}

#[test]
fn parses_v03_control_flow_fixture() {
    assert_golden("control_flow");
}

#[test]
fn parses_v03_defer_errdefer_fixture() {
    assert_golden("defer_errdefer");
}

#[test]
fn parses_v03_unsafe_free_alloc_fixture() {
    assert_golden("unsafe_free_alloc");
}

#[test]
fn parses_v03_catch_cast_precedence_fixture() {
    assert_golden("catch_cast_precedence");
}

#[test]
fn parses_v03_safe_access_try_fixture() {
    assert_golden("safe_access_try");
}

#[test]
fn parses_v03_lambda_array_fixture() {
    assert_golden("lambda_array");
}

#[test]
fn parses_v03_if_expr_async_block_fixture() {
    assert_golden("if_expr_async_block");
}

#[test]
fn parses_raw_string_fixture() {
    assert_golden("raw_string");
}

#[test]
fn parses_multiline_string_fixture() {
    assert_golden("multiline_string");
}

#[test]
fn parses_trailing_block_call_fixture() {
    assert_golden("trailing_block_call");
}

#[test]
fn parses_generic_trailing_block_call_fixture() {
    assert_golden("generic_trailing_block_call");
}

#[test]
fn parses_bare_block_call_fixture() {
    assert_golden("bare_block_call");
}

#[test]
fn parses_range_expression_fixture() {
    assert_golden("range_expression");
}

#[test]
fn parses_span_dump_fixture() {
    assert_golden("span_dump");
}

#[test]
fn parses_http_parser_realworld_fixture() {
    assert_golden("http_parser_realworld");
}

#[test]
fn parses_raytracer_math_realworld_fixture() {
    assert_golden("raytracer_math_realworld");
}

#[test]
fn parses_elf_binary_parser_fixture() {
    assert_golden("elf_binary_parser");
}

#[test]
fn parses_sha256_hash_realworld_fixture() {
    assert_golden("sha256_hash_realworld");
}

#[test]
fn parses_lz_compression_realworld_fixture() {
    assert_golden("lz_compression_realworld");
}

#[test]
fn parses_kandiru_json_ast_realworld_fixture() {
    assert_golden("kandiru_json_ast_realworld");
}

#[test]
fn parses_aratu_log_store_realworld_fixture() {
    assert_golden("aratu_log_store_realworld");
}

#[test]
fn parses_katu_walker_realworld_fixture() {
    assert_golden("katu_walker_realworld");
}

#[test]
fn parses_poraque_hexdump_realworld_fixture() {
    assert_golden("poraque_hexdump_realworld");
}

#[test]
fn parses_sarapo_status_realworld_fixture() {
    assert_golden("sarapo_status_realworld");
}

#[test]
fn parses_kururu_camera_realworld_fixture() {
    assert_golden("kururu_camera_realworld");
}

#[test]
fn parses_stdlib_vector_algorithm_fixture() {
    assert_golden("stdlib_vector_algorithm");
}

#[test]
fn parses_stdlib_ring_buffer_fixture() {
    assert_golden("stdlib_ring_buffer");
}

#[test]
fn parses_concurrency_thread_pool_fixture() {
    assert_golden("concurrency_thread_pool");
}

#[test]
fn parses_atomic_lock_free_stack_fixture() {
    assert_golden("atomic_lock_free_stack");
}

#[test]
fn parses_trailing_commas_in_named_imports_and_generics() {
    assert_parses_source(
        "module tests.inline\nfrom ui import { Button, }\ntype Box<T,> = T\nfunc id<T,>(value: T): T { return value; }\n",
    );
}

#[test]
fn parses_trailing_comma_in_where_clause() {
    assert_parses_source(
        "module tests.inline\nfunc identity<T>(value: T): T where T: Display, { return value; }\ninterface Displayable { func fmt() where T: Display,; }\n",
    );
}

#[test]
fn rejects_module_without_terminator_before_top_level_decl() {
    assert_rejects_source(
        "module tests.inline const value int = 1\n",
        ParseErrorCode::ExpectedToken,
    );
}

#[test]
fn parses_required_v02_examples() {
    for path in [
        "examples/stable/syntax/hello.aru",
        "examples/stable/syntax/variables.aru",
        "examples/stable/syntax/functions.aru",
        "examples/stable/syntax/structs.aru",
        "examples/stable/syntax/enums.aru",
        "examples/stable/syntax/match.aru",
        "examples/stable/syntax/generics.aru",
        "examples/draft/async.aru",
        "examples/stable/interop/ffi.aru",
        "examples/stable/interop/extern_c.aru",
        "examples/stable/semantics/errors.aru",
        "examples/stable/semantics/mut_set.aru",
        "examples/draft/semantics/defer_errdefer.aru",
        "examples/invalid/semantics/double_free.aru",
        "examples/invalid/semantics/unsafe_outside_block.aru",
    ] {
        assert_parses_example(path);
    }
}

#[test]
fn rejects_invalid_assignment_target() {
    let root = workspace_root();
    let source_path = root
        .join("tests")
        .join("parser")
        .join("invalid_assignment_target.aru");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
    let err = arandu_parser::parse(&source).expect_err("parser should reject invalid target");
    assert_eq!(err.code, ParseErrorCode::ExpectedToken);
}

#[test]
fn rejects_invalid_place_fixture() {
    let root = workspace_root();
    let source_path = root.join("tests").join("parser").join("invalid_place.aru");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
    let err = arandu_parser::parse(&source).expect_err("parser should reject invalid place");
    assert_eq!(err.code, ParseErrorCode::ExpectedToken);
}

#[test]
fn rejects_empty_named_import() {
    assert_rejects_source(
        "module tests.inline\nimport {} from ui\n",
        ParseErrorCode::ExpectedToken,
    );
}

#[test]
fn rejects_empty_generic_params() {
    assert_rejects_source(
        "module tests.inline\ntype Box<> = int\n",
        ParseErrorCode::ExpectedToken,
    );
}

#[test]
fn rejects_single_parenthesized_result_type() {
    assert_rejects_source(
        "func f() (int) { return 1 }\n",
        ParseErrorCode::ExpectedToken,
    );
}

#[test]
fn rejects_v02_invalid_parser_fixtures() {
    let root = workspace_root();
    for (name, expected) in [
        ("invalid_struct_field", ParseErrorCode::ExpectedToken),
        ("invalid_enum_payload", ParseErrorCode::ExpectedToken),
        (
            "invalid_extern_abi_interpolation",
            ParseErrorCode::ExpectedToken,
        ),
        (
            "invalid_generic_call_without_parens",
            ParseErrorCode::ExpectedToken,
        ),
        ("invalid_match_arm", ParseErrorCode::ExpectedToken),
        ("invalid_match_pattern", ParseErrorCode::ExpectedToken),
        ("invalid_chained_range", ParseErrorCode::ExpectedToken),
        (
            "invalid_lambda_missing_arrow",
            ParseErrorCode::ExpectedToken,
        ),
        ("invalid_catch_missing_pipe", ParseErrorCode::ExpectedToken),
        ("invalid_unsafe_expr", ParseErrorCode::ExpectedToken),
        ("invalid_safe_index", ParseErrorCode::ExpectedToken),
        (
            "invalid_for_missing_semicolon",
            ParseErrorCode::ExpectedToken,
        ),
        ("invalid_multiline_unterminated", ParseErrorCode::Lex),
        (
            "invalid_generic_bare_without_block",
            ParseErrorCode::ExpectedExpression,
        ),
        (
            "invalid_trailing_block_newline",
            ParseErrorCode::ExpectedExpression,
        ),
    ] {
        let source_path = root
            .join("tests")
            .join("parser")
            .join(format!("{name}.aru"));
        let source = fs::read_to_string(&source_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
        let err = arandu_parser::parse(&source).expect_err("parser should reject fixture");
        assert_eq!(err.code, expected, "{name} failed with unexpected code");
    }
}
