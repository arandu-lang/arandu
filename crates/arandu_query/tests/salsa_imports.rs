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
    db.new_file(
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
