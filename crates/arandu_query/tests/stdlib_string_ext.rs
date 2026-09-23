//! Tests for std.alloc.string extensions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

mod common;

const STRING_ARU: &str = include_str!("../../../stdlib/alloc/string.aru");

#[test]
fn stdlib_string_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/string.aru".to_string(),
        STRING_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("string.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "String", "new", "from", "asStr", "asBytes", "pushStr", "truncate",
    ];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_string_methods_usage() {
    let mut db = DatabaseImpl::default();
    let mut str_file = None;
    for (path, source) in common::STDLIB_MODULES {
        let file = db.new_file((*path).to_string(), (*source).to_string());
        if *path == "stdlib/alloc/string.aru" {
            str_file = Some(file);
        }
    }
    let str_file = str_file.expect("string module is in the canonical stdlib graph");
    let main_src = r#"
import std.alloc.string as string

func testString(): int {
    let mut s = string.from("hello world")
    if !s.startsWith("hello") {
        return 1
    }
    if !s.endsWith("world") {
        return 2
    }
    if !s.contains("lo wo") {
        return 3
    }
    let c = s.clone()
    if c.len() != s.len() {
        return 4
    }
    s.truncate(5)
    if s.len() != 5 {
        return 5
    }
    return 0
}

func main(): int {
    return testString()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_str = file_ide_diagnostics(&db, str_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_str: Vec<_> = diags_str.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_str.is_empty(),
        "unexpected errors in string.aru: {error_diags_str:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
