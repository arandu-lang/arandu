//! Tests for std.core.atomic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const ATOMIC_ARU: &str = include_str!("../../../../stdlib/core/atomic.aru");

#[test]
fn stdlib_atomic_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/atomic.aru".to_string(), ATOMIC_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("atomic.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "MemoryOrder",
        "AtomicBool",
        "AtomicBool.new",
        "AtomicBool.load",
        "AtomicBool.store",
        "AtomicBool.swap",
        "AtomicInt",
        "AtomicInt.new",
        "AtomicInt.load",
        "AtomicInt.store",
        "AtomicInt.swap",
        "AtomicInt.fetchAdd",
        "AtomicInt.fetchSub",
        "AtomicInt.compareExchange",
        "AtomicUint",
        "AtomicUint.new",
        "AtomicUint.load",
        "AtomicUint.store",
        "AtomicUint.fetchAdd",
        "AtomicUint.fetchSub",
        "AtomicPtr",
        "AtomicPtr.new",
        "AtomicPtr.load",
        "AtomicPtr.store",
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
fn stdlib_atomic_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "test_atomic_usage.aru".to_string(),
        r#"
            module test_atomic_usage

            import std.core.atomic as atomic

            public struct Counter {
                count: atomic.AtomicInt
            }

            public func newCounter(): Counter {
                return Counter {
                    count: atomic.AtomicInt.new(0),
                }
            }

            public func Counter.inc(self: mut ref Counter): int {
                return self.count.fetchAdd(1, atomic.MemoryOrder.SeqCst)
            }

            public func testAtomic(): int {
                let mut c = newCounter()
                c.inc()
                c.inc()
                return c.count.load(atomic.MemoryOrder.SeqCst)
            }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("test_atomic_usage.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    assert!(exports.symbols.contains_key("Counter"));
    assert!(exports.symbols.contains_key("newCounter"));
    assert!(exports.symbols.contains_key("testAtomic"));
}
