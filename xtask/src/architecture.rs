//! Deterministic checks for workspace boundaries that are easy to erode in a
//! seemingly local refactor.

use std::collections::{BTreeMap, BTreeSet};
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
    "arandu_testing_contract",
    "arandu_backend_c",
    "arandu_backend_cranelift",
    "arandu_backend_wasm",
    "arandu_fmt",
    "arandu_doc",
    "arandu_ide",
    "arandu_web",
];

const BACKEND_CRATES: &[&str] = &[
    "arandu_backend_c",
    "arandu_backend_cranelift",
    "arandu_backend_llvm",
    "arandu_backend_wasm",
];

/// Backends consume validated AMIR, layout and the codegen contract; front-end
/// phases (lexer, parser, resolve, typeck, HIR lowering) stay out of reach.
const BACKEND_ALLOWED_DEPENDENCIES: &[&str] = &[
    "arandu_base",
    "arandu_codegen",
    "arandu_diagnostics",
    "arandu_middle",
    "arandu_mir",
    "arandu_runtime",
    "arandu_testing_contract",
];

/// Existing backend edges that predate the boundary. The list may only shrink:
/// an entry whose edge no longer exists is reported so it gets removed.
const BACKEND_DEPENDENCY_DEBT: &[(&str, &str)] = &[
    ("arandu_backend_c", "arandu_semantics"),
    ("arandu_backend_cranelift", "arandu_semantics"),
    ("arandu_backend_llvm", "arandu_semantics"),
    ("arandu_backend_wasm", "arandu_semantics"),
];

/// Compiler phases and their contracts. They must never depend on backends,
/// the query layer or drivers.
const PHASE_CRATES: &[&str] = &[
    "arandu_base",
    "arandu_codegen",
    "arandu_diagnostics",
    "arandu_lexer",
    "arandu_middle",
    "arandu_mir",
    "arandu_parser",
    "arandu_resolve",
    "arandu_semantics",
    "arandu_testing_contract",
    "arandu_typeck",
];

const DOWNSTREAM_CRATES: &[&str] = &[
    "arandu_backend_c",
    "arandu_backend_cranelift",
    "arandu_backend_llvm",
    "arandu_backend_wasm",
    "arandu_cli",
    "arandu_fuzz_support",
    "arandu_ide",
    "arandu_lsp",
    "arandu_query",
    "arandu_test_support",
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
                "check-architecture: ok (Salsa ownership, crate dependency direction and pure-crate filesystem boundaries)"
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
    let mut violations = runtime_dependency_violations(&read_utf8(&root.join("Cargo.lock"))?)?;
    let mut dependencies = BTreeMap::new();

    for entry in sorted_entries(&crates)? {
        let crate_root = entry.path();
        if !crate_root.is_dir() {
            continue;
        }
        let Some(crate_name) = crate_root.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        let manifest = crate_root.join("Cargo.toml");
        if manifest.is_file() {
            let text = read_utf8(&manifest)?;
            dependencies.insert(
                crate_name.to_owned(),
                normal_dependencies(&text)
                    .map_err(|error| format!("{}: {error}", relative(root, &manifest)))?,
            );
            if crate_name != "arandu_query"
                && crate_name != "arandu_middle"
                && text
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

    violations.extend(dependency_direction_violations(&dependencies));
    Ok(violations)
}

/// Normal (non-dev, non-build) dependencies, including target-specific ones,
/// keyed by package name so renamed dependencies cannot hide an edge.
fn normal_dependencies(manifest: &str) -> Result<BTreeSet<String>, String> {
    let document: toml::Value =
        toml::from_str(manifest).map_err(|error| format!("parse manifest: {error}"))?;
    let mut tables = Vec::new();
    tables.extend(document.get("dependencies"));
    if let Some(targets) = document.get("target").and_then(toml::Value::as_table) {
        tables.extend(
            targets
                .values()
                .filter_map(|target| target.get("dependencies")),
        );
    }
    let mut names = BTreeSet::new();
    for table in tables {
        let table = table.as_table().ok_or("dependencies must be a table")?;
        for (key, spec) in table {
            let name = spec
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

fn dependency_direction_violations(
    dependencies: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    let mut violations = Vec::new();
    for (krate, deps) in dependencies {
        let krate = krate.as_str();
        for dep in deps.iter().map(String::as_str) {
            if BACKEND_CRATES.contains(&krate)
                && dep.starts_with("arandu_")
                && !BACKEND_ALLOWED_DEPENDENCIES.contains(&dep)
                && !BACKEND_DEPENDENCY_DEBT.contains(&(krate, dep))
            {
                violations.push(format!(
                    "{krate} depends on {dep}; backends consume AMIR through arandu_middle, arandu_mir and arandu_codegen, not front-end phases"
                ));
            }
            if PHASE_CRATES.contains(&krate) && DOWNSTREAM_CRATES.contains(&dep) {
                violations.push(format!(
                    "{krate} depends on downstream crate {dep}; compiler phases must not depend on backends, queries or drivers"
                ));
            }
        }
    }
    for &(krate, dep) in BACKEND_DEPENDENCY_DEBT {
        if !dependencies
            .get(krate)
            .is_some_and(|deps| deps.contains(dep))
        {
            violations.push(format!(
                "{krate} no longer depends on {dep}; remove the edge from BACKEND_DEPENDENCY_DEBT"
            ));
        }
    }
    violations
}

/// The lock includes target-specific and dev dependencies. Check them all
/// conservatively so joint workspace builds cannot reintroduce compiler
/// initializers into the runtime through Cargo feature unification.
fn runtime_dependency_violations(lock: &str) -> Result<Vec<String>, String> {
    let document: toml::Value =
        toml::from_str(lock).map_err(|error| format!("parse Cargo.lock: {error}"))?;
    let packages = document
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("Cargo.lock has no package array")?;
    let mut graph = BTreeMap::<String, BTreeSet<String>>::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or("Cargo.lock package has no name")?;
        let edges = graph.entry(name.to_owned()).or_default();
        if let Some(dependencies) = package.get("dependencies") {
            let dependencies = dependencies
                .as_array()
                .ok_or("Cargo.lock dependencies must be an array")?;
            for dependency in dependencies {
                let name = dependency
                    .as_str()
                    .and_then(|dependency| dependency.split_whitespace().next())
                    .ok_or("Cargo.lock dependency must name a package")?;
                edges.insert(name.to_owned());
            }
        }
    }
    if !graph.contains_key("arandu_runtime") {
        return Err("Cargo.lock has no arandu_runtime package".into());
    }
    let mut pending = BTreeSet::from(["arandu_runtime".to_owned()]);
    let mut visited = BTreeSet::new();
    let mut violations = Vec::new();
    while let Some(name) = pending.pop_first() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if (name.starts_with("arandu_")
            && !matches!(name.as_str(), "arandu_runtime" | "arandu_testing_contract"))
            || matches!(name.as_str(), "salsa" | "inventory")
        {
            violations.push(format!(
                "Cargo.lock: arandu_runtime transitively depends on compiler package {name}; runtime contracts must remain independent of compiler IR and Salsa"
            ));
        }
        let dependencies = graph
            .get(&name)
            .ok_or_else(|| format!("Cargo.lock dependency has no package entry: {name}"))?;
        pending.extend(dependencies.iter().cloned());
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

    fn graph(edges: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        edges
            .iter()
            .map(|(krate, deps)| {
                (
                    (*krate).to_owned(),
                    deps.iter().map(|dep| (*dep).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn debt_graph() -> Vec<(&'static str, &'static [&'static str])> {
        BACKEND_DEPENDENCY_DEBT
            .iter()
            .map(|&(krate, _)| (krate, &["arandu_middle", "arandu_semantics"][..]))
            .collect()
    }

    #[test]
    fn normal_dependencies_cover_targets_and_renames_but_not_dev() {
        let manifest = r#"
[dependencies]
arandu_middle = { workspace = true }
front = { package = "arandu_typeck", path = "../arandu_typeck" }
[target.'cfg(unix)'.dependencies]
arandu_parser = "0.1"
[dev-dependencies]
arandu_cli = { path = "../arandu_cli" }
"#;
        let deps = normal_dependencies(manifest).unwrap();
        assert_eq!(
            deps.into_iter().collect::<Vec<_>>(),
            ["arandu_middle", "arandu_parser", "arandu_typeck"]
        );
    }

    #[test]
    fn backend_boundary_accepts_contracts_and_recorded_debt() {
        assert!(dependency_direction_violations(&graph(&debt_graph())).is_empty());
    }

    #[test]
    fn backend_boundary_rejects_new_front_end_edges() {
        let mut edges = debt_graph();
        edges.push(("arandu_backend_c", &["arandu_semantics", "arandu_typeck"]));
        edges.push(("arandu_typeck", &["arandu_query"]));
        let violations = dependency_direction_violations(&graph(&edges));
        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("arandu_backend_c depends on arandu_typeck"));
        assert!(violations[1].contains("arandu_typeck depends on downstream crate arandu_query"));
    }

    #[test]
    fn backend_boundary_reports_paid_debt() {
        let mut edges = debt_graph();
        edges[0].1 = &["arandu_middle"];
        let violations = dependency_direction_violations(&graph(&edges));
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("remove the edge from BACKEND_DEPENDENCY_DEBT"));
    }

    #[test]
    fn runtime_contract_accepts_leaf_dependencies() {
        let lock = r#"
[[package]]
name = "arandu_runtime"
dependencies = ["arandu_testing_contract"]
[[package]]
name = "arandu_testing_contract"
dependencies = ["serde 1.0.0"]
[[package]]
name = "serde"
"#;
        assert!(runtime_dependency_violations(lock).unwrap().is_empty());
    }

    #[test]
    fn runtime_contract_rejects_indirect_compiler_dependencies() {
        let lock = r#"
[[package]]
name = "arandu_runtime"
dependencies = ["bridge"]
[[package]]
name = "bridge"
version = "1.0.0"
dependencies = ["arandu_middle"]
[[package]]
name = "bridge"
version = "2.0.0"
dependencies = ["salsa 1.0.0"]
[[package]]
name = "arandu_middle"
[[package]]
name = "salsa"
dependencies = ["inventory"]
[[package]]
name = "inventory"
"#;
        let violations = runtime_dependency_violations(lock).unwrap();
        assert_eq!(violations.len(), 3);
        assert!(violations[0].contains("arandu_middle"));
        assert!(violations[1].contains("salsa"));
        assert!(violations[2].contains("inventory"));
    }

    #[test]
    fn runtime_contract_fails_closed_on_invalid_lock() {
        for lock in [
            "invalid = [",
            "",
            "[[package]]\nname = 'other'",
            "[[package]]\nname = 'arandu_runtime'\ndependencies = ['missing']",
        ] {
            assert!(runtime_dependency_violations(lock).is_err());
        }
    }
}
