//! Tests for std.alloc.bitset.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const BITSET_ARU: &str = include_str!("../../../stdlib/alloc/bitset.aru");
const VEC_ARU: &str = include_str!("../../../stdlib/alloc/vec.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");

#[test]
fn stdlib_bitset_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/bitset.aru".to_string(),
        BITSET_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("bitset.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "BitSet",
        "bitsetNew",
        "bitsetWithCapacity",
        "BitSet.contains",
        "BitSet.insert",
        "BitSet.remove",
        "BitSet.clear",
        "BitSet.unionWith",
        "BitSet.intersectWith",
        "BitSet.differenceWith",
        "BitSet.countOnes",
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
fn stdlib_bitset_usage_in_program() {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    db.new_file("stdlib/core/option.aru".to_string(), OPTION_ARU.to_string());
    db.new_file("stdlib/alloc/vec.aru".to_string(), VEC_ARU.to_string());
    let bitset_file = db.new_file(
        "stdlib/alloc/bitset.aru".to_string(),
        BITSET_ARU.to_string(),
    );
    let file = db.new_file(
        "test_bitset_usage.aru".to_string(),
        r#"
            module test_bitset_usage

            import std.alloc.bitset as bitset

            public func testBits(): uint {
                let mut bs = bitset.bitsetNew()
                bs.insert(1)
                bs.insert(65)
                bs.insert(128)
                return bs.countOnes()
            }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("test_bitset_usage.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    assert!(exports.symbols.contains_key("testBits"));
    for (path, checked_file) in [("bitset.aru", bitset_file), ("usage", file)] {
        let errors: Vec<_> = file_ide_diagnostics(&db, checked_file)
            .iter()
            .filter(|diag| diag.severity == 0)
            .collect();
        assert!(
            errors.is_empty(),
            "unexpected diagnostics in {path}: {errors:?}"
        );
    }
}
