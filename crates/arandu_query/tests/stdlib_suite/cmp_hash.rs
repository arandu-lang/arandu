//! Tests for std.core.cmp and std.core.hash.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const CMP_ARU: &str = include_str!("../../../../stdlib/core/cmp.aru");
const HASH_ARU: &str = include_str!("../../../../stdlib/core/hash.aru");

#[test]
fn stdlib_cmp_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/cmp.aru".to_string(), CMP_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("cmp.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "Ordering",
        "Ordering.isLess",
        "Ordering.isEqual",
        "Ordering.isGreater",
        "Ordering.then",
        "PartialEq",
        "Eq",
        "Ord",
        "min",
        "max",
        "clamp",
        "minBy",
        "maxBy",
        "clampBy",
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
fn stdlib_hash_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/hash.aru".to_string(), HASH_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("PARSE ERROR: {:?}", e);
            panic!("hash.aru must parse; got {e}");
        }
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "Hasher",
        "Hash",
        "FnvHasher",
        "FnvHasher.new",
        "FnvHasher.finish",
        "FnvHasher.write",
        "FnvHasher.writeU8",
        "FnvHasher.writeU16",
        "FnvHasher.writeU32",
        "FnvHasher.writeU64",
        "FnvHasher.writeInt",
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
fn stdlib_cmp_and_hash_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "test_usage.aru".to_string(),
        r#"
            module test_usage

            import std.core.cmp as cmp
            import std.core.hash as hash

            public struct Point {
                x: int
                y: int
            }

            public func Point.eq(self: ref Point, other: ref Point): bool {
                return self.x == other.x && self.y == other.y
            }

            public func Point.cmp(self: ref Point, other: ref Point): cmp.Ordering {
                if self.x < other.x {
                    return cmp.Ordering.Less
                }
                if self.x > other.x {
                    return cmp.Ordering.Greater
                }
                if self.y < other.y {
                    return cmp.Ordering.Less
                }
                if self.y > other.y {
                    return cmp.Ordering.Greater
                }
                return cmp.Ordering.Equal
            }

            public func Point.hash<H: hash.Hasher>(self: ref Point, state: mut ref H): void {
                state.writeInt(self.x)
                state.writeInt(self.y)
            }

            public func testCmp(): bool {
                let p1 = Point { x: 1, y: 2 }
                let p2 = Point { x: 1, y: 3 }
                let order = p1.cmp(ref p2)
                return order.isLess()
            }

            public func testHash(): u64 {
                let mut h = hash.FnvHasher.new()
                let p = Point { x: 42, y: 100 }
                p.hash(ref h)
                return h.finish()
            }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("test_usage.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    assert!(exports.symbols.contains_key("testCmp"));
    assert!(exports.symbols.contains_key("testHash"));
}
