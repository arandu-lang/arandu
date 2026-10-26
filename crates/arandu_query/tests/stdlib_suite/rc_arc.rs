//! Tests for std.alloc.rc and std.alloc.arc.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const RC_ARU: &str = include_str!("../../../../stdlib/alloc/rc.aru");
const ARC_ARU: &str = include_str!("../../../../stdlib/alloc/arc.aru");

#[test]
fn stdlib_rc_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/alloc/rc.aru".to_string(), RC_ARU.to_string());
    assert!(parse(&db, file).as_ref().is_ok(), "rc.aru must parse");
    let exports = exported_symbols(&db, file);
    for name in [
        "Rc",
        "Rc.new",
        "Rc.tryNew",
        "Rc.get",
        "Rc.getMut",
        "Rc.tryUnwrap",
        "Rc.clone",
        "Rc.strongCount",
        "Rc.weakCount",
        "Rc.isUnique",
        "Rc.downgrade",
        "Weak",
        "Weak.clone",
        "Weak.upgrade",
    ] {
        assert!(exports.symbols.contains_key(name), "Rc missing `{name}`");
    }
    assert!(
        !exports.symbols.contains_key("destroy"),
        "Rc must not export manual `destroy`"
    );
}

#[test]
fn stdlib_arc_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/alloc/arc.aru".to_string(), ARC_ARU.to_string());
    assert!(parse(&db, file).as_ref().is_ok(), "arc.aru must parse");
    let exports = exported_symbols(&db, file);
    for name in [
        "Arc",
        "Arc.new",
        "Arc.tryNew",
        "Arc.get",
        "Arc.getMut",
        "Arc.tryUnwrap",
        "Arc.clone",
        "Arc.strongCount",
        "Arc.weakCount",
        "Arc.isUnique",
        "Arc.downgrade",
        "ArcWeak",
        "Weak",
    ] {
        assert!(exports.symbols.contains_key(name), "Arc missing `{name}`");
    }
    assert!(
        !exports.symbols.contains_key("destroy"),
        "Arc must not export manual `destroy`"
    );
}

#[test]
fn stdlib_rc_and_arc_usage_typechecks() {
    let mut db = DatabaseImpl::default();
    for (path, source) in super::common::STDLIB_MODULES {
        db.new_stdlib_file((*path).to_string(), (*source).to_string());
    }

    let main_file = db.new_file(
        "main.aru".to_string(),
        r#"
import std.alloc.rc as rc
import std.alloc.arc as arc

func main(): int {
    let r = rc.Rc<int>.new(42)
    let r2 = r.clone()
    if r.strongCount() != (2 as usize) {
        return 1
    }
    let w = r.downgrade()
    if r.weakCount() != (1 as usize) {
        return 2
    }
    let upgraded = w.upgrade()
    if upgraded is Option.None {
        return 3
    }

    let a = arc.Arc<int>.new(100)
    let a2 = a.clone()
    if a.strongCount() != (2 as usize) {
        return 4
    }
    let aw = a.downgrade()
    if a.weakCount() != (1 as usize) {
        return 5
    }
    let a_upgraded = aw.upgrade()
    if a_upgraded is Option.None {
        return 6
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
