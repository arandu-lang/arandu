#![allow(clippy::unwrap_used, clippy::expect_used)]
//! What importers observe of a file is its declarations: a body edit that
//! allocates new local symbols leaves the declaration surface equal, so no
//! importer is lowered again, while a signature change still reaches them.

use arandu_query::manifest::ProjectManifest;
use arandu_query::passes::lower_amir;
use arandu_query::vfs::{DirectoryListing, ModuleRoots};
use arandu_query::{scan_aru_entries, DatabaseImpl, RebuildLog, SourceFile, StableHash};
use salsa::Setter;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const MAIN: &str = "import app.lib as lib\n\nfunc twice(value: int): int {\n    return lib.api(value) * 2\n}\n\nfunc main(): int {\n    return twice(1)\n}\n";

/// `helper` gains `locals` new `let` bindings; `other` is public when
/// `exported`.
fn library(locals: usize, exported: bool) -> String {
    let mut helper = String::from("func helper(value: int): int {\n    let mut acc = value\n");
    for local in 1..=locals {
        helper.push_str(&format!(
            "    let extra_{local} = acc + {local}\n    acc = extra_{local}\n"
        ));
    }
    helper.push_str("    return acc\n}\n");
    let visibility = if exported { "public " } else { "" };
    format!(
        "{helper}\n{visibility}func other(value: int): int {{\n    let doubled = value * 2\n    return doubled\n}}\n\npublic func api(value: int): int {{\n    return helper(value) + other(1)\n}}\n"
    )
}

struct Package {
    db: DatabaseImpl,
    files: BTreeMap<String, SourceFile>,
    root: PathBuf,
}

impl Package {
    fn new(label: &str, db: DatabaseImpl, library: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "arandu-surface-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.aru"), MAIN).unwrap();
        fs::write(src.join("lib.aru"), library).unwrap();
        let mut db = db;
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

    fn main(&self) -> SourceFile {
        self.files["main.aru"]
    }

    fn set_library(&mut self, text: String) {
        let file = self.files["lib.aru"];
        file.set_text(&mut self.db).to(Arc::from(text));
    }

    /// The lowered program, with its diagnostics.
    fn lower(&self) -> (blake3::Hash, Vec<String>) {
        let main = self.main();
        let hash = lower_amir(&self.db, main).value.stable_hash();
        let diagnostics =
            lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&self.db, main)
                .into_iter()
                .map(|diagnostic| diagnostic.0.message.clone())
                .collect();
        (hash, diagnostics)
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn runs(log: &RebuildLog, query: &str) -> usize {
    log.count_executions_matching(&format!("{query}("))
}

#[test]
fn a_new_local_in_a_private_body_relowers_no_importer() {
    let (db, log) = DatabaseImpl::with_rebuild_log();
    let mut package = Package::new("local", db, &library(0, false));
    let (_, diagnostics) = package.lower();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    log.clear();

    package.set_library(library(2, false));
    let (incremental, diagnostics) = package.lower();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // The units of the importing file keep theirs, and its declaration
    // context is not linked again. The library's three units are lowered
    // again because their item contexts read the file's symbol table.
    assert_eq!(runs(&log, "runtime_unit"), 3, "{}", log.format_chain(false));
    assert_eq!(runs(&log, "local_declaration_context"), 0);
    assert_eq!(runs(&log, "linked_imports"), 1);

    let cold = Package::new("local-cold", DatabaseImpl::new(), &library(2, false));
    assert_eq!(
        incremental,
        cold.lower().0,
        "warm lowering equals a cold one"
    );
}

/// The control: a declaration change still reaches the importer.
#[test]
fn a_signature_change_still_relowers_the_importer() {
    let (db, log) = DatabaseImpl::with_rebuild_log();
    let mut package = Package::new("signature", db, &library(0, false));
    let _ = package.lower();
    log.clear();

    package.set_library(library(0, true));
    let (incremental, _) = package.lower();
    assert!(
        runs(&log, "runtime_unit") >= 3,
        "{}",
        log.format_chain(false)
    );
    let cold = Package::new("signature-cold", DatabaseImpl::new(), &library(0, true));
    assert_eq!(incremental, cold.lower().0);
}

/// Repeated local edits keep the warm result equal to a cold build: what the
/// surface leaves out is never read stale.
#[test]
fn successive_local_edits_lower_to_the_bytes_of_a_cold_build() {
    let mut package = Package::new("successive", DatabaseImpl::new(), &library(0, false));
    let _ = package.lower();
    for locals in [3, 1, 4, 0] {
        package.set_library(library(locals, false));
        let (incremental, diagnostics) = package.lower();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let cold = Package::new(
            "successive-cold",
            DatabaseImpl::new(),
            &library(locals, false),
        );
        assert_eq!(incremental, cold.lower().0, "{locals} locals");
    }
}
