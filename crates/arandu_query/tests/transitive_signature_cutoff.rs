#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A module observes its imports through what they export (INC.3a): a
//! signature change two imports away that the middle module's surface never
//! mentions must not retype or relower the modules above it, while a type
//! that surface does mention still reaches them. Both must equal a cold build.

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

const MAIN: &str = "import app.top as top\n\nfunc main(): int {\n    return top.run(1)\n}\n";
const TOP: &str = "import app.mid as mid\n\nfunc helper(value: int): int {\n    return value + 2\n}\n\npublic func run(value: int): int {\n    return mid.relay(value) + mid.make().x + helper(value)\n}\n";
const MID: &str = "import app.base as base\n\npublic func relay(value: int): int {\n    return base.used(value)\n}\n\npublic func make(): base.Point {\n    return base.Point { x: 3 }\n}\n";

fn base(wide_unused: bool, point_fields: &str) -> String {
    let unused = if wide_unused {
        "value: int, extra: int"
    } else {
        "value: int"
    };
    format!(
        "public struct Point {{ {point_fields} }}\n\npublic func used(value: int): int {{\n    return value + 1\n}}\n\npublic func unused({unused}): int {{\n    return value\n}}\n"
    )
}

struct Package {
    db: DatabaseImpl,
    log: Arc<RebuildLog>,
    files: BTreeMap<String, SourceFile>,
    root: PathBuf,
}

impl Package {
    fn new(label: &str, base_source: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "arandu-transitive-{label}-{}-{}",
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
            ("top.aru", TOP),
            ("mid.aru", MID),
            ("base.aru", base_source),
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

    fn lowered(&self) -> blake3::Hash {
        let lowered = lower_amir(&self.db, self.files["main.aru"]);
        assert!(lowered.value.amir.funcs.len() >= 6, "the program lowers");
        lowered.value.stable_hash()
    }

    fn edit_base(&mut self, source: &str) {
        let file = self.files["base.aru"];
        file.set_text(&mut self.db).to(Arc::from(source));
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_signature_the_middle_surface_never_mentions_stops_at_its_importer() {
    let mut package = Package::new("unused", &base(false, "public x: int"));
    let _ = package.lowered();
    package.log.clear();

    let edited = base(true, "public x: int");
    package.edit_base(&edited);
    let incremental = package.lowered();

    // `base` and its direct importer `mid` retype; `top` and `main` only see
    // `mid`, whose surface names `Point` but not `unused`. `top` still links
    // the surface of `base` for `Point`, so the unit that reads it relowers:
    // the cut is per file, not yet per symbol.
    let chain = package.log.format_chain(false);
    assert_eq!(
        package.log.count_executions_matching("signature_context"),
        2,
        "{chain}"
    );
    // Each item's typing context holds only what it reaches (INC.3a), so of
    // every function only `unused` lowers its HIR again.
    assert_eq!(
        package.log.count_executions_matching("function_hir"),
        1,
        "{chain}"
    );
    // base: used, unused; mid: relay, make; top: run.
    assert_eq!(
        package.log.count_executions_matching("instance_hir"),
        5,
        "{chain}"
    );

    let cold = Package::new("unused-cold", &edited);
    assert_eq!(incremental, cold.lowered());
}

#[test]
fn a_type_the_middle_surface_mentions_still_reaches_the_modules_above() {
    let mut package = Package::new("reached", &base(false, "public x: int"));
    let _ = package.lowered();
    package.log.clear();

    let edited = base(false, "public w: int = 0, public x: int");
    package.edit_base(&edited);
    let incremental = package.lowered();
    assert!(
        package.log.count_executions_matching("signature_context") >= 3,
        "`top` reads `Point` through `mid.make`: {}",
        package.log.format_chain(false)
    );

    let cold = Package::new("reached-cold", &edited);
    assert_eq!(incremental, cold.lowered());
}
