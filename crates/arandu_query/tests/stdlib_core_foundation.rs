//! Regression coverage for the freestanding `std.core` foundation modules.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const FIXED_ARU: &str = include_str!("../../../stdlib/core/fixed.aru");
const IO_ARU: &str = include_str!("../../../stdlib/core/io.aru");
const STR_ARU: &str = include_str!("../../../stdlib/core/str.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");
const OPTION_ARU: &str = include_str!("../../../stdlib/core/option.aru");
const RESULT_ARU: &str = include_str!("../../../stdlib/core/result.aru");
const SLICE_ARU: &str = include_str!("../../../stdlib/core/slice.aru");

#[test]
fn foundation_modules_parse_and_export_their_contracts() {
    let mut db = DatabaseImpl::default();
    for (path, source, expected) in [
        (
            "stdlib/core/fixed.aru",
            FIXED_ARU,
            &["Q16_16", "fromRaw", "fromInt"][..],
        ),
        (
            "stdlib/core/io.aru",
            IO_ARU,
            &[
                "Reader",
                "Writer",
                "Seeker",
                "SliceReader",
                "newSliceReader",
                "SliceWriter",
                "newSliceWriter",
            ][..],
        ),
        (
            "stdlib/core/str.aru",
            STR_ARU,
            &[
                "isEmpty",
                "lenBytes",
                "startsWith",
                "endsWith",
                "contains",
                "find",
            ][..],
        ),
    ] {
        let file = db.new_file(path.to_string(), source.to_string());
        parse(&db, file)
            .as_ref()
            .unwrap_or_else(|error| panic!("{path} must parse: {error}"));
        let exports = exported_symbols(&db, file);
        for symbol in expected {
            assert!(
                exports.symbols.contains_key(*symbol),
                "{path} must export `{symbol}`"
            );
        }
    }
}

#[test]
fn foundation_modules_are_freestanding_and_type_check_together() {
    assert!(!FIXED_ARU.contains("extern \"C\""));
    assert!(!IO_ARU.contains("extern \"C\""));
    assert!(!STR_ARU.contains("extern \"C\""));

    let mut db = DatabaseImpl::default();
    let mut foundations = Vec::new();
    for (path, source) in [
        ("stdlib/core/intrinsics.aru", INTRINSICS_ARU),
        ("stdlib/core/option.aru", OPTION_ARU),
        ("stdlib/core/result.aru", RESULT_ARU),
        ("stdlib/core/slice.aru", SLICE_ARU),
        ("stdlib/core/fixed.aru", FIXED_ARU),
        ("stdlib/core/io.aru", IO_ARU),
        ("stdlib/core/str.aru", STR_ARU),
    ] {
        let file = db.new_file(path.to_string(), source.to_string());
        if matches!(
            path,
            "stdlib/core/fixed.aru" | "stdlib/core/io.aru" | "stdlib/core/str.aru"
        ) {
            foundations.push((path, file));
        }
    }
    for (path, file) in foundations {
        let errors: Vec<_> = file_ide_diagnostics(&db, file)
            .iter()
            .filter(|diagnostic| diagnostic.severity == 0)
            .cloned()
            .collect();
        assert!(errors.is_empty(), "unexpected errors in {path}: {errors:?}");
    }
}
