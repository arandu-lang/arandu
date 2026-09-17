//! Deterministic checks for workspace boundaries that are easy to erode in a
//! seemingly local refactor.

use std::fs;
use std::path::{Path, PathBuf};

const PURE_CRATES: &[&str] = &[
    "arandu_base",
    "arandu_lexer",
    "arandu_parser",
    "arandu_middle",
    "arandu_resolve",
    "arandu_typeck",
    "arandu_mir",
    "arandu_semantics",
    "arandu_codegen",
    "arandu_backend_c",
    "arandu_backend_cranelift",
    "arandu_backend_wasm",
    "arandu_fmt",
    "arandu_doc",
    "arandu_ide",
    "arandu_web",
];

const FS_EFFECT_MARKERS: &[&str] = &[
    "std::fs",
    "use std::{fs",
    "fs::read(",
    "fs::read_to_string(",
    "fs::read_dir(",
    "fs::write(",
    "fs::remove_",
    "fs::rename(",
];

pub fn check(root: &Path) -> i32 {
    match violations(root) {
        Ok(violations) if violations.is_empty() => {
            println!(
                "check-architecture: ok (Salsa ownership and pure-crate filesystem boundaries)"
            );
            0
        }
        Ok(violations) => {
            eprintln!("check-architecture: boundary violation(s):");
            for violation in violations {
                eprintln!("  - {violation}");
            }
            1
        }
        Err(error) => {
            eprintln!("check-architecture: {error}");
            1
        }
    }
}

fn violations(root: &Path) -> Result<Vec<String>, String> {
    let crates = root.join("crates");
    let mut violations = Vec::new();

    for entry in sorted_entries(&crates)? {
        let crate_root = entry.path();
        if !crate_root.is_dir() {
            continue;
        }
        let Some(crate_name) = crate_root.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        let manifest = crate_root.join("Cargo.toml");
        if manifest.is_file() && crate_name != "arandu_query" && crate_name != "arandu_middle" {
            let text = read_utf8(&manifest)?;
            if text
                .lines()
                .any(|line| line.trim_start().starts_with("salsa ="))
            {
                violations.push(format!(
                    "{} declares Salsa directly; only arandu_query owns execution and arandu_middle declares shared DB contracts",
                    relative(root, &manifest)
                ));
            }
        }

        let source_root = crate_root.join("src");
        if !source_root.is_dir() {
            continue;
        }
        let mut rust_files = Vec::new();
        collect_rust_files(&source_root, &mut rust_files)?;
        rust_files.sort();
        for path in rust_files {
            let text = read_utf8(&path)?;
            let salsa_allowed = crate_name == "arandu_query"
                || (crate_name == "arandu_middle" && path == source_root.join("db.rs"));
            if !salsa_allowed && (text.contains("salsa::") || text.contains("#[salsa")) {
                violations.push(format!(
                    "{} uses Salsa outside its owner/shared DB contract",
                    relative(root, &path)
                ));
            }

            let profiling_sink =
                crate_name == "arandu_base" && path == source_root.join("tracing_bridge.rs");
            if PURE_CRATES.contains(&crate_name)
                && !profiling_sink
                && FS_EFFECT_MARKERS.iter().any(|marker| text.contains(marker))
            {
                violations.push(format!(
                    "{} performs filesystem I/O in a pure compiler crate",
                    relative(root, &path)
                ));
            }
        }
    }

    Ok(violations)
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {} entry: {error}", path.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn collect_rust_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in sorted_entries(path)? {
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry_path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_rust_files(&entry_path, files)?;
        } else if entry_path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(entry_path);
        }
    }
    Ok(())
}

fn read_utf8(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {} as UTF-8: {error}", path.display()))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_cover_reads_writes_and_salsa_macros() {
        assert!(FS_EFFECT_MARKERS.contains(&"fs::read_to_string("));
        assert!(FS_EFFECT_MARKERS.contains(&"fs::write("));
        assert!("#[salsa::tracked]".contains("#[salsa"));
    }
}
