//! Tests for std.path, std.io and std.fs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

mod common;

const PATH_ARU: &str = include_str!("../../../stdlib/std/path.aru");
const IO_ARU: &str = include_str!("../../../stdlib/std/io.aru");
const FS_ARU: &str = include_str!("../../../stdlib/std/fs.aru");

#[test]
fn stdlib_path_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/std/path.aru".to_string(), PATH_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("path.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "Path",
        "PathBuf",
        "pathFromStr",
        "newPathBuf",
        "isAbsolute",
        "join",
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
fn stdlib_io_fs_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file_io = db.new_file("stdlib/std/io.aru".to_string(), IO_ARU.to_string());
    match parse(&db, file_io).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("io.aru must parse; got {e}"),
    }
    let exports_io = exported_symbols(&db, file_io);
    for key in ["IoErrorKind", "IoError", "Read", "Write", "Stdout", "Stdin"] {
        assert!(
            exports_io.symbols.contains_key(key),
            "expected exported symbol `{key}` in io.aru, got {:?}",
            exports_io.symbols.keys().collect::<Vec<_>>()
        );
    }

    let file_fs = db.new_file("stdlib/std/fs.aru".to_string(), FS_ARU.to_string());
    match parse(&db, file_fs).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("fs.aru must parse; got {e}"),
    }
    let exports_fs = exported_symbols(&db, file_fs);
    for key in ["OpenOptions", "File", "readOnly", "writeOnly", "fileExists"] {
        assert!(
            exports_fs.symbols.contains_key(key),
            "expected exported symbol `{key}` in fs.aru, got {:?}",
            exports_fs.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_io_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let mut fs_file = None;
    for (path, source) in common::STDLIB_MODULES {
        let file = db.new_file((*path).to_string(), (*source).to_string());
        if *path == "stdlib/std/fs.aru" {
            fs_file = Some(file);
        }
    }
    let fs_file = fs_file.expect("fs module is in the canonical stdlib graph");

    let main_src = r#"
import std.path as path
import std.fs as fs

func testPath(): bool {
    let p = path.pathFromStr("/home/user/file.txt")
    return p.isAbsolute()
}

func testFs(): fs.OpenOptions {
    return fs.readOnly()
}

func main(): int {
    if testPath() {
        return 0
    }
    return 1
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
