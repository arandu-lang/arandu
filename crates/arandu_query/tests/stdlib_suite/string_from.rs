//! `from` is a soft keyword: it must work both as the import keyword
//! and as an ordinary free/method name.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const STRING_ARU: &str = include_str!("../../../../stdlib/alloc/string.aru");

#[test]
fn stdlib_string_exports_from_and_string_from() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/string.aru".to_string(),
        STRING_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("string.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    for key in ["String.new", "String.withCapacity", "String.from"] {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn from_is_usable_as_free_and_method_name() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "lib.aru".to_string(),
        r#"
            public func from(value: str): str { return value }
            public struct Widget { x: int }
            public func Widget.from(shared self): int { return self.x }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("`from` as free/method name must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    for key in ["from", "Widget.from"] {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}
