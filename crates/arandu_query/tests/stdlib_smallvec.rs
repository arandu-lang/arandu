//! Tests for std.alloc.smallvec.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const SMALLVEC_ARU: &str = include_str!("../../../stdlib/alloc/smallvec.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");

#[test]
fn stdlib_smallvec_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/smallvec.aru".to_string(),
        SMALLVEC_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("smallvec.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = ["SmallVec4", "new"];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_smallvec_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let _ = db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    let _ = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    let _ = db.new_file("stdlib/core/option.aru".to_string(), OPTION_ARU.to_string());
    let smallvec_file = db.new_file(
        "stdlib/alloc/smallvec.aru".to_string(),
        SMALLVEC_ARU.to_string(),
    );

    let main_src = r#"
import std.alloc.smallvec as smallvec

func testSmallVec(): int {
    let mut sv = smallvec.new<int>()
    sv.push(10)
    sv.push(20)
    let l = sv.len()
    let is_h = sv.isHeap()
    if l == 2 && !is_h {
        return 0
    }
    return 1
}

func main(): int {
    return testSmallVec()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_sv = file_ide_diagnostics(&db, smallvec_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_sv: Vec<_> = diags_sv.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_sv.is_empty(),
        "unexpected errors in smallvec.aru: {error_diags_sv:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
