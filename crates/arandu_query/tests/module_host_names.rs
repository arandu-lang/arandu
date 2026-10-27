#![allow(clippy::unwrap_used, clippy::expect_used)]
//! SYM.1: package sources without a `module` declaration still qualify their
//! function host names, so equal names in different modules stay distinct
//! linker symbols.

use arandu_middle::Severity;
use arandu_query::manifest::ProjectManifest;
use arandu_query::passes::lower_amir;
use arandu_query::vfs::{DirectoryListing, ModuleRoots};
use arandu_query::{scan_aru_entries, DatabaseImpl};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_pkg(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arandu-sym1-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

fn write_pkg_file(root: &Path, name: &str, contents: &str) {
    fs::write(root.join("src").join(name), contents).unwrap();
}

#[test]
fn equal_function_names_in_undeclared_modules_get_distinct_host_names() {
    let root = temp_pkg("names");
    write_pkg_file(
        &root,
        "main.aru",
        "import app.left as left\nimport app.right as right\n\nfunc helper(value: int): int {\n    return value + 100\n}\n\nfunc main(): int {\n    return left.api(1) + right.api(2) + helper(0)\n}\n",
    );
    write_pkg_file(
        &root,
        "left.aru",
        "func helper(value: int): int {\n    return value + 10\n}\n\npublic func api(value: int): int {\n    return helper(value)\n}\n",
    );
    write_pkg_file(
        &root,
        "right.aru",
        "func helper(value: int, scale: int): int {\n    return value * scale\n}\n\npublic func api(value: int): int {\n    return helper(value, 3)\n}\n",
    );

    let mut db = DatabaseImpl::new();
    let src = root.join("src");
    let entries = scan_aru_entries(&src);
    let mut main = None;
    for relative in &entries {
        let path = src.join(relative);
        let file = db.new_file(
            path.to_string_lossy().into_owned(),
            fs::read_to_string(&path).unwrap(),
        );
        if relative == "main.aru" {
            main = Some(file);
        }
    }
    let listing = DirectoryListing::new(&db, Arc::new(src.clone()), Arc::new(entries));
    let roots = ModuleRoots::new(&db, "app".to_string(), Arc::new(src), None, listing);
    db.set_module_roots(roots);
    let manifest = ProjectManifest::new(
        &db,
        "app".to_string(),
        "0.0.1".to_string(),
        "src/main.aru".to_string(),
        String::new(),
        Arc::new(root.join("Arandu.toml")),
    );
    db.set_project_manifest(manifest);

    let main = main.expect("entry file");
    let artifacts = lower_amir(&db, main);
    let diagnostics =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, main);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.severity == Severity::Error),
        "{:?}",
        diagnostics
            .iter()
            .map(|diagnostic| &diagnostic.0)
            .collect::<Vec<_>>()
    );

    let symbols = &artifacts.type_check.symbols;
    let mut helpers = artifacts
        .amir
        .funcs
        .iter()
        .map(|function| symbols.get(function.symbol))
        .filter(|symbol| symbol.name == "helper")
        .map(|symbol| symbols.host_func_name(symbol).to_owned())
        .collect::<Vec<_>>();
    helpers.sort();
    // The entry keeps unqualified names, independent of the package name.
    assert_eq!(helpers, ["app.left.helper", "app.right.helper", "helper"]);

    let entry = artifacts
        .amir
        .funcs
        .iter()
        .map(|function| symbols.get(function.symbol))
        .find(|symbol| symbol.name == "main")
        .expect("entry function");
    assert_eq!(symbols.host_func_name(entry), "main");

    let _ = fs::remove_dir_all(&root);
}
