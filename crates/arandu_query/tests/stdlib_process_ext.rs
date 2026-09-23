//! Tests for std.process extensions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const PROCESS_ARU: &str = include_str!("../../../stdlib/std/process.aru");

#[test]
fn stdlib_process_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/std/process.aru".to_string(),
        PROCESS_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("process.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "exit",
        "ExitStatus",
        "exitStatus",
        "Command",
        "newCommand",
        "Child",
        "childFromPid",
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
fn stdlib_process_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let proc_file = db.new_file(
        "stdlib/std/process.aru".to_string(),
        PROCESS_ARU.to_string(),
    );
    let main_src = r#"
import std.process as process

func testProcess(): int {
    let status = process.exitStatus(0)
    if !status.success() {
        return 1
    }
    if status.code() != 0 {
        return 2
    }
    let cmd = process.newCommand("echo")
    if cmd.program() != "echo" {
        return 3
    }
    let child = process.childFromPid(1234)
    if child.id() != 1234 {
        return 4
    }
    return 0
}

func main(): int {
    return testProcess()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_proc = file_ide_diagnostics(&db, proc_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_proc: Vec<_> = diags_proc.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_proc.is_empty(),
        "unexpected errors in process.aru: {error_diags_proc:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
