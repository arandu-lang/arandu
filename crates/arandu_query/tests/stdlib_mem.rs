//! Tests for std.core.mem.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");

#[test]
fn stdlib_mem_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("mem.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "sizeOf",
        "alignOf",
        "ptrOffset",
        "ptrRead",
        "ptrWrite",
        "refWrite",
        "swap",
        "replace",
        "take",
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
fn stdlib_mem_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let _ = db.new_file("stdlib/core/option.aru".to_string(), OPTION_ARU.to_string());
    let mem_file = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    let main_src = r#"
import std.core.mem as mem

func testUsage(p1: mut ref int, p2: mut ref int): int {
    mem.swap<int>(p1, p2)
    return mem.replace<int>(p1, 42)
}

func testTake(opt: mut ref Option<int>): Option<int> {
    return mem.take<int>(opt)
}

func main(): int {
    let sz = mem.sizeOf<int>()
    let al = mem.alignOf<int>()
    if sz == 0 || al == 0 {
        return 1
    }
    return 0
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_mem = file_ide_diagnostics(&db, mem_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_mem: Vec<_> = diags_mem.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_mem.is_empty(),
        "unexpected errors in mem.aru: {error_diags_mem:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
