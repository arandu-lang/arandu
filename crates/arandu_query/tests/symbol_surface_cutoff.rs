#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::manifest::ProjectManifest;
use arandu_query::runtime::symbol_surface;
use arandu_query::vfs::{DirectoryListing, ModuleRoots};
use arandu_query::{scan_aru_entries, DatabaseImpl, SourceFile, StableHash};
use salsa::Setter;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestPackage {
    db: DatabaseImpl,
    files: BTreeMap<String, SourceFile>,
    root: PathBuf,
}

impl TestPackage {
    fn new(initial_code: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "arandu-symsurface-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.aru"), initial_code).unwrap();

        let (mut db, _log) = DatabaseImpl::with_rebuild_log();
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
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn symbol_surface_preserves_surface_on_body_changes_and_isolates_siblings() {
    let code_v1 = r#"
public func foo(): int {
    return 1
}

public func bar(): int {
    return 2
}
"#;
    let mut pkg = TestPackage::new(code_v1);
    let main_file = pkg.files["main.aru"];

    let sigs = arandu_query::passes::declaration_signatures(&pkg.db, main_file);
    let foo_sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "foo")
        .expect("foo symbol")
        .id;
    let bar_sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "bar")
        .expect("bar symbol")
        .id;

    let foo_surf_v1 = symbol_surface(&pkg.db, main_file, foo_sym).stable_hash();
    let bar_surf_v1 = symbol_surface(&pkg.db, main_file, bar_sym).stable_hash();

    // 1. Change only foo's body
    let code_v2 = r#"
public func foo(): int {
    let mut acc = 10
    acc = acc + 20
    return acc
}

public func bar(): int {
    return 2
}
"#;
    main_file.set_text(&mut pkg.db).to(Arc::from(code_v2));

    let foo_surf_v2 = symbol_surface(&pkg.db, main_file, foo_sym).stable_hash();
    let bar_surf_v2 = symbol_surface(&pkg.db, main_file, bar_sym).stable_hash();

    assert_eq!(
        foo_surf_v1, foo_surf_v2,
        "foo's surface must remain equal when only its body changed"
    );
    assert_eq!(
        bar_surf_v1, bar_surf_v2,
        "bar's surface must remain equal when sibling's body changed"
    );

    // 2. Change foo's signature (return type to bool)
    let code_v3 = r#"
public func foo(): bool {
    return true
}

public func bar(): int {
    return 2
}
"#;
    main_file.set_text(&mut pkg.db).to(Arc::from(code_v3));

    let foo_surf_v3 = symbol_surface(&pkg.db, main_file, foo_sym).stable_hash();
    let bar_surf_v3 = symbol_surface(&pkg.db, main_file, bar_sym).stable_hash();

    assert_ne!(
        foo_surf_v1, foo_surf_v3,
        "foo's surface must change when its signature changed"
    );
    assert_eq!(
        bar_surf_v1, bar_surf_v3,
        "bar's surface must NOT change when foo's signature changed"
    );
}

#[test]
fn symbol_surface_distinguishes_nominal_struct_and_enum_mutations() {
    let code_v1 = r#"
public struct Point {
    x: int,
    y: int,
}

public enum Color {
    Red,
    Green,
    Blue,
}

public func origin(): int {
    return 0
}
"#;
    let mut pkg = TestPackage::new(code_v1);
    let main_file = pkg.files["main.aru"];

    let sigs = arandu_query::passes::declaration_signatures(&pkg.db, main_file);
    let point_sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "Point")
        .expect("Point")
        .id;
    let color_sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "Color")
        .expect("Color")
        .id;
    let origin_sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "origin")
        .expect("origin")
        .id;

    let point_surf_v1 = symbol_surface(&pkg.db, main_file, point_sym).stable_hash();
    let color_surf_v1 = symbol_surface(&pkg.db, main_file, color_sym).stable_hash();
    let origin_surf_v1 = symbol_surface(&pkg.db, main_file, origin_sym).stable_hash();

    // Mutate struct Point field
    let code_v2 = r#"
public struct Point {
    x: int,
    y: float,
}

public enum Color {
    Red,
    Green,
    Blue,
}

public func origin(): int {
    return 0
}
"#;
    main_file.set_text(&mut pkg.db).to(Arc::from(code_v2));

    let point_surf_v2 = symbol_surface(&pkg.db, main_file, point_sym).stable_hash();
    let color_surf_v2 = symbol_surface(&pkg.db, main_file, color_sym).stable_hash();
    let origin_surf_v2 = symbol_surface(&pkg.db, main_file, origin_sym).stable_hash();

    assert_ne!(
        point_surf_v1, point_surf_v2,
        "Point surface must change when a field type changes"
    );
    assert_eq!(
        color_surf_v1, color_surf_v2,
        "Color surface must NOT change when Point changed"
    );
    assert_eq!(
        origin_surf_v1, origin_surf_v2,
        "origin func surface must NOT change when Point changed"
    );
}

#[test]
fn symbol_surface_covers_enum_variants_and_parameter_modes() {
    let v1 = "public enum Color {\n    Red,\n    Green,\n}\n\npublic func take(value: ref int): int {\n    return value\n}\n";
    let mut pkg = TestPackage::new(v1);
    let file = pkg.files["main.aru"];
    let sigs = arandu_query::passes::declaration_signatures(&pkg.db, file);
    let color = sigs.symbols.iter().find(|s| s.name == "Color").unwrap().id;
    let take = sigs.symbols.iter().find(|s| s.name == "take").unwrap().id;
    let color_v1 = symbol_surface(&pkg.db, file, color).stable_hash();
    let take_v1 = symbol_surface(&pkg.db, file, take).stable_hash();

    let v2 = "public enum Color {\n    Red,\n    Green,\n    Blue,\n}\n\npublic func take(value: int): int {\n    return value\n}\n";
    file.set_text(&mut pkg.db).to(Arc::from(v2));
    assert_ne!(
        color_v1,
        symbol_surface(&pkg.db, file, color).stable_hash(),
        "a new variant changes the enum's surface"
    );
    assert_ne!(
        take_v1,
        symbol_surface(&pkg.db, file, take).stable_hash(),
        "a parameter mode changes the function's surface"
    );
}
