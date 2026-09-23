//! Tests for std.fs extensions (Metadata).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

mod common;

const FS_ARU: &str = include_str!("../../../stdlib/std/fs.aru");

#[test]
fn stdlib_fs_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/std/fs.aru".to_string(), FS_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("fs.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "OpenOptions",
        "readOnly",
        "writeOnly",
        "File",
        "fileExists",
        "Metadata",
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
fn stdlib_fs_metadata_usage() {
    let mut db = DatabaseImpl::default();
    for (path, source) in common::STDLIB_MODULES {
        let _ = db.new_file((*path).to_string(), (*source).to_string());
    }
    let fs_file = db.new_file("stdlib/std/fs.aru".to_string(), FS_ARU.to_string());
    let main_src = r#"
import std.fs as fs

func testMetadata(): int {
    let meta = fs.Metadata {
        file_size: 4096,
        is_dir: false,
        is_file: true,
        readonly: true,
    }
    if meta.size() != 4096 {
        return 1
    }
    if meta.isDir() {
        return 2
    }
    if !meta.isFile() {
        return 3
    }
    if !meta.isReadonly() {
        return 4
    }
    return 0
}

func main(): int {
    return testMetadata()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_fs = file_ide_diagnostics(&db, fs_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_fs: Vec<_> = diags_fs.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_fs.is_empty(),
        "unexpected errors in fs.aru: {error_diags_fs:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
