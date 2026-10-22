//! Tests for std.alloc.deque and std.alloc.slab.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const DEQUE_ARU: &str = include_str!("../../../../stdlib/alloc/deque.aru");
const SLAB_ARU: &str = include_str!("../../../../stdlib/alloc/slab.aru");

#[test]
fn stdlib_deque_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/alloc/deque.aru".to_string(), DEQUE_ARU.to_string());
    assert!(parse(&db, file).as_ref().is_ok(), "deque.aru must parse");
    let exports = exported_symbols(&db, file);
    for name in [
        "VecDeque",
        "VecDeque.new",
        "VecDeque.withCapacity",
        "VecDeque.pushBack",
        "VecDeque.pushFront",
        "VecDeque.popBack",
        "VecDeque.popFront",
        "VecDeque.len",
        "VecDeque.isEmpty",
        "VecDeque.clear",
        "VecDeque.capacity",
    ] {
        assert!(
            exports.symbols.contains_key(name),
            "VecDeque missing `{name}`"
        );
    }
    assert!(
        !exports.symbols.contains_key("destroy"),
        "VecDeque must not export manual `destroy`"
    );
}

#[test]
fn stdlib_slab_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/alloc/slab.aru".to_string(), SLAB_ARU.to_string());
    assert!(parse(&db, file).as_ref().is_ok(), "slab.aru must parse");
    let exports = exported_symbols(&db, file);
    for name in [
        "Slab",
        "Slab.new",
        "Slab.withCapacity",
        "Slab.insert",
        "Slab.get",
        "Slab.remove",
        "Slab.len",
        "Slab.isEmpty",
        "Slab.clear",
        "Slab.contains",
    ] {
        assert!(exports.symbols.contains_key(name), "Slab missing `{name}`");
    }
    assert!(
        !exports.symbols.contains_key("destroy"),
        "Slab must not export manual `destroy`"
    );
}

#[test]
fn stdlib_deque_and_slab_usage_typechecks() {
    let mut db = DatabaseImpl::default();
    for (path, source) in super::common::STDLIB_MODULES {
        db.new_stdlib_file((*path).to_string(), (*source).to_string());
    }

    let main_file = db.new_file(
        "main.aru".to_string(),
        r#"
import std.alloc.deque as deque
import std.alloc.slab as slab

func main(): int {
    let mut q = deque.VecDeque<int>.new()
    q.pushBack(10)
    q.pushFront(5)
    if q.len() != (2 as usize) {
        return 1
    }
    let front = q.popFront()
    if front is Option.None {
        return 2
    }

    let mut s = slab.Slab<int>.new()
    let id1 = s.insert(100)
    let id2 = s.insert(200)
    if s.len() != (2 as usize) {
        return 3
    }
    let val = s.get(id1)
    if val is Option.None {
        return 4
    }
    let rem = s.remove(id1)
    if rem is Option.None {
        return 5
    }
    return 0
}
"#
        .to_string(),
    );

    let errors: Vec<_> = file_ide_diagnostics(&db, main_file)
        .iter()
        .filter(|diag| diag.severity == 0)
        .cloned()
        .collect();
    assert!(errors.is_empty(), "unexpected diagnostics: {errors:?}");
}
