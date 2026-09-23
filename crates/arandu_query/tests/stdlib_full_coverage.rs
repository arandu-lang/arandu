//! Type-check every standard-library module in one shared module graph.
#![allow(clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::parse;
use std::path::{Path, PathBuf};

fn collect_stdlib_files(directory: &Path, root: &Path, files: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .map(|entry| {
            entry.unwrap_or_else(|error| panic!("failed to read stdlib directory entry: {error}"))
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::path);

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_stdlib_files(&path, root, files);
        } else if path.extension().is_some_and(|extension| extension == "aru") {
            let relative = path
                .strip_prefix(root)
                .expect("stdlib file must stay beneath the stdlib root")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            files.push((format!("stdlib/{relative}"), source));
        }
    }
}

#[test]
fn every_stdlib_module_parses_and_type_checks_with_the_full_graph() {
    let stdlib_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    let stdlib_root = stdlib_root
        .canonicalize()
        .expect("workspace stdlib directory must exist");
    let mut sources = Vec::new();
    collect_stdlib_files(&stdlib_root, &stdlib_root, &mut sources);
    assert!(!sources.is_empty(), "stdlib must contain Arandu modules");

    let mut db = DatabaseImpl::default();
    let files: Vec<_> = sources
        .iter()
        .map(|(path, source)| {
            let file = db.new_file(path.clone(), source.clone());
            parse(&db, file)
                .as_ref()
                .unwrap_or_else(|error| panic!("{path} must parse: {error}"));
            (path.as_str(), file)
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
