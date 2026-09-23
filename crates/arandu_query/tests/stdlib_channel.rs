//! Tests for std.runtime.channel Channel and std.runtime.mutex AsyncMutex
//! concurrency primitives.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const CHANNEL_ARU: &str = include_str!("../../../stdlib/std/runtime/channel.aru");
const MUTEX_ARU: &str = include_str!("../../../stdlib/std/runtime/mutex.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");

#[test]
fn stdlib_runtime_exports_channel_and_async_mutex() {
    let mut db = DatabaseImpl::default();
    let channel_file = db.new_file(
        "stdlib/std/runtime/channel.aru".to_string(),
        CHANNEL_ARU.to_string(),
    );
    match parse(&db, channel_file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("channel.aru must parse; got {e}"),
    }
    let channel_exports = exported_symbols(&db, channel_file);
    let channel_expected = ["Channel", "newChannel"];
    for key in channel_expected {
        assert!(
            channel_exports.symbols.contains_key(key),
            "expected exported symbol `{key}` from channel.aru, got {:?}",
            channel_exports.symbols.keys().collect::<Vec<_>>()
        );
    }

    let mutex_file = db.new_file(
        "stdlib/std/runtime/mutex.aru".to_string(),
        MUTEX_ARU.to_string(),
    );
    match parse(&db, mutex_file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("mutex.aru must parse; got {e}"),
    }
    let mutex_exports = exported_symbols(&db, mutex_file);
    let mutex_expected = ["AsyncMutex", "newAsyncMutex"];
    for key in mutex_expected {
        assert!(
            mutex_exports.symbols.contains_key(key),
            "expected exported symbol `{key}` from mutex.aru, got {:?}",
            mutex_exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_channel_and_mutex_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let _mem_file = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    let _intrinsics_file = db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    let _channel_file = db.new_file(
        "stdlib/std/runtime/channel.aru".to_string(),
        CHANNEL_ARU.to_string(),
    );
    let _mutex_file = db.new_file(
        "stdlib/std/runtime/mutex.aru".to_string(),
        MUTEX_ARU.to_string(),
    );
    let main_src = r#"
import std.runtime.channel as rch
import std.runtime.mutex as rmut

func testChannel(): int {
    let mut ch = rch.newChannel<int>(4)
    if !ch.isEmpty() || ch.len() != 0 || ch.capacity() != 4 {
        return 1
    }
    let sent1 = ch.trySend(10)
    let sent2 = ch.trySend(20)
    if !sent1 || !sent2 || ch.len() != 2 {
        return 2
    }
    let r1 = ch.tryRecv()
    let r2 = ch.tryRecv()
    match r1 {
        Some(v) => {
            if v != 10 { return 3 }
        }
        None => { return 4 }
    }
    match r2 {
        Some(v) => {
            if v != 20 { return 5 }
        }
        None => { return 6 }
    }
    if !ch.isEmpty() {
        return 7
    }
    ch.destroy()
    return 0
}

func testAsyncMutex(): int {
    let mut m = rmut.newAsyncMutex<int>(42)
    if m.isLocked() {
        return 10
    }
    let locked = m.tryLock()
    if !locked || !m.isLocked() {
        return 11
    }
    let locked_again = m.tryLock()
    if locked_again {
        return 12
    }
    if m.get() != 42 {
        return 13
    }
    m.store(99)
    if m.get() != 99 {
        return 14
    }
    m.unlock()
    if m.isLocked() {
        return 15
    }
    return 0
}

func main(): int {
    let c = testChannel()
    if c != 0 {
        return c
    }
    return testAsyncMutex()
}
"#;
    let main_file = db.new_file("src/main.aru".to_string(), main_src.to_string());
    let diags = file_ide_diagnostics(&db, main_file);
    let diags_summary: Vec<_> = diags.iter().map(|d| (&d.code, &d.message)).collect();
    assert!(
        diags.is_empty(),
        "expected zero diagnostics for channel and async mutex usage, got: {diags_summary:?}"
    );
}
