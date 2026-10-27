#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A body edit that changes length moves every later declaration of its file.
//! Importers observe declarations by surface, so the edit must not re-lower
//! them, and the incremental result must still equal a cold build.

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

const MAIN: &str = "import app.zlib as lib\nimport app.user as user\n\nfunc main(): int {\n    return lib.after(1) + user.combined(2)\n}\n";
const USER: &str = "import app.zlib as lib\n\npublic func combined(value: int): int {\n    return lib.grow(value) + lib.after(value)\n}\n";

fn library(growth: usize) -> String {
    let mut body = String::from("    let mut acc = value\n");
    for step in 0..growth {
        body.push_str(&format!("    acc = acc + {step}\n"));
    }
    format!(
        "public func grow(value: int): int {{\n{body}    return acc\n}}\n\npublic func after(value: int): int {{\n    return value + 1\n}}\n"
    )
}

struct Package {
    db: DatabaseImpl,
    log: Arc<RebuildLog>,
    files: BTreeMap<String, SourceFile>,
    root: PathBuf,
}

impl Package {
    fn new(label: &str, library_source: &str) -> Self {
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
        for (name, text) in [
            ("main.aru", MAIN),
            ("user.aru", USER),
            ("zlib.aru", library_source),
        ] {
            fs::write(src.join(name), text).unwrap();
        }
        let (mut db, log) = DatabaseImpl::with_rebuild_log();
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
        Self {
            db,
            log,
            files,
            root,
        }
    }

    /// Content hash of the whole lowering result: AMIR, the composed type
    /// context with its symbol positions, and docs.
    fn lowered(&self) -> blake3::Hash {
        lower_amir(&self.db, self.files["main.aru"])
            .value
            .stable_hash()
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn growing_a_body_relowers_only_its_own_module_and_matches_a_cold_build() {
    let mut package = Package::new("incremental", &library(0));
    let _ = package.lowered();
    package.log.clear();

    let lib = package.files["zlib.aru"];
    lib.set_text(&mut package.db).to(Arc::from(library(3)));
    let incremental = package.lowered();

    // `grow` changed and `after` moved; `combined` and `main` only import them.
    assert_eq!(
        package.log.count_executions_matching("instance_hir"),
        2,
        "{}",
        package.log.format_chain(false)
    );

    let cold = Package::new("cold", &library(3));
    assert_eq!(incremental, cold.lowered());
}
