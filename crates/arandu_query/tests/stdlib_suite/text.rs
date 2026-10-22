//! Tests for std.text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

use crate::common;

const TEXT_ARU: &str = include_str!("../../../../stdlib/std/text.aru");

#[test]
fn stdlib_text_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/std/text.aru".to_string(), TEXT_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("text.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "StringBuilder",
        "StringBuilder.new",
        "StringBuilder.withCapacity",
        "StringBuilder.append",
        "StringBuilder.appendLine",
        "StringBuilder.appendChar",
        "StringBuilder.appendInt",
        "StringBuilder.clear",
        "StringBuilder.intoString",
        "trimStart",
        "trimEnd",
        "trim",
        "toUpper",
        "toLower",
        "repeat",
        "padLeft",
        "padRight",
        "center",
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
fn stdlib_text_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let mut text_file = None;
    for (path, source) in common::STDLIB_MODULES {
        let file = db.new_stdlib_file((*path).to_string(), (*source).to_string());
        if *path == "stdlib/std/text.aru" {
            text_file = Some(file);
        }
    }
    let text_file = text_file.expect("text.aru must be registered");

    let main_src = r#"
import std.text as text

func testTextOperations(): bool {
    let mut sb = text.StringBuilder.new()
    sb.append("hello")
    sb.appendChar(' ')
    sb.appendInt(42 as i64)
    sb.appendLine("!")
    let s = sb.intoString()

    let trimmed = text.trim("   hello   ")
    let upper = text.toUpper("abc")
    let lower = text.toLower("ABC")
    let rep = text.repeat("x", 5 as usize)
    let padded = text.padLeft("7", 4 as usize, '0')
    return true
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags = file_ide_diagnostics(&db, text_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_text: Vec<_> = diags.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_text.is_empty(),
        "unexpected errors in text.aru: {error_diags_text:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru using text.aru: {error_diags_main:?}"
    );
}
