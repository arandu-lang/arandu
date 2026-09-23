//! Tests for std.io.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const IO_ARU: &str = include_str!("../../../stdlib/std/io.aru");
const SLICE_ARU: &str = include_str!("../../../stdlib/core/slice.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");
const RESULT_ARU: &str = include_str!("../../../stdlib/core/result.aru");

#[test]
fn stdlib_io_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/std/io.aru".to_string(), IO_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("io.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "IoErrorKind",
        "IoError",
        "newIoError",
        "Read",
        "Write",
        "BufRead",
        "BufReader",
        "newBufReader",
        "BufWriter",
        "newBufWriter",
        "stdout",
        "stderr",
        "stdin",
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
fn stdlib_io_bufreader_and_bufwriter_usage() {
    let mut db = DatabaseImpl::default();
    let _ = db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    let _ = db.new_file("stdlib/core/option.aru".to_string(), OPTION_ARU.to_string());
    let _ = db.new_file("stdlib/core/result.aru".to_string(), RESULT_ARU.to_string());
    let _ = db.new_file("stdlib/core/slice.aru".to_string(), SLICE_ARU.to_string());
    let io_file = db.new_file("stdlib/std/io.aru".to_string(), IO_ARU.to_string());

    let main_src = r#"
import std.io as io
import std.core.slice as slice

struct MockStream {
    closed: bool
}

func MockStream.read(self: mut ref MockStream, buf: mut ref []u8): Result<uint, io.IoError> {
    return Result.Ok(0 as uint)
}

func MockStream.write(self: mut ref MockStream, buf: []u8): Result<uint, io.IoError> {
    return Result.Ok(slice.len<u8>(buf))
}

func MockStream.flush(self: mut ref MockStream): Result<bool, io.IoError> {
    return Result.Ok(true)
}

func testBufReader(reader: mut ref io.BufReader<MockStream>, dest: mut ref []u8): Result<uint, io.IoError> {
    return reader.read(dest)
}

func testBufWriter(writer: mut ref io.BufWriter<MockStream>, src: []u8): Result<uint, io.IoError> {
    return writer.write(src)
}

func main(): int {
    return 0
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_io = file_ide_diagnostics(&db, io_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_io: Vec<_> = diags_io.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_io.is_empty(),
        "unexpected errors in io.aru: {error_diags_io:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
