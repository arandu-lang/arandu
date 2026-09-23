//! Type-check every standard-library module in one shared module graph.
#![allow(clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::parse;

mod common;

#[test]
fn every_stdlib_module_parses_and_type_checks_with_the_full_graph() {
    let mut db = DatabaseImpl::default();
    let files: Vec<_> = common::STDLIB_MODULES
        .iter()
        .map(|(path, source)| {
            let file = db.new_file((*path).to_string(), (*source).to_string());
            parse(&db, file)
                .as_ref()
                .unwrap_or_else(|error| panic!("{path} must parse: {error}"));
            (*path, file)
        })
        .collect();

    let mut failures = Vec::new();
    for (path, file) in files {
        let errors: Vec<_> = file_ide_diagnostics(&db, file)
            .iter()
            .filter(|diagnostic| diagnostic.severity == 0)
            .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
            .collect();
        if !errors.is_empty() {
            failures.push(format!("{path}: {}", errors.join("; ")));
        }
    }

    assert!(
        failures.is_empty(),
        "stdlib type-check failures:\n{}",
        failures.join("\n")
    );
}
