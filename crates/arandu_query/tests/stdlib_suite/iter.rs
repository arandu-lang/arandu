//! Tests for std.core.iter.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const ITER_ARU: &str = include_str!("../../../../stdlib/core/iter.aru");

#[test]
fn stdlib_iter_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/core/iter.aru".to_string(), ITER_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("ITER PARSE ERROR: {:?}", e);
            panic!("iter.aru must parse; got {e}");
        }
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "Iterator",
        "Take",
        "take",
        "Take.next",
        "Skip",
        "skip",
        "Skip.next",
        "Indexed",
        "Enumerate",
        "enumerate",
        "Enumerate.next",
        "StepBy",
        "stepBy",
        "StepBy.next",
        "count",
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
fn stdlib_iter_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "test_iter_usage.aru".to_string(),
        r#"
            module test_iter_usage

            import std.core.iter as iter
            import std.core.option as option

            public struct RangeIter {
                current: int
                endVal: int
            }

            public func range(startVal: int, endVal: int): RangeIter {
                return RangeIter {
                    current: startVal,
                    endVal: endVal,
                }
            }

            public func RangeIter.next(self: mut ref RangeIter): Option<int> {
                if self.current >= self.endVal {
                    return Option.None
                }
                let val = self.current
                self.current = self.current + 1
                return Option.Some(val)
            }

            public func testPipeline(): uint {
                let r = range(0, 10)
                let taken = iter.take(r, 5)
                let skipped = iter.skip(taken, 2)
                return iter.count(skipped)
            }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("test_iter_usage.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    assert!(exports.symbols.contains_key("RangeIter"));
    assert!(exports.symbols.contains_key("range"));
    assert!(exports.symbols.contains_key("testPipeline"));
}
