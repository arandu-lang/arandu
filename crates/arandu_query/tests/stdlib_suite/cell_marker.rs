//! Tests for std.core.cell and std.core.marker.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const CELL_ARU: &str = include_str!("../../../../stdlib/core/cell.aru");
const MARKER_ARU: &str = include_str!("../../../../stdlib/core/marker.aru");

#[test]
fn stdlib_cell_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/cell.aru".to_string(), CELL_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("cell.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "UnsafeCell",
        "UnsafeCell.new",
        "UnsafeCell.intoInner",
        "Cell",
        "Cell.new",
        "Cell.get",
        "Cell.set",
        "Cell.replace",
        "Cell.intoInner",
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
fn stdlib_marker_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/core/marker.aru".to_string(), MARKER_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("marker.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "PhantomData",
        "PhantomData.new",
        "phantom",
        "Copy",
        "Send",
        "Sync",
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
fn stdlib_cell_marker_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "test_cell_marker.aru".to_string(),
        r#"
            module test_cell_marker

            import std.core.cell as cell
            import std.core.marker as marker

            public struct TypedHandle<T> {
                id: int
                marker: marker.PhantomData<T>
            }

            public func newHandle<T>(id: int): TypedHandle<T> {
                return TypedHandle<T> {
                    id: id,
                    marker: marker.PhantomData.new<T>(),
                }
            }

            public func testCell(): int {
                let mut c = cell.Cell.new(10)
                let old = c.replace(20)
                return old + c.get()
            }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("test_cell_marker.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    assert!(exports.symbols.contains_key("TypedHandle"));
    assert!(exports.symbols.contains_key("newHandle"));
    assert!(exports.symbols.contains_key("testCell"));
}
