//! Tests for std.time.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const TIME_ARU: &str = include_str!("../../../stdlib/std/time.aru");

#[test]
fn stdlib_time_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/std/time.aru".to_string(), TIME_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("time.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "monotonicNs",
        "Duration",
        "durationFromNanos",
        "durationFromMicros",
        "durationFromMillis",
        "durationFromSecs",
        "Instant",
        "now",
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
fn stdlib_time_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let time_file = db.new_file("stdlib/std/time.aru".to_string(), TIME_ARU.to_string());
    let main_src = r#"
import std.time as time

func testDuration(): int {
    let d1 = time.durationFromSecs(2)
    let d2 = time.durationFromMillis(500)
    let total = d1.add(d2)
    return total.asMillis()
}

func testInstant(): time.Duration {
    let start = time.now()
    return start.elapsed()
}

func main(): int {
    let ms = testDuration()
    if ms < 2500 {
        return 1
    }
    let d = testInstant()
    return d.asNanos()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_time = file_ide_diagnostics(&db, time_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_time: Vec<_> = diags_time.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_time.is_empty(),
        "unexpected errors in time.aru: {error_diags_time:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
