//! Tests for std.parallel exports and syntax.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const PARALLEL_ARU: &str = include_str!("../../../stdlib/std/parallel.aru");

#[test]
fn stdlib_parallel_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/std/parallel.aru".to_string(),
        PARALLEL_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("parallel.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "ParallelJob",
        "Combine",
        "AccumulatorInit",
        "CopyIdentity",
        "ParallelError",
        "granularityCutoff",
        "ChunkContext",
        "dispatchChunk",
        "ChunkContextWithInit",
        "dispatchChunkWithInit",
        "foldSequential",
        "parallelFold",
        "parallelFoldWithGrain",
        "parallelFoldWithInit",
    ];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}
