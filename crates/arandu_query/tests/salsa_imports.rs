#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_query::DatabaseImpl;
use salsa::Setter;

/// Regression: builtin prelude (`import io` / `import err`) must resolve on the
/// Salsa/CLI path without requiring on-disk `io.aru` / `err.aru` files.
#[test]
fn test_prelude_import_io_err_without_files() {
    let mut db = DatabaseImpl::default();
    let src = r#"
        module tests.prelude_import

        import io
        import err

        func main() {
            let msg = err.new("x")
            io.println("ok")
        }
    "#;
    let file = db.new_file("tests_prelude_import.aru".to_string(), src.to_string());

    let resolved = arandu_query::passes::resolve(&db, file);
    let has_m001 = resolved
        .diagnostics
        .iter()
        .any(|d| matches!(d.code, arandu_middle::DiagCode::M001UnresolvedImport));
    assert!(
        !has_m001,
        "prelude import must not emit M001, got: {:?}",
        resolved.diagnostics
    );

    let tc = arandu_query::passes::type_check(&db, file);
    assert!(
        tc.diagnostics.is_empty(),
        "type check with import io/err should succeed, got: {:?}",
        tc.diagnostics
    );
}

#[test]
fn test_prelude_import_io_as_alias() {
    let mut db = DatabaseImpl::default();
    let src = r#"
        module tests.prelude_alias

        import io as out

        func main() {
            out.println("hi")
        }
    "#;
    let file = db.new_file("tests_prelude_alias.aru".to_string(), src.to_string());
    let tc = arandu_query::passes::type_check(&db, file);
    assert!(
        tc.diagnostics.is_empty(),
        "import io as out should resolve prelude members, got: {:?}",
        tc.diagnostics
    );
}

#[test]
fn test_cross_file_type_check() {
    let mut db = DatabaseImpl::default();
    let mod_b_text = r#"
        public func add(a: int, b: int): int {
            return a + b
        }
    "#;
    let _mod_b = db.new_file("mod_b.aru".to_string(), mod_b_text.to_string());

    let mod_a_text = r#"
        import mod_b
        func main(): int {
            return mod_b.add(10, 20)
        }
    "#;
    let mod_a = db.new_file("mod_a.aru".to_string(), mod_a_text.to_string());

    let tc_a = arandu_query::passes::type_check(&db, mod_a);
    assert!(
        tc_a.diagnostics.is_empty(),
        "Expected no diagnostics, got: {:?}",
        tc_a.diagnostics
    );
}

#[test]
fn test_early_cutoff_on_function_body_change() {
    let mut db = DatabaseImpl::default();

    // We create a base file mod_b
    let mod_b_text = "public func add(a: int, b: int): int {\n return a + b\n }";
    let mod_b = db.new_file("mod_b.aru".to_string(), mod_b_text.to_string());

    // And mod_a depends on mod_b
    let mod_a_text = "import mod_b\nfunc main(): int {\n return mod_b.add(1, 2)\n }";
    let mod_a = db.new_file("mod_a.aru".to_string(), mod_a_text.to_string());

    // 1. Evaluate mod_a typecheck.
    let tc1 = arandu_query::passes::type_check(&db, mod_a);
    assert!(tc1.diagnostics.is_empty());

    // 2. Change the body of mod_b but keep the signature the same
    let mod_b_text_new = "public func add(a: int, b: int): int {\n let c = a\n return c + b\n }";
    mod_b
        .set_text(&mut db)
        .to(std::sync::Arc::from(mod_b_text_new));

    // 3. Re-evaluate mod_a typecheck.
    let tc2 = arandu_query::passes::type_check(&db, mod_a);
    assert!(tc2.diagnostics.is_empty());
}

#[test]
fn test_early_cutoff_on_inherent_method_body_change() {
    let mut db = DatabaseImpl::default();

    // mod_b defines a struct and an inherent method on it
    let mod_b_text = "public struct Counter { public val: int }\npublic func Counter.compute(self: ref Counter): int {\n    return self.val + 1\n}\n";
    let mod_b = db.new_file("mod_b_method.aru".to_string(), mod_b_text.to_string());

    // mod_a imports mod_b and calls the inherent method
    let mod_a_text = "import mod_b_method\nfunc main(): int {\n    let c = mod_b_method.Counter { val: 10 }\n    return c.compute()\n}\n";
    let mod_a = db.new_file("mod_a_method.aru".to_string(), mod_a_text.to_string());

    // 1. Evaluate mod_a typecheck.
    let tc1 = arandu_query::passes::type_check(&db, mod_a);
    assert!(
        tc1.diagnostics.is_empty(),
        "initial tc failed: {:?}",
        tc1.diagnostics
    );

    // Exported symbols before
    let exp1 = (*arandu_query::passes::exported_symbols(&db, mod_b)).clone();

    // 2. Change the body of Counter.compute in mod_b
    let mod_b_text_new = "public struct Counter { public val: int }\npublic func Counter.compute(self: ref Counter): int {\n    let temp = self.val * 2\n    return temp - self.val + 1\n}\n";
    mod_b
        .set_text(&mut db)
        .to(std::sync::Arc::from(mod_b_text_new));

    // Exported symbols after
    let exp2 = (*arandu_query::passes::exported_symbols(&db, mod_b)).clone();
    assert_eq!(
        exp1, exp2,
        "exported symbols must remain identical across body edit"
    );

    // 3. Re-evaluate mod_a typecheck
    let tc2 = arandu_query::passes::type_check(&db, mod_a);
    assert!(
        tc2.diagnostics.is_empty(),
        "tc after edit failed: {:?}",
        tc2.diagnostics
    );
}

#[test]
fn test_early_cutoff_on_primitive_inherent_method_body_change() {
    let mut db = DatabaseImpl::default();

    // mod_b defines an inherent method on a primitive type (int)
    let mod_b_text = "public func int.double(self: int): int {\n    return self + self\n}\n";
    let mod_b = db.new_file("mod_b_prim.aru".to_string(), mod_b_text.to_string());

    // mod_a imports mod_b and calls the primitive inherent method
    let mod_a_text =
        "import mod_b_prim\nfunc main(): int {\n    let x: int = 21\n    return x.double()\n}\n";
    let mod_a = db.new_file("mod_a_prim.aru".to_string(), mod_a_text.to_string());

    // 1. Evaluate mod_a typecheck.
    let tc1 = arandu_query::passes::type_check(&db, mod_a);
    assert!(
        tc1.diagnostics.is_empty(),
        "initial tc failed: {:?}",
        tc1.diagnostics
    );

    // Exported symbols before
    let exp1 = (*arandu_query::passes::exported_symbols(&db, mod_b)).clone();

    // 2. Change the body of int.double in mod_b
    let mod_b_text_new =
        "public func int.double(self: int): int {\n    let two = 2\n    return self * two\n}\n";
    mod_b
        .set_text(&mut db)
        .to(std::sync::Arc::from(mod_b_text_new));

    // Exported symbols after
    let exp2 = (*arandu_query::passes::exported_symbols(&db, mod_b)).clone();
    assert_eq!(
        exp1, exp2,
        "exported symbols must remain identical across body edit"
    );

    // 3. Re-evaluate mod_a typecheck
    let tc2 = arandu_query::passes::type_check(&db, mod_a);
    assert!(
        tc2.diagnostics.is_empty(),
        "tc after edit failed: {:?}",
        tc2.diagnostics
    );
}

#[test]
fn test_cross_file_collision_during_circular_import_is_still_deterministic() {
    // circular dependency: A imports B, B imports A
    let mod_a_text = r#"
        import mod_b
        public func foo() {
            mod_b.bar()
        }
    "#;
    let mod_b_text = r#"
        import mod_a
        public func bar() {
            mod_a.foo()
        }
    "#;
    let run = |reverse: bool| {
        let mut db = DatabaseImpl::default();
        let mod_a;
        if reverse {
            let _mod_b = db.new_file("mod_b.aru".to_string(), mod_b_text.to_string());
            mod_a = db.new_file("mod_a.aru".to_string(), mod_a_text.to_string());
        } else {
            mod_a = db.new_file("mod_a.aru".to_string(), mod_a_text.to_string());
            let _mod_b = db.new_file("mod_b.aru".to_string(), mod_b_text.to_string());
        }

        let tc = arandu_query::passes::type_check(&db, mod_a);
        let mut diagnostics = tc
            .diagnostics
            .iter()
            .map(|d| (d.code.as_str(), d.message.clone(), d.span.start, d.span.end))
            .collect::<Vec<_>>();
        diagnostics.sort();
        diagnostics
    };

    let forward = run(false);
    let reverse = run(true);
    assert!(
        !forward.is_empty(),
        "circular import must emit a diagnostic"
    );
    assert_eq!(
        forward, reverse,
        "diagnostics must not depend on module registration order"
    );
}

#[test]
fn test_circular_import_diagnostics_survive_repeated_body_revisions() {
    let mut db = DatabaseImpl::default();
    let mod_a = db.new_file(
        "mod_a.aru".to_string(),
        r#"
            import mod_b
            public func foo() {
                mod_b.bar()
            }
        "#
        .to_string(),
    );
    let mod_b = db.new_file(
        "mod_b.aru".to_string(),
        r#"
            import mod_a
            public func bar() {
                let marker = 0
                mod_a.foo()
            }
        "#
        .to_string(),
    );

    let diagnostic_signature = |db: &DatabaseImpl| {
        let tc = arandu_query::passes::type_check(db, mod_a);
        let mut diagnostics = tc
            .diagnostics
            .iter()
            .map(|d| (d.code.as_str(), d.message.clone(), d.span.start, d.span.end))
            .collect::<Vec<_>>();
        diagnostics.sort();
        diagnostics
    };
    let expected = diagnostic_signature(&db);
    assert!(
        !expected.is_empty(),
        "circular import must emit a diagnostic before revisions"
    );

    for revision in 1..=16 {
        let marker = revision % 2;
        mod_b.set_text(&mut db).to(std::sync::Arc::from(format!(
            r#"
            import mod_a
            public func bar() {{
                let marker = {marker}
                mod_a.foo()
            }}
        "#
        )));
        assert_eq!(
            diagnostic_signature(&db),
            expected,
            "cycle diagnostics changed or disappeared after body revision {revision}"
        );
    }
}

#[test]
fn test_import_generic_spawn_infer_from_coroutine() {
    let mut db = arandu_query::DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/std/runtime/executor.aru".to_string(),
        include_str!("../../../stdlib/std/runtime/executor.aru").to_string(),
    );
    let src = r#"
        module tests.import_spawn_infer
        import std.runtime.executor as rt
        async func answer(): int { return 42 }
        func main(): int {
            let ex = rt.SyncExecutor.new()
            let h = ex.spawn(answer())
            return ex.join(h)
        }
    "#;
    let file = db.new_file("tests_import_spawn_infer.aru".to_string(), src.to_string());
    let tc = arandu_query::passes::type_check(&db, file);
    let errs: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, arandu_middle::Severity::Error))
        .map(|d| d.message.clone())
        .collect();
    assert!(errs.is_empty(), "import spawn infer failed: {errs:?}");
}

#[test]
fn test_local_generic_spawn_infer_still_ok() {
    let mut db = arandu_query::DatabaseImpl::default();
    let src = r#"
        module tests.local_spawn_infer
        extern "C" {
            func ar_rt_spawn_i64(state: ptr[u8]): int
            func ar_rt_join_i64(handle: int): int
        }
        struct SyncExecutor { flags: int }
        struct TaskHandle { id: int }
        func spawn_g<T>(shared ex: SyncExecutor, job: Coroutine<T>): TaskHandle {
            unsafe {
                let id = ar_rt_spawn_i64(job as ptr[u8])
                return TaskHandle { id: id }
            }
        }
        func join_g<T>(shared ex: SyncExecutor, handle: TaskHandle): T {
            unsafe {
                let v = ar_rt_join_i64(handle.id)
                return v as T
            }
        }
        async func answer(): int { return 42 }
        func main(): int {
            let ex = SyncExecutor { flags: 0 }
            let h = spawn_g(ex, answer())
            return join_g(ex, h)
        }
    "#;
    let file = db.new_file("tests_local_spawn_infer.aru".to_string(), src.to_string());
    let tc = arandu_query::passes::type_check(&db, file);
    let errs: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, arandu_middle::Severity::Error))
        .map(|d| d.message.clone())
        .collect();
    assert!(errs.is_empty(), "local spawn infer failed: {errs:?}");
}

#[test]
fn test_unresolved_named_import_suggests_levenshtein() {
    let mut db = DatabaseImpl::default();
    let _math = db.new_file(
        "math.aru".to_string(),
        "public func calculate_total() { }\n".to_string(),
    );
    let client = db.new_file(
        "client.aru".to_string(),
        "from math import { calculate_totla }\nfunc main() { }\n".to_string(),
    );
    let res = arandu_query::passes::resolve(&db, client);
    let errs: Vec<_> = res
        .diagnostics
        .iter()
        .filter(|d| d.code == arandu_middle::DiagCode::M001UnresolvedImport)
        .collect();
    assert_eq!(
        errs.len(),
        1,
        "Expected 1 M001 diagnostic, got: {:?}",
        res.diagnostics
    );
    let hints: Vec<_> = errs[0].hints.iter().map(|h| h.message.as_str()).collect();
    assert_eq!(hints, vec!["did you mean 'calculate_total'?"]);
}

#[test]
fn canonical_enum_constructors_keep_their_ir_types_through_reexports_and_aliases() {
    let mut db = DatabaseImpl::default();
    for (path, source) in [
        (
            "stdlib/core/option.aru",
            "module std.core.option\npublic enum Option<T> { Some(T), None }",
        ),
        (
            "stdlib/core/result.aru",
            "module std.core.result\npublic enum Result<T, E> { Ok(T), Err(E) }",
        ),
        (
            "stdlib/core/future.aru",
            "module std.core.future\npublic enum Poll<T> { Ready(T), Pending }",
        ),
    ] {
        db.new_stdlib_file(path.into(), source.into());
    }
    db.new_file(
        "facade.aru".into(),
        "public use std.core.option.{Option}\npublic use std.core.result.{Result}\npublic use std.core.future.{Poll}".into(),
    );
    let file = db.new_file(
        "main.aru".into(),
        r#"
from facade import { Option as Maybe, Result as Outcome, Poll as Progress }
enum Option<T> { Some(T), None }
func optional(): Maybe<int> { return Maybe.Some(42) }
func outcome(): Outcome<int, str> { return Outcome.Ok(42) }
func progress(value: Progress<int>): int {
    match value {
        Progress.Ready(item) => { return item }
        Progress.Pending => { return 0 }
    }
}
func main(): int {
    let nominal: Option<int> = Option.Some(7)
    let inferred = Maybe.Some(42)
    let ready = progress(Progress.Ready(42))
    let checked = outcome()
    match optional() {
        Maybe.Some(value) => { return ready - value }
        Maybe.None => { return 1 }
    }
}
"#
        .into(),
    );
    let result = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error),
        "canonical constructors must not produce nominal IR types: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_incremental_dependency_newline_edit() {
    let mut warm = arandu_query::AnalysisHost::new();
    let dep_orig = "module dependency\npublic func value(): int { return 1 }\n";
    let consumer_src =
        "module consumer\nimport dependency\nfunc main(): int { return dependency.value() }\n";
    let dep = warm.new_file("dependency.aru".into(), dep_orig.into());
    let consumer = warm.new_file("consumer.aru".into(), consumer_src.into());

    let _ = arandu_query::file_ide_diagnostics(warm.db(), consumer);

    let dep_edited = "module dependency\npublic func value()\n: int { return 1 }\n";
    warm.set_text(dep, dep_edited);

    let mut fresh = arandu_query::AnalysisHost::new();
    let fresh_dep = fresh.new_file("dependency.aru".into(), dep_edited.into());
    let fresh_consumer = fresh.new_file("consumer.aru".into(), consumer_src.into());

    let actual_dep = arandu_query::file_ide_diagnostics(warm.db(), dep);
    let expected_dep = arandu_query::file_ide_diagnostics(fresh.db(), fresh_dep);
    assert_eq!(**actual_dep, **expected_dep);

    let actual_consumer = arandu_query::file_ide_diagnostics(warm.db(), consumer);
    let expected_consumer = arandu_query::file_ide_diagnostics(fresh.db(), fresh_consumer);
    assert_eq!(**actual_consumer, **expected_consumer);
}
