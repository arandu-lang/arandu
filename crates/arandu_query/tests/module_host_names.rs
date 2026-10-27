#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Module identity of package sources: undeclared modules still qualify their
//! function host names (SYM.1), a declaration must match the file's module
//! (N020), and coinciding qualified names are rejected before codegen (N021).

use arandu_middle::{DiagCode, Diagnostic, Severity};
use arandu_query::manifest::ProjectManifest;
use arandu_query::passes::{local_symbols, lower_amir};
use arandu_query::vfs::{DirectoryListing, ModuleRoots};
use arandu_query::{scan_aru_entries, DatabaseImpl, SourceFile};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

struct Package {
    db: DatabaseImpl,
    files: BTreeMap<String, SourceFile>,
    root: PathBuf,
}

impl Package {
    /// Registers `sources` as package `app` with entry `src/main.aru`, the
    /// way the CLI loads a project.
    fn new(label: &str, sources: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "arandu-modules-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        for (name, text) in sources {
            fs::write(src.join(name), text).unwrap();
        }
        let mut db = DatabaseImpl::new();
        let entries = scan_aru_entries(&src);
        let mut files = BTreeMap::new();
        for relative in &entries {
            let path = src.join(relative);
            let file = db.new_file(
                path.to_string_lossy().into_owned(),
                fs::read_to_string(&path).unwrap(),
            );
            files.insert(relative.clone(), file);
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
        Self { db, files, root }
    }

    fn file(&self, name: &str) -> SourceFile {
        self.files[name]
    }

    fn lowering_diagnostics(&self) -> Vec<Diagnostic> {
        let main = self.file("main.aru");
        let _ = lower_amir(&self.db, main);
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&self.db, main)
            .into_iter()
            .map(|diagnostic| diagnostic.0.clone())
            .collect()
    }

    fn mismatches(&self, name: &str) -> Vec<Diagnostic> {
        local_symbols(&self.db, self.file(name))
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagCode::N020ModuleDeclarationMismatch)
            .cloned()
            .collect()
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn equal_function_names_in_undeclared_modules_get_distinct_host_names() {
    let package = Package::new(
        "names",
        &[
            (
                "main.aru",
                "import app.left as left\nimport app.right as right\n\nfunc helper(value: int): int {\n    return value + 100\n}\n\nfunc main(): int {\n    return left.api(1) + right.api(2) + helper(0)\n}\n",
            ),
            (
                "left.aru",
                "func helper(value: int): int {\n    return value + 10\n}\n\npublic func api(value: int): int {\n    return helper(value)\n}\n",
            ),
            (
                "right.aru",
                "func helper(value: int, scale: int): int {\n    return value * scale\n}\n\npublic func api(value: int): int {\n    return helper(value, 3)\n}\n",
            ),
        ],
    );
    let diagnostics = package.lowering_diagnostics();
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "{diagnostics:?}"
    );

    let main = package.file("main.aru");
    let artifacts = lower_amir(&package.db, main);
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
}

#[test]
fn module_declarations_must_match_their_file_and_offer_the_fix() {
    let package = Package::new(
        "declarations",
        &[
            (
                "main.aru",
                "module wrong\nimport app.left as left\nimport app.outro as outro\n\nfunc main(): int {\n    return left.api() + outro.api()\n}\n",
            ),
            (
                "left.aru",
                "module app.left\n\npublic func api(): int {\n    return 1\n}\n",
            ),
            (
                "outro.aru",
                "module app.left\n\npublic func api(): int {\n    return 2\n}\n",
            ),
        ],
    );
    assert!(package.mismatches("left.aru").is_empty());

    let outro = package.mismatches("outro.aru");
    assert_eq!(outro.len(), 1, "{outro:?}");
    assert!(outro[0].message.contains("`app.left`") && outro[0].message.contains("`app.outro`"));
    let replacement = outro[0]
        .hints
        .iter()
        .find_map(|hint| hint.replacement.as_ref())
        .expect("structured fix");
    assert_eq!(replacement.new_text, "module app.outro");

    // The entry is the package itself.
    let entry = package.mismatches("main.aru");
    assert_eq!(entry.len(), 1, "{entry:?}");
    assert!(entry[0].message.contains("`app`"));
}

#[test]
fn coinciding_qualified_names_are_rejected_before_codegen() {
    // `Box.read` in the entry `module app` and `read` in `src/Box.aru` are
    // both `app.Box.read`.
    let package = Package::new(
        "collision",
        &[
            (
                "main.aru",
                "module app\nimport app.Box as boxes\n\nstruct Box {\n    value: int\n}\n\nfunc Box.read(shared self): int {\n    return self.value\n}\n\nfunc main(): int {\n    let item = Box { value: 1 }\n    return item.read() + boxes.read()\n}\n",
            ),
            ("Box.aru", "public func read(): int {\n    return 2\n}\n"),
        ],
    );
    let diagnostics = package.lowering_diagnostics();
    let collisions = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagCode::N021DuplicateFunctionSymbol)
        .collect::<Vec<_>>();
    assert_eq!(collisions.len(), 1, "{diagnostics:?}");
    assert!(collisions[0].message.contains("`app.Box.read`"));
    assert!(
        !diagnostics.iter().any(Diagnostic::is_ice),
        "{diagnostics:?}"
    );
}
