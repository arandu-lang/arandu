//! Tests for std.alloc.arena.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const ARENA_ARU: &str = include_str!("../../../stdlib/alloc/arena.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");

#[test]
fn stdlib_arena_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/alloc/arena.aru".to_string(), ARENA_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("arena.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = ["Arena", "new", "ScratchArena", "newScratch", "withCapacity"];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_arena_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let _ = db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    let _ = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    let arena_file = db.new_file("stdlib/alloc/arena.aru".to_string(), ARENA_ARU.to_string());
    let main_src = r#"
import std.alloc.arena as arena

func testArena(): int {
    let mut a = arena.new(1024)
    let p1 = a.alloc(32, 8)
    if p1 is Option.None {
        return 1
    }
    let used = a.allocatedBytes()
    if used < 32 {
        return 2
    }
    let cap = a.capacity()
    if cap != 1024 {
        return 3
    }
    let rem = a.remainingBytes()
    if rem > 1024 - 32 {
        return 4
    }
    let optTyped = a.allocTyped<int>(4)
    if optTyped is Option.None {
        return 5
    }
    let optSlice = a.allocSlice<int>(4)
    if optSlice is Option.None {
        return 6
    }
    a.reset()
    if a.allocatedBytes() != 0 {
        return 7
    }
    a.free()
    return 0
}

func testScratchArena(): int {
    let mut s = arena.withCapacity(2048)
    if s.capacity() != 2048 {
        return 10
    }
    let p1 = s.alloc(64, 16)
    if p1 is Option.None {
        return 11
    }
    if s.allocatedBytes() < 64 {
        return 12
    }
    let mark = s.checkpoint()
    let optSlice = s.allocSlice<int>(8)
    if optSlice is Option.None {
        return 13
    }
    let after_alloc = s.allocatedBytes()
    if after_alloc <= mark {
        return 14
    }
    let peak1 = s.peakBytes()
    if peak1 < after_alloc {
        return 15
    }

    // Scoped rollback to checkpoint
    s.rewind(mark)
    if s.allocatedBytes() != mark {
        return 16
    }
    // High-water mark is preserved across rewinds
    if s.peakBytes() != peak1 {
        return 17
    }

    // Full reset
    s.reset()
    if s.allocatedBytes() != 0 {
        return 18
    }
    // High-water mark is preserved across full reset
    if s.peakBytes() != peak1 {
        return 19
    }

    s.free()
    return 0
}

func main(): int {
    let r1 = testArena()
    if r1 != 0 {
        return r1
    }
    return testScratchArena()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_arena = file_ide_diagnostics(&db, arena_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_arena: Vec<_> = diags_arena.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_arena.is_empty(),
        "unexpected errors in arena.aru: {error_diags_arena:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
