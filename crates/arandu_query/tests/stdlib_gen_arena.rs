//! Tests for std.alloc.gen_arena.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const GEN_ARENA_ARU: &str = include_str!("../../../stdlib/alloc/gen_arena.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");

#[test]
fn stdlib_gen_arena_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/gen_arena.aru".to_string(),
        GEN_ARENA_ARU.to_string(),
    );
    assert!(
        parse(&db, file).as_ref().is_ok(),
        "gen_arena.aru must parse"
    );
    let exports = exported_symbols(&db, file);
    for name in [
        "GenArena", "GenRef", "new", "insert", "get", "remove", "destroy",
    ] {
        assert!(exports.symbols.contains_key(name), "missing `{name}`");
    }
}

#[test]
fn stdlib_gen_arena_usage_typechecks() {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    db.new_file("stdlib/core/option.aru".to_string(), OPTION_ARU.to_string());
    let arena_file = db.new_file(
        "stdlib/alloc/gen_arena.aru".to_string(),
        GEN_ARENA_ARU.to_string(),
    );
    let main_file = db.new_file(
        "main.aru".to_string(),
        r#"
import std.alloc.gen_arena as arena

func main(): int {
    let mut values = arena.new<int>()
    let first = arena.insert(values, 7)
    if arena.get(values, first) != 7 {
        return 1
    }
    let removed = arena.remove(values, first)
    if removed is Option.None {
        return 2
    }
    arena.destroy(values)
    return 0
}
"#
        .to_string(),
    );

    for file in [arena_file, main_file] {
        let errors: Vec<_> = file_ide_diagnostics(&db, file)
            .iter()
            .filter(|diag| diag.severity == 0)
            .collect();
        assert!(errors.is_empty(), "unexpected diagnostics: {errors:?}");
    }
}
