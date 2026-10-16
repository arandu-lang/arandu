//! P4: logical package namespaces are explicit Salsa inputs, not filesystem probes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use arandu_middle::{ModuleId, PackageId, TargetId};
use arandu_query::{
    register_manifest, DatabaseImpl, ManifestData, ModuleBinding, PackageModuleMap,
};
use salsa::Setter;

fn ids() -> (PackageId, TargetId, ModuleId) {
    (
        PackageId::try_from_usize(1).unwrap(),
        TargetId::try_from_usize(1).unwrap(),
        ModuleId::try_from_usize(1).unwrap(),
    )
}

fn enable_package_mode(db: &mut DatabaseImpl) {
    let manifest = register_manifest(
        db,
        "arandu.toml".into(),
        ManifestData::legacy("app".into(), "0.1.0".into(), "src/main.aru".into()),
        "test-manifest".into(),
    );
    db.set_project_manifest(manifest);
}

#[test]
fn map_allows_direct_export_and_rejects_private_and_transitive_namespaces() {
    let mut db = DatabaseImpl::new();
    let geometry = db.new_file(
        "dependency/src/geometry.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let private = db.new_file(
        "dependency/src/internal.aru".into(),
        "public func secret(): int { return 99 }".into(),
    );
    let (package, target, module) = ids();
    let map = PackageModuleMap::new(
        &db,
        PackageId::try_from_usize(0).unwrap(),
        TargetId::try_from_usize(0).unwrap(),
        Arc::new(vec![(
            "math/geometry.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file: geometry,
            },
        )]),
    );
    db.set_package_module_map(map);

    assert!(db.source_file_by_path("dependency/src/internal.aru") == Some(private));
    assert!(
        arandu_middle::db::SourceDatabase::resolve_module_path(&db, "math/geometry.aru")
            == Some(geometry)
    );
    assert!(
        arandu_middle::db::SourceDatabase::resolve_module_path(&db, "math/internal.aru").is_none()
    );
    assert!(
        arandu_middle::db::SourceDatabase::resolve_module_path(&db, "transitive/thing.aru")
            .is_none()
    );
}

#[test]
fn package_map_change_invalidates_import_without_filesystem_discovery() {
    let mut db = DatabaseImpl::new();
    let geometry = db.new_file(
        "dependency/src/geometry.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let importer = db.new_file(
        "app/src/main.aru".into(),
        "import math.geometry as geometry\nfunc main(): int { return geometry.answer() }".into(),
    );
    let (package, target, module) = ids();
    let map = PackageModuleMap::new(
        &db,
        PackageId::try_from_usize(0).unwrap(),
        TargetId::try_from_usize(0).unwrap(),
        Arc::new(vec![(
            "math/geometry.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file: geometry,
            },
        )]),
    );
    db.set_package_module_map(map);
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .is_empty());

    map.set_bindings(&mut db).to(Arc::new(Vec::new()));
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(
            diagnostic.code,
            arandu_middle::DiagCode::M001UnresolvedImport
        )));
}

#[test]
fn internal_exports_are_available_only_within_the_same_package() {
    fn analyze(import_path: &str, library_package: PackageId) -> Vec<arandu_middle::Diagnostic> {
        let mut db = DatabaseImpl::new();
        let importer = db.new_file(
            "app/src/main.aru".into(),
            format!(
                "from {import_path} import {{ answer }}\nfunc main(): int {{ return answer() }}"
            ),
        );
        let library = db.new_file(
            "dep/src/lib.aru".into(),
            "internal func answer(): int { return 42 }".into(),
        );
        let own_package = PackageId::try_from_usize(1).unwrap();
        let target = TargetId::try_from_usize(1).unwrap();
        let main_module = ModuleId::try_from_usize(1).unwrap();
        let library_module = ModuleId::try_from_usize(2).unwrap();
        let map = PackageModuleMap::new(
            &db,
            own_package,
            target,
            Arc::new(vec![
                (
                    "self/lib.aru".into(),
                    ModuleBinding {
                        package: library_package,
                        target,
                        module: library_module,
                        file: library,
                    },
                ),
                (
                    "self/main.aru".into(),
                    ModuleBinding {
                        package: own_package,
                        target,
                        module: main_module,
                        file: importer,
                    },
                ),
            ]),
        );
        db.set_package_module_map(map);
        let public = arandu_query::passes::exported_symbols(&db, library);
        let package_scoped = arandu_query::passes::internal_symbols(&db, library);
        assert!(public.symbols.is_empty());
        assert!(public.internal_symbols.is_empty());
        assert!(package_scoped.symbols.is_empty());
        assert!(package_scoped.internal_symbols.contains_key("answer"));
        arandu_query::passes::type_check(&db, importer)
            .diagnostics
            .clone()
    }

    let own_package = PackageId::try_from_usize(1).unwrap();
    let external_package = PackageId::try_from_usize(2).unwrap();
    assert!(analyze("self.lib", own_package).is_empty());
    let diagnostics = analyze("self.lib", external_package);
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code == arandu_middle::DiagCode::N016InternalOutsidePackage
        }),
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn sealed_interface_implementation_is_rejected_across_packages() {
    let mut db = DatabaseImpl::new();
    let library = db.new_file(
        "dep/src/lib.aru".into(),
        "public sealed interface Expr {}".into(),
    );
    let importer = db.new_file(
        "app/src/main.aru".into(),
        "import math as math\nstruct Number {}\nimpl Number: math.Expr {}".into(),
    );
    let app = PackageId::try_from_usize(1).unwrap();
    let dependency = PackageId::try_from_usize(2).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let map = PackageModuleMap::new(
        &db,
        app,
        target,
        Arc::new(vec![
            (
                "math.aru".into(),
                ModuleBinding {
                    package: dependency,
                    target,
                    module: ModuleId::try_from_usize(2).unwrap(),
                    file: library,
                },
            ),
            (
                "self/main.aru".into(),
                ModuleBinding {
                    package: app,
                    target,
                    module: ModuleId::try_from_usize(1).unwrap(),
                    file: importer,
                },
            ),
        ]),
    );
    db.set_package_module_map(map);

    let result = arandu_query::passes::type_check(&db, importer);
    let diagnostics = &result.diagnostics;
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code == arandu_middle::DiagCode::N017SealedImplOutsidePackage
        }),
        "diagnostics: {diagnostics:?}"
    );
    map.set_bindings(&mut db).to(Arc::new(vec![
        (
            "math.aru".into(),
            ModuleBinding {
                package: app,
                target,
                module: ModuleId::try_from_usize(2).unwrap(),
                file: library,
            },
        ),
        (
            "self/main.aru".into(),
            ModuleBinding {
                package: app,
                target,
                module: ModuleId::try_from_usize(1).unwrap(),
                file: importer,
            },
        ),
    ]));
    let result = arandu_query::passes::type_check(&db, importer);
    assert!(!result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == arandu_middle::DiagCode::N017SealedImplOutsidePackage
    }));
}

#[test]
fn sealed_interface_matches_use_explicit_implementers_for_exhaustiveness() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "app/src/main.aru".into(),
        concat!(
            "sealed interface Expr {}\n",
            "struct Number {}\n",
            "struct Sum {}\n",
            "impl Number: Expr {}\n",
            "impl Sum: Expr {}\n",
            "func eval(e: Expr): int { return match e { Number {} => 1 } }\n",
        )
        .into(),
    );
    let (package, target, module) = ids();
    db.set_package_module_map(PackageModuleMap::new(
        &db,
        package,
        target,
        Arc::new(vec![(
            "self/main.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file,
            },
        )]),
    ));

    let result = arandu_query::passes::type_check(&db, file);
    let diagnostics = &result.diagnostics;
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code == arandu_middle::DiagCode::T024NonExhaustiveMatch
                && diagnostic.message.contains("Sum")
        }),
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn sealed_exhaustiveness_discovers_implementations_across_package_modules() {
    let mut db = DatabaseImpl::new();
    let interface_file = db.new_file(
        "app/src/expr.aru".into(),
        "public sealed interface Expr {}".into(),
    );
    let implementations_file = db.new_file(
        "app/src/expr_impls.aru".into(),
        concat!(
            "from self.expr import { Expr }\n",
            "public struct Number {}\n",
            "public struct Sum {}\n",
            "impl Number: Expr {}\n",
            "impl Sum: Expr {}\n",
        )
        .into(),
    );
    let main_file = db.new_file(
        "app/src/main.aru".into(),
        concat!(
            "from self.expr import { Expr }\n",
            "from self.expr_impls import { Number, Sum }\n",
            "func eval(e: Expr): int { return match e { Number {} => 1 } }\n",
        )
        .into(),
    );
    let (package, target, _) = ids();
    let map = PackageModuleMap::new(
        &db,
        package,
        target,
        Arc::new(vec![
            (
                "self/expr.aru".into(),
                ModuleBinding {
                    package,
                    target,
                    module: ModuleId::try_from_usize(1).unwrap(),
                    file: interface_file,
                },
            ),
            (
                "self/expr_impls.aru".into(),
                ModuleBinding {
                    package,
                    target,
                    module: ModuleId::try_from_usize(2).unwrap(),
                    file: implementations_file,
                },
            ),
            (
                "self/main.aru".into(),
                ModuleBinding {
                    package,
                    target,
                    module: ModuleId::try_from_usize(3).unwrap(),
                    file: main_file,
                },
            ),
        ]),
    );
    db.set_package_module_map(map);

    let result = arandu_query::passes::type_check(&db, main_file);
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == arandu_middle::DiagCode::T024NonExhaustiveMatch
                && diagnostic.message.contains("Sum")
        }),
        "diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn dependency_body_edit_keeps_the_same_export_contract() {
    let mut db = DatabaseImpl::new();
    let dependency = db.new_file(
        "dependency/src/lib.aru".into(),
        "public func answer(): int { return 41 }".into(),
    );
    let importer = db.new_file(
        "app/src/main.aru".into(),
        "import math as math\nfunc main(): int { return math.answer() }".into(),
    );
    let (package, target, module) = ids();
    let map = PackageModuleMap::new(
        &db,
        PackageId::try_from_usize(0).unwrap(),
        TargetId::try_from_usize(0).unwrap(),
        Arc::new(vec![(
            "math.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file: dependency,
            },
        )]),
    );
    db.set_package_module_map(map);
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .is_empty());

    dependency
        .set_text(&mut db)
        .to(Arc::from("public func answer(): int { return 42 }"));
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .is_empty());
}

#[test]
fn package_mode_migrates_implicit_local_import_with_structured_replacement() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    db.new_file(
        "util.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let importer = db.new_file(
        "src/main.aru".into(),
        "import util as util\nfunc main(): int { return util.answer() }".into(),
    );

    let result = arandu_query::passes::type_check(&db, importer);
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == arandu_middle::DiagCode::M004LegacyLocalImport)
        .expect("legacy local import must have a migration diagnostic");
    let replacement = diagnostic
        .hints
        .iter()
        .find_map(|hint| hint.replacement.as_ref())
        .expect("migration diagnostic must carry a structured replacement");
    assert_eq!(replacement.new_text, "import self.util as util");
}

#[test]
fn package_mode_rejects_quoted_filesystem_import_before_alias_collection() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    db.new_file(
        "vendor/util.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let importer = db.new_file(
        "src/main.aru".into(),
        "import \"vendor/util.aru\" as vendor\nfunc main(): int { return 0 }".into(),
    );

    let result = arandu_query::passes::type_check(&db, importer);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == arandu_middle::DiagCode::M005FilesystemImportForbidden
    }));
    assert!(result
        .symbols
        .lookup_module(result.symbols.global_scope(), "vendor")
        .is_none());
}

#[test]
fn reexport_facade_public_exports() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    let inner = db.new_file(
        "inner.aru".into(),
        "public func compute(): int { return 42 }\npublic struct Item { public value: int }".into(),
    );
    let facade = db.new_file(
        "facade.aru".into(),
        "public use self.inner.{ compute, Item }".into(),
    );
    let consumer = db.new_file(
        "consumer.aru".into(),
        "from self.facade import { compute, Item }\nfunc test(): int { let x = Item { value: compute() }; return x.value }".into(),
    );

    let package = PackageId::try_from_usize(1).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let inner_mod = ModuleId::try_from_usize(1).unwrap();
    let facade_mod = ModuleId::try_from_usize(2).unwrap();
    let consumer_mod = ModuleId::try_from_usize(3).unwrap();

    let mut bindings: Vec<(String, ModuleBinding)> = vec![
        (
            "self/consumer.aru".into(),
            ModuleBinding {
                package,
                target,
                module: consumer_mod,
                file: consumer,
            },
        ),
        (
            "self/facade.aru".into(),
            ModuleBinding {
                package,
                target,
                module: facade_mod,
                file: facade,
            },
        ),
        (
            "self/inner.aru".into(),
            ModuleBinding {
                package,
                target,
                module: inner_mod,
                file: inner,
            },
        ),
    ];
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    let map = PackageModuleMap::new(&db, package, target, Arc::new(bindings));
    db.set_package_module_map(map);

    let exports = arandu_query::passes::exported_symbols(&db, facade);
    assert!(exports.symbols.contains_key("compute"));
    assert!(exports.symbols.contains_key("Item"));

    let res = arandu_query::passes::type_check(&db, consumer);
    assert!(res.diagnostics.is_empty(), "diags: {:?}", res.diagnostics);
}

#[test]
fn reexport_internal_package_scoped() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    let inner = db.new_file(
        "inner.aru".into(),
        "internal func secret(): int { return 99 }".into(),
    );
    let facade = db.new_file(
        "facade.aru".into(),
        "internal use self.inner.{ secret }".into(),
    );
    let same_pkg_consumer = db.new_file(
        "same_pkg.aru".into(),
        "from self.facade import { secret }\nfunc test(): int { return secret() }".into(),
    );

    let package = PackageId::try_from_usize(1).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let inner_mod = ModuleId::try_from_usize(1).unwrap();
    let facade_mod = ModuleId::try_from_usize(2).unwrap();
    let consumer_mod = ModuleId::try_from_usize(3).unwrap();

    let mut bindings: Vec<(String, ModuleBinding)> = vec![
        (
            "self/facade.aru".into(),
            ModuleBinding {
                package,
                target,
                module: facade_mod,
                file: facade,
            },
        ),
        (
            "self/inner.aru".into(),
            ModuleBinding {
                package,
                target,
                module: inner_mod,
                file: inner,
            },
        ),
        (
            "self/same_pkg.aru".into(),
            ModuleBinding {
                package,
                target,
                module: consumer_mod,
                file: same_pkg_consumer,
            },
        ),
    ];
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    let map = PackageModuleMap::new(&db, package, target, Arc::new(bindings));
    db.set_package_module_map(map);

    let exports = arandu_query::passes::exported_symbols(&db, facade);
    assert!(!exports.symbols.contains_key("secret"));

    let internal = arandu_query::passes::internal_symbols(&db, facade);
    assert!(internal.internal_symbols.contains_key("secret"));

    let res = arandu_query::passes::type_check(&db, same_pkg_consumer);
    assert!(res.diagnostics.is_empty(), "diags: {:?}", res.diagnostics);
}

#[test]
fn reexport_narrowing_diagnostic_n018() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    let inner = db.new_file(
        "inner.aru".into(),
        "internal func secret(): int { return 99 }".into(),
    );
    let facade = db.new_file(
        "facade.aru".into(),
        "public use self.inner.{ secret }".into(),
    );

    let package = PackageId::try_from_usize(1).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let inner_mod = ModuleId::try_from_usize(1).unwrap();
    let facade_mod = ModuleId::try_from_usize(2).unwrap();

    let mut bindings: Vec<(String, ModuleBinding)> = vec![
        (
            "self/facade.aru".into(),
            ModuleBinding {
                package,
                target,
                module: facade_mod,
                file: facade,
            },
        ),
        (
            "self/inner.aru".into(),
            ModuleBinding {
                package,
                target,
                module: inner_mod,
                file: inner,
            },
        ),
    ];
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    let map = PackageModuleMap::new(&db, package, target, Arc::new(bindings));
    db.set_package_module_map(map);

    let res = arandu_query::passes::type_check(&db, facade);
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.code == arandu_middle::DiagCode::N018ReExportNarrowing),
        "expected N018 diagnostic, got: {:?}",
        res.diagnostics
    );
}

#[test]
fn reexport_cycle_diagnostic_n019() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    let a = db.new_file("mod_a.aru".into(), "public use self.mod_b.{ Item }".into());
    let b = db.new_file("mod_b.aru".into(), "public use self.mod_a.{ Item }".into());

    let package = PackageId::try_from_usize(1).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let mod_a_id = ModuleId::try_from_usize(1).unwrap();
    let mod_b_id = ModuleId::try_from_usize(2).unwrap();

    let map = PackageModuleMap::new(
        &db,
        package,
        target,
        Arc::new(vec![
            (
                "self/mod_a.aru".into(),
                ModuleBinding {
                    package,
                    target,
                    module: mod_a_id,
                    file: a,
                },
            ),
            (
                "self/mod_b.aru".into(),
                ModuleBinding {
                    package,
                    target,
                    module: mod_b_id,
                    file: b,
                },
            ),
        ]),
    );
    db.set_package_module_map(map);

    let res = arandu_query::passes::type_check(&db, a);
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.code == arandu_middle::DiagCode::N019CyclicReExport),
        "expected N019 diagnostic, got: {:?}",
        res.diagnostics
    );
}

#[test]
fn inline_submodule_definition_and_qualified_access() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        r#"
module math {
    public struct Vector {
        public x: int,
        public y: int,
    }

    public func add(a: int, b: int): int {
        return a + b
    }
}

func main(): int {
    let v = math.Vector { x: 10, y: 20 };
    return math.add(v.x, v.y)
}
"#
        .into(),
    );

    let res = arandu_query::passes::type_check(&db, file);
    assert!(res.diagnostics.is_empty(), "diags: {:?}", res.diagnostics);
}

#[test]
fn inline_submodule_methods_keep_their_own_type_namespace() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        concat!(
            "module first {\n",
            "  public struct Item { public value: int }\n",
            "  public func Item.get(self: ref Item): int { return self.value }\n",
            "}\n",
            "module second {\n",
            "  public struct Item { public value: int }\n",
            "  public func Item.get(self: ref Item): int { return self.value }\n",
            "}\n",
            "func main(): int {\n",
            "  let a = first.Item { value: 1 }\n",
            "  let b = second.Item { value: 2 }\n",
            "  return a.get() + b.get()\n",
            "}\n",
        )
        .into(),
    );

    let result = arandu_query::passes::type_check(&db, file);
    assert!(
        result.diagnostics.is_empty(),
        "diags: {:?}",
        result.diagnostics
    );
}

#[test]
fn prelude_facade_reexports_enum_and_struct_types_accessible_to_consumer() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);
    let opt_file = db.new_file(
        "status.aru".into(),
        r#"
public enum Status {
    Active(int),
    Inactive,
}
"#
        .into(),
    );
    let prelude_file = db.new_file(
        "prelude.aru".into(),
        "public use self.status.{ Status }".into(),
    );
    let consumer_file = db.new_file(
        "consumer.aru".into(),
        r#"
from self.prelude import { Status }

func check(x: Status): int {
    match x {
        Active(v) => { return v }
        Inactive => { return 0 }
    }
}
"#
        .into(),
    );

    let package = PackageId::try_from_usize(1).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let opt_mod = ModuleId::try_from_usize(1).unwrap();
    let prelude_mod = ModuleId::try_from_usize(2).unwrap();
    let consumer_mod = ModuleId::try_from_usize(3).unwrap();

    let mut bindings: Vec<(String, ModuleBinding)> = vec![
        (
            "self/status.aru".into(),
            ModuleBinding {
                package,
                target,
                module: opt_mod,
                file: opt_file,
            },
        ),
        (
            "self/prelude.aru".into(),
            ModuleBinding {
                package,
                target,
                module: prelude_mod,
                file: prelude_file,
            },
        ),
        (
            "self/consumer.aru".into(),
            ModuleBinding {
                package,
                target,
                module: consumer_mod,
                file: consumer_file,
            },
        ),
    ];
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    let map = PackageModuleMap::new(&db, package, target, Arc::new(bindings));
    db.set_package_module_map(map);

    let res = arandu_query::passes::type_check(&db, consumer_file);
    assert!(res.diagnostics.is_empty(), "diags: {:?}", res.diagnostics);
}

#[test]
fn internal_func_rejected_for_foreign_package_consumer() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&mut db);

    let reactor_file = db.new_file(
        "reactor.aru".into(),
        r#"
internal func reactorRegisterSocket(sockId: int): int {
    return sockId + 1
}
"#
        .into(),
    );
    let foreign_consumer = db.new_file(
        "app.aru".into(),
        r#"
from runtime.reactor import { reactorRegisterSocket }

func main(): int {
    return reactorRegisterSocket(10)
}
"#
        .into(),
    );

    let pkg_runtime = PackageId::try_from_usize(1).unwrap();
    let pkg_app = PackageId::try_from_usize(2).unwrap();
    let target = TargetId::try_from_usize(1).unwrap();
    let reactor_mod = ModuleId::try_from_usize(1).unwrap();
    let app_mod = ModuleId::try_from_usize(2).unwrap();

    let mut bindings: Vec<(String, ModuleBinding)> = vec![
        (
            "runtime/reactor.aru".into(),
            ModuleBinding {
                package: pkg_runtime,
                target,
                module: reactor_mod,
                file: reactor_file,
            },
        ),
        (
            "app/main.aru".into(),
            ModuleBinding {
                package: pkg_app,
                target,
                module: app_mod,
                file: foreign_consumer,
            },
        ),
    ];
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    let map = PackageModuleMap::new(&db, pkg_app, target, Arc::new(bindings));
    db.set_package_module_map(map);

    let res = arandu_query::passes::type_check(&db, foreign_consumer);
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.code == arandu_middle::DiagCode::N016InternalOutsidePackage),
        "expected N016 diagnostic, got: {:?}",
        res.diagnostics
    );
}
