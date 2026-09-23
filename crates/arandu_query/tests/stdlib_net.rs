//! Tests for std.net.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

mod common;

const NET_ARU: &str = include_str!("../../../stdlib/std/net.aru");

#[test]
fn stdlib_net_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/std/net.aru".to_string(), NET_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("net.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = ["SocketAddr", "socketAddr", "TcpStream", "TcpListener"];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_net_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let net_file = load_stdlib(&mut db);
    let main_src = r#"
import std.net as net

func testAddr(): net.SocketAddr {
    return net.socketAddr("127.0.0.1", 8080)
}

func main(): int {
    let addr = testAddr()
    if addr.port != 8080 {
        return 1
    }
    return 0
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_net = file_ide_diagnostics(&db, net_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_net: Vec<_> = diags_net.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_net.is_empty(),
        "unexpected errors in net.aru: {error_diags_net:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}

#[test]
fn stdlib_net_tcp_stream_slice_read_write_safe() {
    let mut db = DatabaseImpl::default();
    let net_file = load_stdlib(&mut db);

    let main_src = r#"
import std.net as net
import std.io as io

func readGeneric<R: io.Read>(reader: mut ref R, buf: mut ref []u8): Result<uint, io.IoError> {
    return reader.read(buf)
}

func writeGeneric<W: io.Write>(writer: mut ref W, buf: []u8): Result<uint, io.IoError> {
    let _ = writer.flush()
    return writer.write(buf)
}

func sendData(stream: mut ref net.TcpStream, data: []u8): bool {
    let res: Result<uint, io.IoError> = writeGeneric<net.TcpStream>(stream, data)
    match res {
        Result.Ok(_) => { return true }
        Result.Err(_) => { return false }
    }
}

func recvData(stream: mut ref net.TcpStream, buf: mut ref []u8): bool {
    let res: Result<uint, io.IoError> = readGeneric<net.TcpStream>(stream, buf)
    match res {
        Result.Ok(_) => { return true }
        Result.Err(_) => { return false }
    }
}

func main(): int {
    return 0
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_net = file_ide_diagnostics(&db, net_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_net: Vec<_> = diags_net.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_net.is_empty(),
        "unexpected errors in net.aru: {error_diags_net:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}

fn load_stdlib(db: &mut DatabaseImpl) -> arandu_query::db::SourceFile {
    let mut net_file = None;
    for (path, source) in common::STDLIB_MODULES {
        let file = db.new_file((*path).to_string(), (*source).to_string());
        if *path == "stdlib/std/net.aru" {
            net_file = Some(file);
        }
    }
    net_file.expect("net module is in the canonical stdlib graph")
}
