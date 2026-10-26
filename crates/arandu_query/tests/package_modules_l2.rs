//! PROMOTE-L2: package-local multi-file via dual ModuleRoots (same resolve_module_path).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use arandu_middle::db::SourceDatabase;
use arandu_query::{
    parse_manifest_str, scan_aru_entries, validate_package_name, DatabaseImpl, DirectoryListing,
    ManifestError, ModuleRoots, ReservedNameError,
};
use salsa::Setter;

fn temp_pkg(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arandu_l2_{name}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

fn install_roots(db: &mut DatabaseImpl, package_name: &str, src: PathBuf) -> ModuleRoots {
    let entries = scan_aru_entries(&src);
    for relative in &entries {
        let path = src.join(relative);
        db.new_file(
            path.to_string_lossy().into_owned(),
            fs::read_to_string(&path).unwrap(),
        );
    }
    let listing = DirectoryListing::new(db, Arc::new(src.clone()), Arc::new(entries));
    let roots = ModuleRoots::new(
        db,
        package_name.to_string(),
        Arc::new(src),
        None, // no stdlib for pure package tests
        listing,
    );
    db.set_module_roots(roots);
    roots
}

#[test]
fn reserved_package_name_rejected_at_manifest() {
    let err = parse_manifest_str(
        std::path::Path::new("Arandu.toml"),
        r#"
name = "std"
version = "0.0.1"
entry = "src/main.aru"
"#,
    )
    .unwrap_err();
    assert!(
        matches!(err, ManifestError::ReservedName { .. }),
        "expected ReservedName, got {err}"
    );
    assert!(validate_package_name("std").is_err());
    assert!(matches!(
        validate_package_name("io"),
        Err(ReservedNameError::Reserved { .. })
    ));
    assert!(validate_package_name("my_app").is_ok());
}

#[test]
fn package_import_my_app_util_resolves() {
    let root = temp_pkg("util");
    let src = root.join("src");
    fs::write(
        src.join("util.aru"),
        r#"
public func answer(): int {
    return 42
}
"#,
    )
    .unwrap();
    fs::write(
        src.join("main.aru"),
        r#"
module my_app
import my_app.util as util
func main(): int {
    return util.answer()
}
"#,
    )
    .unwrap();

    let mut db = DatabaseImpl::new();
    install_roots(&mut db, "my_app", src.clone());

    // Register entry under a dummy key; imports use my_app/util.aru.
    let main_text = fs::read_to_string(src.join("main.aru")).unwrap();
    let main = db.new_file("src/main.aru".into(), main_text);

    let util_key = "my_app/util.aru";
    let resolved = db.resolve_module_path(util_key);
    assert!(
        resolved.is_some(),
        "expected {util_key} via ModuleRoots listing"
    );

    let tc = arandu_query::passes::type_check(&db, main);
    assert!(
        tc.diagnostics.is_empty(),
        "package import typeck failed: {:?}",
        tc.diagnostics
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_listing_miss_until_entries_updated() {
    let root = temp_pkg("listing");
    let src = root.join("src");
    fs::write(
        src.join("main.aru"),
        "module my_app\nfunc main(): int { return 0 }\n",
    )
    .unwrap();
    // util.aru exists on disk but is NOT in the initial listing.

    let mut db = DatabaseImpl::new();
    let listing = DirectoryListing::new(
        &db,
        Arc::new(src.clone()),
        Arc::new(vec!["main.aru".into()]), // util missing on purpose
    );
    let roots = ModuleRoots::new(&db, "my_app".into(), Arc::new(src.clone()), None, listing);
    db.set_module_roots(roots);

    assert!(
        db.resolve_module_path("my_app/util.aru").is_none(),
        "must not see util before listing update (no bare fs::exists)"
    );

    // User creates util.aru and watcher / CLI refreshes listing.
    fs::write(
        src.join("util.aru"),
        "public func answer(): int { return 7 }\n",
    )
    .unwrap();
    let util_path = src.join("util.aru");
    db.new_file(
        util_path.to_string_lossy().into_owned(),
        fs::read_to_string(&util_path).unwrap(),
    );
    listing
        .set_entries(&mut db)
        .to(Arc::new(vec!["main.aru".into(), "util.aru".into()]));

    assert!(
        db.resolve_module_path("my_app/util.aru").is_some(),
        "after listing update, package module must resolve"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn creating_missing_module_invalidates_composed_typecheck() {
    let root = temp_pkg("create_typecheck");
    let src = root.join("src");
    fs::write(
        src.join("main.aru"),
        "module my_app\nimport my_app.util as util\nfunc main(): int { return util.answer() }\n",
    )
    .unwrap();

    let mut db = DatabaseImpl::new();
    let roots = install_roots(&mut db, "my_app", src.clone());
    let main = db.new_file(
        "my_app/main.aru".into(),
        fs::read_to_string(src.join("main.aru")).unwrap(),
    );
    assert!(arandu_query::passes::type_check(&db, main)
        .diagnostics
        .iter()
        .any(|diag| matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)));

    fs::write(
        src.join("util.aru"),
        "public func answer(): int { return 42 }\n",
    )
    .unwrap();
    let util_path = src.join("util.aru");
    db.new_file(
        util_path.to_string_lossy().into_owned(),
        fs::read_to_string(&util_path).unwrap(),
    );
    let listing = roots.package_listing(&db);
    listing
        .set_entries(&mut db)
        .to(Arc::new(scan_aru_entries(&src)));

    let diagnostics = &arandu_query::passes::type_check(&db, main).diagnostics;
    assert!(
        diagnostics.is_empty(),
        "structural listing input must invalidate composed typecheck: {diagnostics:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn removing_one_registry_alias_keeps_reverse_file_identity() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "C:/workspace/util.aru".into(),
        "public func answer(): int { return 42 }\n".into(),
    );
    let file_id = *file.file_id(&db);
    db.register_source_file("my_app/util.aru".into(), file);
    db.unregister_source_file("my_app/util.aru");

    assert!(db.source_file_by_id(file_id).is_some());
    assert!(db.source_file_by_path("C:/workspace/util.aru").is_some());
}

#[test]
fn package_local_circular_import_reuses_cycle_recover() {
    let root = temp_pkg("cycle");
    let src = root.join("src");
    fs::write(
        src.join("a.aru"),
        r#"
import my_app.b as b
public func foo(): int {
    return b.bar()
}
"#,
    )
    .unwrap();
    fs::write(
        src.join("b.aru"),
        r#"
import my_app.a as a
public func bar(): int {
    return a.foo()
}
"#,
    )
    .unwrap();

    let mut db = DatabaseImpl::new();
    install_roots(&mut db, "my_app", src.clone());

    let a_text = fs::read_to_string(src.join("a.aru")).unwrap();
    let a = db.new_file("src/a.aru".into(), a_text);

    let tc = arandu_query::passes::type_check(&db, a);
    let has_cycle = tc
        .diagnostics
        .iter()
        .any(|d| d.message.contains("cyclic") || d.message.contains("cycle"));
    assert!(
        has_cycle,
        "expected cycle diagnostic from module_signatures cycle_recover, got: {:?}",
        tc.diagnostics
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn n006_alias_conflict_local_and_stdlib() {
    // Same alias `path` for stdlib + package-local → N006ImportConflict.
    let mut db = DatabaseImpl::new();
    let _local = db.new_file(
        "my_app/path.aru".into(),
        r#"
public func is_empty(s: str): bool {
    return true
}
"#
        .into(),
    );
    db.new_stdlib_file(
        "stdlib/std/path.aru".into(),
        "public func is_empty(s: str): bool { return true }\n".into(),
    );

    // ModuleAlias form: `import X as path` twice.
    let src = r#"
module tests.n006
import std.path as path
import my_app.path as path
func main(): int {
    return 0
}
"#;
    let file = db.new_file("tests_n006.aru".into(), src.into());
    let resolved = arandu_query::passes::resolve(&db, file);
    let has_n006 = resolved
        .diagnostics
        .iter()
        .any(|d| matches!(d.code, arandu_middle::DiagCode::N006ImportConflict));
    assert!(
        has_n006,
        "ModuleAlias collision must emit N006, got: {:?}",
        resolved.diagnostics
    );

    // Named form: `from … import { is_empty }` twice.
    let src2 = r#"
module tests.n006_named
from std.path import { is_empty }
from my_app.path import { is_empty }
func main(): int { return 0 }
"#;
    let file2 = db.new_file("tests_n006_named.aru".into(), src2.into());
    let r2 = arandu_query::passes::resolve(&db, file2);
    let n006 = r2
        .diagnostics
        .iter()
        .any(|d| matches!(d.code, arandu_middle::DiagCode::N006ImportConflict));
    assert!(
        n006,
        "Named import collision must emit N006, got: {:?}",
        r2.diagnostics
    );
}

#[test]
fn bare_util_aru_resolves_under_package_src() {
    let root = temp_pkg("bare");
    let src = root.join("src");
    fs::write(src.join("util.aru"), "public func n(): int { return 3 }\n").unwrap();
    fs::write(
        src.join("main.aru"),
        r#"
import util
func main(): int {
    return util.n()
}
"#,
    )
    .unwrap();

    let mut db = DatabaseImpl::new();
    install_roots(&mut db, "my_app", src.clone());
    let main = db.new_file(
        "src/main.aru".into(),
        fs::read_to_string(src.join("main.aru")).unwrap(),
    );
    assert!(db.resolve_module_path("util.aru").is_some());
    let tc = arandu_query::passes::type_check(&db, main);
    assert!(
        tc.diagnostics.is_empty(),
        "bare package import failed: {:?}",
        tc.diagnostics
    );
    let _ = fs::remove_dir_all(&root);
}
