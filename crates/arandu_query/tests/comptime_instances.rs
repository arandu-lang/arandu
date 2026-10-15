#![allow(clippy::expect_used)]
use arandu_middle::Severity;
use arandu_query::{passes, DatabaseImpl};
use salsa::Setter;

#[test]
fn concrete_instances_select_only_their_branch_and_freeze_dependent_arguments() {
    let sources = [
        "func choose<T>(): int { comptime if @sizeOf(T) == 4 { return 42 } else { return unavailable() } }\nfunc main(): int { return choose<int>() }",
        "func choose<comptime N: uint>(): uint { comptime if N == 4 { return 42 } else { return N } }\nfunc main(): uint { return choose<4>() + choose<8>() }",
        "func count<comptime N: uint>(): uint { return N }\nfunc next<comptime N: uint>(): uint { return count<comptime (N + 1)>() }\nfunc main(): uint { return next<4>() + next<8>() }",
        "func size<T>(): usize { return comptime @sizeOf(T) }\nfunc main(): usize { return size<int>() + size<i64>() }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { comptime if i < 2 { sum += i } else { sum += 10 } }\nreturn sum }",
        "func count<comptime N: uint>(): uint { return N }\nfunc main(): uint { let mut sum: uint = 0\ncomptime for i in 0..4 { sum += count<comptime (i + 1)>() }\nreturn sum }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { let n = comptime (i + 1)\nsum += n }\nreturn sum }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { comptime for j in 0..i { sum += j } }\nreturn sum }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { comptime for j in 0..i { comptime if j == 1 { sum += 10 } else { sum += i + j } } }\nreturn sum }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { comptime if i < 2 { comptime for j in 0..i { sum += j } } else { comptime for j in 0..2 { sum += i + j } } }\nreturn sum }",
    ];
    for source in sources {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("instances.aru".into(), source.into());
        let lowered = passes::lower_amir(&db, file);
        let errors: Vec<_> = lowered
            .type_check
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{source}\n{errors:?}");
        assert!(
            !lowered.amir.funcs.is_empty(),
            "{source}\n{:?}",
            arandu_query::runtime::runtime_program(&db, file).diagnostics
        );
    }
}

#[test]
fn changing_nested_occurrences_matches_a_clean_compilation() {
    let source = "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { comptime for j in 0..i { comptime if j == 1 { sum += 10 } else { let n = comptime (i + j)\nsum += n } } }\nreturn sum }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("nested.aru".into(), source.into());
    assert!(arandu_query::runtime::runtime_program(&db, file)
        .diagnostics
        .is_empty());
    let edited = source.replace("j == 1", "j == 2");
    file.set_text(&mut db).to(edited.clone().into());
    let incremental = arandu_query::runtime::runtime_program(&db, file);
    let mut clean = DatabaseImpl::new();
    let fresh = clean.new_file("nested.aru".into(), edited);
    let rebuilt = arandu_query::runtime::runtime_program(&clean, fresh);
    assert!(
        incremental.diagnostics.is_empty(),
        "{:?}",
        incremental.diagnostics
    );
    assert!(rebuilt.diagnostics.is_empty(), "{:?}", rebuilt.diagnostics);
    assert_eq!(
        format!("{:?}", incremental.artifacts.amir),
        format!("{:?}", rebuilt.artifacts.amir)
    );
}

#[test]
fn concrete_static_selection_cycles_fail_without_panicking() {
    for source in [
        "func choose<comptime N: uint>(): int { comptime if choose<N>() == 0 { return 1 } else { return 2 } }\nfunc main(): int { return choose<4>() }",
        "func first<comptime N: uint>(): int { comptime if second<N>() == 0 { return 1 } else { return 2 } }\nfunc second<comptime N: uint>(): int { comptime if first<N>() == 0 { return 1 } else { return 2 } }\nfunc main(): int { return first<4>() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("concrete-cycle.aru".into(), source.into());
        let output = arandu_query::runtime::runtime_program(&db, file);
        assert!(output.diagnostics.iter().any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::T044ComptimeEvaluationFailed), "{:?}", output.diagnostics);
        assert!(output.diagnostics.iter().all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")), "{:?}", output.diagnostics);
    }
}

#[test]
fn public_roots_can_call_concrete_generic_helpers() {
    for source in [
        "func choose<comptime N: uint>(): uint { return N + 1 }\nfunc main(): uint { return comptime choose<41>() }",
        "func identity<T>(value: T): T { return value }\nfunc main(): int { return comptime identity<int>(42) }",
        "func ready<comptime N: uint>(): bool { return N == 4 }\nfunc choose<comptime N: uint>(): uint { comptime if ready<N>() { return N + 1 } else { return absent } }\nfunc main(): uint { return choose<4>() }",
        "func choose<comptime N: uint>(): uint { return N + 1 }\nfunc wrapper<comptime N: uint>(): uint { return comptime choose<N>() }\nfunc main(): uint { return wrapper<41>() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("generic-helper.aru".into(), source.into());
        let output = arandu_query::runtime::runtime_program(&db, file);
        assert!(output.diagnostics.is_empty(), "{source}\n{:?}", output.diagnostics);
    }
}

#[test]
fn a_sibling_body_edit_cuts_off_the_staged_concrete_runtime_unit() {
    let source = "func choose<comptime N: uint>(): uint { return N + 1 }\nfunc wrapper<comptime N: uint>(): uint { return comptime choose<N>() }\nfunc sibling(): int { return 10 }";
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("generic-cutoff.aru".into(), source.into());
    let headers = passes::resolved_headers(&db, file);
    let owner = headers
        .declarations
        .symbols
        .iter()
        .find(|symbol| symbol.name == "wrapper")
        .expect("wrapper")
        .id;
    let key = arandu_middle::types::FunctionInstance {
        definition: owner,
        arguments: vec![arandu_middle::types::TypeShape::Const(41)],
    };
    let first = arandu_query::runtime::runtime_raw_unit(
        &db,
        arandu_query::runtime::Instance::new(&db, file, key.clone()),
    );
    assert!(first.result.is_ok(), "{:?}", first.result);
    log.clear();
    file.set_text(&mut db)
        .to(source.replace("return 10", "return 11").into());
    let retained = arandu_query::runtime::runtime_raw_unit(
        &db,
        arandu_query::runtime::Instance::new(&db, file, key),
    );
    assert!(retained.result.is_ok(), "{:?}", retained.result);
    assert_eq!(
        log.count_executions_matching("runtime_raw_unit"),
        0,
        "{}",
        log.format_chain(true)
    );
    assert_eq!(
        log.count_executions_matching("ctfe_eval_root"),
        0,
        "{}",
        log.format_chain(true)
    );
}
