//! Source-to-AMIR SIMD identity, signature and opaque-storage regressions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_middle::amir::{AmirRvalue, AmirStmt};
use arandu_middle::types::{ArType, VectorType};
use arandu_query::db::DatabaseImpl;
use arandu_query::{file_ide_diagnostics, runtime::runtime_program};

#[path = "common/mod.rs"]
mod common;

fn database() -> DatabaseImpl {
    let mut db = DatabaseImpl::default();
    for &(path, source) in common::STDLIB_MODULES {
        let file = db.new_stdlib_file(path.to_owned(), source.to_owned());
        arandu_query::passes::parse(&db, file)
            .as_ref()
            .unwrap_or_else(|error| {
                panic!("{path} must parse before testing its surface: {error}")
            });
    }
    db
}

#[test]
fn all_six_canonical_source_shapes_lower_to_native_vector_nodes() {
    for shape in VectorType::ALL {
        let mut db = database();
        let name = shape.name();
        let file = db.new_file(
            "main.aru".into(),
            format!(
                r#"
            import std.core.simd as renamed
            func main(): int {{
                let a = renamed.{name}.splat(2)
                let b = a.add(a)
                let mask = b.eq(b)
                let selected = renamed.{name}.select(mask, b, a)
                return selected.extractLane(0) as int
            }}
        "#
            ),
        );
        let errors: Vec<_> = file_ide_diagnostics(&db, file)
            .iter()
            .filter(|d| d.severity == 0)
            .cloned()
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let runtime = runtime_program(&db, file);
        assert!(
            !runtime
                .diagnostics
                .iter()
                .any(|d| d.severity == arandu_middle::Severity::Error),
            "{name}: {:?}",
            runtime.diagnostics
        );
        let program = &runtime.artifacts.amir;
        let mut splat = false;
        let mut binary = false;
        let mut compare = false;
        let mut select = false;
        let mut extract = false;
        for func in &program.funcs {
            let info = &runtime.artifacts.type_check.type_info;
            for temp in &func.temps {
                if info.type_interner.resolve(temp.ty) == ArType::Vector(shape) {
                    splat = true;
                }
            }
            for block in &func.blocks {
                for stmt in func.block_stmts(block.id) {
                    if let AmirStmt::Assign { rhs, .. } = stmt {
                        match rhs {
                            AmirRvalue::VectorSplat { .. } => splat = true,
                            AmirRvalue::VectorOp { .. } => binary = true,
                            AmirRvalue::VectorCompare { .. } => compare = true,
                            AmirRvalue::VectorSelect { .. } => select = true,
                            AmirRvalue::VectorExtractLane { .. } => extract = true,
                            _ => {}
                        }
                    }
                }
            }
        }
        assert!(
            splat && binary && compare && select && extract,
            "{name}: missing native nodes"
        );
        assert!(!program.funcs.iter().flat_map(|func| func.blocks.iter().flat_map(move |block| func.block_stmts(block.id))).any(|stmt| {
            matches!(stmt, AmirStmt::Call { callee: arandu_middle::amir::AmirOperand::FunctionRef(symbol), .. }
                if runtime.artifacts.type_check.symbols.get(*symbol).lang_item == Some(arandu_middle::symbol_table::LangItem::SimdIntrinsic))
        }), "lowered intrinsics must not remain runtime calls");
    }
}

#[test]
fn nominal_lookalike_stays_nominal_and_cannot_satisfy_simd_signature() {
    let mut db = database();
    let file = db.new_file(
        "fake.aru".into(),
        r#"
        import std.core.simd as simd
        struct F32x4 { lane: f32 }
        func main(): int {
            let fake = F32x4 { lane: 1.0 }
            let native = simd.F32x4.splat(1.0)
            let invalid = native.add(fake)
            return 0
        }
    "#
        .into(),
    );
    assert!(
        file_ide_diagnostics(&db, file)
            .iter()
            .any(|d| d.severity == 0),
        "a same-spelling nominal is not a native vector"
    );
}

#[test]
fn intrinsic_abi_rejects_wrong_lane_width_and_struct_literal_construction() {
    for source in [
        r#"module std.core.simd
           public struct F32x4 {}
           extern "arandu-intrinsic" { func simdF32x4Splat(value: f64): F32x4 }
        "#,
        r#"import std.core.simd as simd
           func main(): int { let invalid = simd.F32x4 {} return 0 }
        "#,
    ] {
        let mut db = database();
        let file = db.new_file("invalid.aru".into(), source.into());
        if source.contains("module std.core.simd") {
            db.trust_stdlib_source(file);
        }
        assert!(
            file_ide_diagnostics(&db, file)
                .iter()
                .any(|d| d.severity == 0 && d.code == "U001"),
            "{source}: {:?}",
            **file_ide_diagnostics(&db, file)
        );
    }
}

#[test]
fn ordinary_function_named_like_simd_intrinsic_remains_an_ordinary_call() {
    let mut db = database();
    let file = db.new_file(
        "ordinary.aru".into(),
        r#"
        func simdF32x4Splat(value: int): int { return value + 7 }
        func main(): int { return simdF32x4Splat(2) }
    "#
        .into(),
    );
    let runtime = runtime_program(&db, file);
    assert!(
        !runtime
            .diagnostics
            .iter()
            .any(|d| d.severity == arandu_middle::Severity::Error),
        "{:?}",
        runtime.diagnostics
    );
    let program = &runtime.artifacts.amir;
    assert!(program
        .funcs
        .iter()
        .flat_map(|func| func
            .blocks
            .iter()
            .flat_map(move |block| func.block_stmts(block.id)))
        .any(|stmt| matches!(stmt, AmirStmt::Call { .. })));
}

#[test]
fn import_path_suffix_does_not_promote_a_foreign_nominal_declaration() {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/core/simd.aru".into(),
        r#"
        module user.vector
        public struct F32x4 { public value: f32 }
    "#
        .into(),
    );
    let file = db.new_file(
        "main.aru".into(),
        r#"
        import std.core.simd as simd
        func main(): int {
            let value = simd.F32x4 { value: 3.0 }
            return value.value as int
        }
    "#
        .into(),
    );
    let errors: Vec<_> = file_ide_diagnostics(&db, file)
        .iter()
        .filter(|d| d.severity == 0)
        .cloned()
        .collect();
    assert!(
        errors.is_empty(),
        "only the declaration module establishes SIMD identity: {errors:?}"
    );
}

#[test]
fn canonical_vector_identity_survives_facade_reexport_and_named_alias() {
    let mut db = database();
    db.new_file(
        "facade.aru".into(),
        "public use std.core.simd.{ F32x4 }".into(),
    );
    let file = db.new_file(
        "main.aru".into(),
        r#"
        from facade import { F32x4 as Four }
        func preserve(value: Four): Four { return value }
    "#
        .into(),
    );
    let errors: Vec<_> = file_ide_diagnostics(&db, file)
        .iter()
        .filter(|d| d.severity == 0)
        .cloned()
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let runtime = runtime_program(&db, file);
    assert!(
        !runtime
            .diagnostics
            .iter()
            .any(|d| d.severity == arandu_middle::Severity::Error),
        "{:?}",
        runtime.diagnostics
    );
    let function = runtime
        .artifacts
        .amir
        .funcs
        .iter()
        .find(|func| runtime.artifacts.type_check.symbols.get(func.symbol).name == "preserve")
        .expect("preserve AMIR");
    assert_eq!(
        runtime
            .artifacts
            .type_check
            .type_info
            .type_interner
            .resolve(function.return_type),
        ArType::Vector(VectorType::F32x4)
    );
}

#[test]
fn declaring_a_standard_namespace_cannot_forge_transfer_or_vector_identity() {
    for (module, name) in [
        ("std.sync.rwlock", "RwLock"),
        ("std.sync.mutex", "Mutex"),
        ("std.core.cell", "UnsafeCell"),
        ("std.alloc.arc", "Arc"),
        ("std.alloc.arc", "ArcWeak"),
        ("std.alloc.string", "String"),
        ("std.alloc.vec", "Vec"),
    ] {
        let mut db = database();
        let file = db.new_file(
            "forged.aru".into(),
            format!(
                r#"
            module {module}
            import std.core.marker as marker
            public struct {name}<T> {{ public raw: ptr[u8] }}
            func demand<T: marker.Send + marker.Sync>(value: T): int {{ return 0 }}
            func main(): int {{ return demand<{name}<int>>({name}<int> {{ raw: nil }}) }}
        "#
            ),
        );
        let errors: Vec<_> = file_ide_diagnostics(&db, file)
            .iter()
            .filter(|d| d.severity == 0)
            .cloned()
            .collect();
        assert!(
            errors
                .iter()
                .any(|d| matches!(d.code.as_str(), "T011" | "T025")),
            "{module}.{name}: {errors:?}"
        );
        let locals = arandu_query::passes::local_symbols(&db, file);
        assert!(locals
            .symbols
            .iter()
            .filter(|symbol| symbol.span.end > symbol.span.start)
            .all(|symbol| symbol.lang_item.is_none()));
    }
    let mut db = database();
    let file = db.new_file("forged_vector.aru".into(), "module std.core.simd\npublic struct F32x4 {}\nfunc preserve(value:F32x4):F32x4{return value}".into());
    let runtime = runtime_program(&db, file);
    assert!(
        !runtime
            .diagnostics
            .iter()
            .any(|d| d.severity == arandu_middle::Severity::Error),
        "{:?}",
        runtime.diagnostics
    );
    let function = runtime
        .artifacts
        .amir
        .funcs
        .iter()
        .find(|f| runtime.artifacts.type_check.symbols.get(f.symbol).name == "preserve")
        .unwrap();
    assert!(matches!(
        runtime
            .artifacts
            .type_check
            .type_info
            .type_interner
            .resolve(function.return_type),
        ArType::Named(_, _)
    ));
}

#[test]
fn stdlib_authority_is_per_file_and_does_not_survive_file_replacement() {
    use arandu_middle::symbol_table::LangItem;
    use arandu_query::passes::local_symbols;
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "module std.core.simd\npublic struct F32x4 {}";
    let first = db.new_file("stdlib/core/simd.aru".into(), source.into());
    let unrelated = db.new_file("other.aru".into(), "func other():int{return 0}".into());
    assert!(!local_symbols(&db, first)
        .symbols
        .iter()
        .any(|s| s.lang_item == Some(LangItem::VectorF32x4)));
    local_symbols(&db, unrelated);
    log.clear();
    db.trust_stdlib_source(first);
    assert!(local_symbols(&db, first)
        .symbols
        .iter()
        .any(|s| s.lang_item == Some(LangItem::VectorF32x4)));
    local_symbols(&db, unrelated);
    assert_eq!(
        log.count_executions_matching("local_symbols"),
        1,
        "granting one file authority cannot invalidate another file's locals"
    );
    let replacement = db.new_file("stdlib/core/simd.aru".into(), source.into());
    assert_ne!(*first.file_id(&db), *replacement.file_id(&db));
    assert!(!db.is_trusted_stdlib_source(replacement));
    assert!(!local_symbols(&db, replacement)
        .symbols
        .iter()
        .any(|s| s.lang_item == Some(LangItem::VectorF32x4)));
}

#[test]
fn canonical_vector_methods_specialize_inferred_and_explicit_generic_payloads() {
    let mut db = database();
    let module = db.new_stdlib_file(
        "stdlib/core/simd.aru".into(),
        r#"
        module std.core.simd
        public struct F32x4 {}
        public func F32x4.payload<T>(self: F32x4, value: T): T { return value }
    "#
        .into(),
    );
    let definition = arandu_query::passes::local_symbols(&db, module)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "F32x4.payload")
        .unwrap()
        .id;
    let file = db.new_file(
        "generic_vector.aru".into(),
        r#"
        import std.core.simd as simd
        func inferred(vector: simd.F32x4): int { return vector.payload(42) }
        func explicit(vector: simd.F32x4): int { return vector.payload<int>(42) }
    "#
        .into(),
    );
    let errors: Vec<_> = file_ide_diagnostics(&db, file)
        .iter()
        .filter(|d| d.severity == 0)
        .cloned()
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let runtime = runtime_program(&db, file);
    assert!(
        !runtime
            .diagnostics
            .iter()
            .any(|d| d.severity == arandu_middle::Severity::Error),
        "{:?}",
        runtime.diagnostics
    );
    assert!(
        runtime
            .instances
            .iter()
            .any(|(_, key)| key.definition == definition
                && key.arguments
                    == vec![arandu_middle::types::TypeShape::Primitive(
                        arandu_middle::types::Primitive::Int
                    )]),
        "the payload generic argument must survive source inference into the instance graph"
    );
}

#[test]
fn stdlib_authority_cannot_be_inherited_by_reusing_a_manual_file_id() {
    let mut db = database();
    let trusted = db.new_stdlib_file(
        "stdlib/internal.aru".into(),
        "module std.core.simd\npublic struct F32x4 {}".into(),
    );
    let file_id = *trusted.file_id(&db);
    let forged = arandu_query::SourceFile::new(
        &db,
        file_id,
        std::sync::Arc::from("module std.core.simd\npublic struct F32x4 {}"),
        std::sync::Arc::new(std::path::PathBuf::from("forged.aru")),
    );
    assert!(db.is_trusted_stdlib_source(trusted));
    assert!(!db.is_trusted_stdlib_source(forged));
}

#[test]
fn comptime_simd_operations_evaluate_and_fold_at_compile_time() {
    let mut db = database();
    let file = db.new_file(
        "main.aru".into(),
        r#"
        import std.core.simd as simd
        func compute(): int {
            let a = simd.I32x4.splat(20)
            let b = a.add(a)
            return b.extractLane(0) as int
        }
        func main(): int {
            let res = comptime compute()
            return res
        }
        "#
        .into(),
    );
    let errors: Vec<_> = file_ide_diagnostics(&db, file)
        .iter()
        .filter(|d| d.severity == 0)
        .cloned()
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let runtime = runtime_program(&db, file);
    assert!(
        !runtime
            .diagnostics
            .iter()
            .any(|d| d.severity == arandu_middle::Severity::Error),
        "{:?}",
        runtime.diagnostics
    );
    let amir = &runtime.artifacts.amir;
    let main_func = amir
        .funcs
        .iter()
        .find(|f| runtime.artifacts.type_check.symbols.get(f.symbol).name == "main")
        .expect("main exists");
    assert!(
        main_func
            .stmts
            .iter_ids()
            .all(|id| !matches!(main_func.stmt(id), AmirStmt::Call { .. })),
        "comptime compute must be folded; call should not exist in main"
    );
}
