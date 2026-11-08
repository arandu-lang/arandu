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
        Self::with_files(
            label,
            &[
                ("main.aru", MAIN.to_owned()),
                ("top.aru", TOP.to_owned()),
                ("mid.aru", MID.to_owned()),
                ("base.aru", base_source.to_owned()),
            ],
        )
    }

    fn with_files(label: &str, sources: &[(&str, String)]) -> Self {
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
        for (name, text) in sources {
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
        assert!(lowered.value.amir.funcs.len() >= 4, "the program lowers");
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
    // `mid`, whose surface names `Point` but not `unused`.
    let chain = package.log.format_chain(false);
    assert_eq!(
        package.log.count_executions_matching("signature_context"),
        2,
        "{chain}"
    );
    // Each item's typing context and headers hold only what it reaches
    // (INC.3a/b), so nothing of `mid`, `top` or `main` lowers again. The one
    // unit of `base` does: the new parameter is a symbol of its file (see
    // `an_arity_change_reexecutes_its_file_and_its_callers_only`).
    assert_eq!(
        package.log.count_executions_matching("function_hir"),
        1,
        "{chain}"
    );
    assert_eq!(
        package.log.count_executions_matching("instance_hir"),
        1,
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

const GATE_MAIN: &str = "import app.top as top\n\nfunc main(): int {\n    return top.run(1)\n}\n";
const GATE_TOP: &str =
    "import app.mid as mid\n\npublic func run(value: int): int {\n    return mid.api(value)\n}\n";

/// How the gate edits a signature: its arity, which declares a parameter
/// symbol, or a parameter type, which declares none.
#[derive(Clone, Copy, PartialEq)]
enum Edit {
    None,
    Arity,
    Type,
}

fn gate_lib(direct: Edit, api: Edit) -> String {
    let signature = |edit| match edit {
        Edit::None => "value: int",
        Edit::Arity => "value: int, extra: int",
        Edit::Type => "value: bool",
    };
    let body = |edit| if edit == Edit::Type { "1" } else { "value + 1" };
    format!(
        "public struct Point {{ public x: int }}\n\nfunc helper(value: int): int {{\n    return value * 2\n}}\n\npublic func weigh<T>(value: T, factor: int): int {{\n    return factor\n}}\n\npublic func api({}): int {{\n    return {}\n}}\n\npublic func direct({}): int {{\n    return {}\n}}\n",
        signature(api),
        if api == Edit::Type { "helper(1)" } else { "helper(value)" },
        signature(direct),
        body(direct),
    )
}

fn gate_mid(direct: Edit, api: Edit) -> String {
    let argument = |edit| match edit {
        Edit::None => "value",
        Edit::Arity => "value, 0",
        Edit::Type => "value == 0",
    };
    format!(
        "import app.lib as lib\n\nfunc call_direct(value: int): int {{\n    return lib.direct({})\n}}\n\nfunc call_api(value: int): int {{\n    return lib.api({})\n}}\n\nfunc local(value: int): int {{\n    return value + 1\n}}\n\nfunc generic(value: int): int {{\n    return lib.weigh(value, 3)\n}}\n\nfunc point(value: int): int {{\n    return lib.Point {{ x: value }}.x\n}}\n\npublic func api(value: int): int {{\n    return call_direct(value) + call_api(value) + local(value) + generic(value) + point(value)\n}}\n",
        argument(direct),
        argument(api),
    )
}

fn gate_package(label: &str, direct: Edit, api: Edit) -> Package {
    Package::with_files(
        label,
        &[
            ("main.aru", GATE_MAIN.to_owned()),
            ("top.aru", GATE_TOP.to_owned()),
            ("mid.aru", gate_mid(direct, api)),
            ("lib.aru", gate_lib(direct, api)),
        ],
    )
}

fn gate_edit(direct: Edit, api: Edit, label: &str) -> (usize, String) {
    let mut package = gate_package(label, Edit::None, Edit::None);
    let _ = package.lowered();
    package.log.clear();
    for (name, text) in [
        ("lib.aru", gate_lib(direct, api)),
        ("mid.aru", gate_mid(direct, api)),
    ] {
        let file = package.files[name];
        file.set_text(&mut package.db).to(Arc::from(text));
    }
    let incremental = package.lowered();
    let cold = gate_package(&format!("{label}-cold"), direct, api);
    assert_eq!(incremental, cold.lowered(), "{label}: warm equals cold");
    (
        package.log.count_executions_matching("instance_hir"),
        package.log.format_chain(false),
    )
}

/// The 0.3.0 gate (INC.3a/b): a signature change reexecutes exactly the units
/// that reference the changed symbol, its own and its callers'.
#[test]
fn a_signature_change_reexecutes_only_the_units_that_reference_it() {
    let (units, chain) = gate_edit(Edit::Type, Edit::None, "gate-direct");
    assert_eq!(units, 2, "lib.direct and mid.call_direct: {chain}");
    let (units, chain) = gate_edit(Edit::None, Edit::Type, "gate-api");
    assert_eq!(units, 2, "lib.api and mid.call_api: {chain}");
}

/// A new parameter is a new symbol of its file, whose dense symbol table
/// every unit of that file carries for the composition: those units and the
/// callers reexecute, no other importer does.
#[test]
fn an_arity_change_reexecutes_its_file_and_its_callers_only() {
    let (units, chain) = gate_edit(Edit::Arity, Edit::None, "arity-direct");
    // The units of `lib` and `mid.call_direct`; the other units of `mid`
    // and those of `top` and `main` stay.
    assert_eq!(units, 5, "{chain}");
}
