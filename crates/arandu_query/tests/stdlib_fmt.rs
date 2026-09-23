//! Tests for std.core.fmt.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const FMT_ARU: &str = include_str!("../../../stdlib/core/fmt.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const RESULT_ARU: &str = include_str!("../../../stdlib/core/result.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");
const STR_ARU: &str = include_str!("../../../stdlib/core/str.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");
const SLICE_ARU: &str = include_str!("../../../stdlib/core/slice.aru");

#[test]
fn stdlib_fmt_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/fmt.aru".to_string(), FMT_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("fmt.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = ["FmtError", "Formatter", "newFormatter", "Display", "Debug"];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_fmt_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let _ = db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    let _ = db.new_file("stdlib/core/slice.aru".to_string(), SLICE_ARU.to_string());
    let _ = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    let _ = db.new_file("stdlib/core/option.aru".to_string(), OPTION_ARU.to_string());
    let _ = db.new_file("stdlib/core/result.aru".to_string(), RESULT_ARU.to_string());
    let _ = db.new_file("stdlib/core/str.aru".to_string(), STR_ARU.to_string());
    let fmt_file = db.new_file("stdlib/core/fmt.aru".to_string(), FMT_ARU.to_string());

    let main_src = r#"
import std.core.fmt as fmt

func testFormatter(f: mut ref fmt.Formatter): bool {
    let res = f.writeBool(true)
    let b = f.writeByte(10 as u8)
    match res {
        Ok(_) => {}
        Err(_) => { return false }
    }
    match b {
        Ok(_) => {}
        Err(_) => { return false }
    }
    return true
}

func testNew(buf: []u8): fmt.Formatter {
    return fmt.newFormatter(buf)
}

func main(): int {
    return 0
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_fmt = file_ide_diagnostics(&db, fmt_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_fmt: Vec<_> = diags_fmt.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_fmt.is_empty(),
        "unexpected errors in fmt.aru: {error_diags_fmt:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
