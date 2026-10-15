#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{ctfe::ConstValue, types::Primitive, DataLayout, DiagCode, Severity};
use arandu_query::{
    passes::{lower_amir, type_check},
    DatabaseImpl,
};
use salsa::Setter;

fn codes(source: &str) -> Vec<DiagCode> {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("public.aru".into(), source.into());
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn value_parameter_marker_is_not_a_nested_comptime_root() {
    let source = "func main(): int { let answer: int = comptime { return 42 }\nreturn answer + (count<0>() as int) }\nfunc count<comptime N: uint>(): uint { return N }";
    let errors = codes(source);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn public_expression_and_block_materialize_without_runtime_helper_calls() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("public.aru".into(), "func add(a: int, b: int): int { return a + b }\nfunc main(): int { let x = comptime add(20, 22)\nreturn x }".into());
    let typed = type_check(&db, file);
    assert!(typed.diagnostics.is_empty(), "{:?}", typed.diagnostics);
    assert!(typed
        .type_info
        .ctfe_values
        .values()
        .any(|(value, _)| matches!(value, ConstValue::Integer(n) if n.value() == 42)));
    let lowered = lower_amir(&db, file);
    assert!(
        lowered
            .type_check
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{:?}",
        lowered.type_check.diagnostics
    );
    let program = &lowered.amir;
    let main = program
        .funcs
        .iter()
        .find(|f| lowered.type_check.symbols.get(f.symbol).name == "main")
        .expect("main");
    assert!(main
        .stmts
        .iter_ids()
        .all(|id| !matches!(main.stmt(id), arandu_middle::amir::AmirStmt::Call { .. })));
}

#[test]
fn frozen_values_from_distinct_files_never_alias_by_arena_index_or_offset() {
    let mut db = DatabaseImpl::new();
    let first = db.new_file(
        "first.aru".into(),
        "func main(): int { return comptime 42 }".into(),
    );
    let second = db.new_file(
        "second.aru".into(),
        "func main(): int { return comptime 43 }".into(),
    );
    let first = type_check(&db, first);
    let second = type_check(&db, second);
    let mut info = (*first.type_info).clone();
    info.merge_from(&second.type_info);
    assert_eq!(info.ctfe_values.len(), 2);
    let mut values: Vec<_> = info
        .ctfe_values
        .iter()
        .map(|(span, (value, _))| {
            let ConstValue::Integer(value) = value else {
                panic!("integer expected")
            };
            (span.file_id, value.value())
        })
        .collect();
    values.sort();
    assert_ne!(values[0].0, values[1].0);
    assert_eq!([values[0].1, values[1].1], [42, 43]);
}

#[test]
fn root_return_is_local_and_unifies_with_tail_and_unit() {
    for source in [
        "func main(): int { let x = comptime { return 42 }\nreturn x }",
        "func main(): int { let x: int = comptime { let y = 20; y + 22 }\nreturn x }",
        "func main(): int { let x = comptime { if true { return 42 } else { return 1 } }\nreturn x }",
        "func main(): int { comptime { return; }\nreturn 42 }",
        "func main(): int { comptime {}\nreturn 42 }",
    ] { assert!(codes(source).is_empty(), "{source}: {:?}", codes(source)); }
    for source in [
        "func main(): int { let x = comptime { return 42; false }\nreturn 0 }",
        "func main(): int { let x: int = comptime { return; }\nreturn 0 }",
    ] {
        assert!(
            codes(source).iter().any(|code| code.as_str() == "T004"),
            "{source}: {:?}",
            codes(source)
        );
    }
}

#[test]
fn public_values_reject_unsupported_results_without_ice() {
    {
        let expression = "'x'";
        let source = format!("func main(): void {{ let x = comptime {expression} }}");
        let errors = codes(&source);
        assert!(
            errors.contains(&DiagCode::T042UnsupportedComptime),
            "{source}: {errors:?}"
        );
        assert!(
            errors.iter().all(|code| !code.as_str().starts_with("ICE")),
            "{errors:?}"
        );
    }
}

#[test]
fn captures_read_write_and_alias_are_rejected_with_declaration_context() {
    for source in [
        "func main(input: int): int { return comptime input }",
        "func main(): int { let outside = 42; return comptime outside }",
        "func main(): int { let mut outside = 0; return comptime { set outside = 42; 42 } }",
        "func main(): bool { let outside = 42; return comptime { let alias = ref outside; true } }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("capture.aru".into(), source.into());
        let checked = type_check(&db, file);
        let capture = checked
            .diagnostics
            .iter()
            .find(|d| d.code == DiagCode::T043ComptimeRuntimeCapture)
            .expect("capture error");
        assert!(
            capture.primary_label.is_some() && !capture.labels.is_empty(),
            "{capture:?}"
        );
        assert!(
            capture.notes.iter().any(|note| note
                .contains("runtime local values and parameters do not exist during compilation")),
            "{capture:?}"
        );
        assert!(checked.type_info.ctfe_values.is_empty());
    }
}

#[test]
fn all_public_integer_types_preserve_full_range_and_global_constants_are_available() {
    for width in [4, 8] {
        let mut db = DatabaseImpl::new();
        let layout = DataLayout::ptr_width(width);
        db.set_target_config(layout);
        for primitive in [
            Primitive::Int,
            Primitive::Uint,
            Primitive::ISize,
            Primitive::USize,
            Primitive::I8,
            Primitive::I16,
            Primitive::I32,
            Primitive::I64,
            Primitive::U8,
            Primitive::U16,
            Primitive::U32,
            Primitive::U64,
            Primitive::Byte,
        ] {
            let ty = arandu_middle::ctfe::IntegerType::new(primitive, layout).expect("integer");
            for expected in [ty.min(), ty.max()] {
                let file = db.new_file(
                    "range.aru".into(),
                    format!(
                        "func main(): void {{ let value: {} = comptime {{ return {expected} }} }}",
                        primitive.as_str()
                    ),
                );
                let checked = type_check(&db, file);
                assert!(
                    checked
                        .diagnostics
                        .iter()
                        .all(|d| d.severity != Severity::Error),
                    "{:?}",
                    checked.diagnostics
                );
                let &(ConstValue::Integer(value), _) = checked
                    .type_info
                    .ctfe_values
                    .values()
                    .next()
                    .expect("materialized integer")
                else {
                    panic!("integer")
                };
                assert_eq!(value.ty(), ty);
                assert_eq!(value.value(), expected);
            }
        }
    }
    assert!(codes("const base = 20\nfunc main(): int { return comptime (base + 22) }").is_empty());
    assert!(
        codes("func generic<T>(input: T): int { return comptime 42 }\nfunc main(): int { return generic<int>(0) }")
            .is_empty()
    );
}

#[test]
fn arithmetic_failures_are_semantic_and_never_runtime_fallback() {
    for (body, code) in [
        (
            "{ let maximum: i8 = 127; maximum + 1 }",
            DiagCode::T046ComptimeArithmetic,
        ),
        (
            "{ let minimum: i64 = -9223372036854775808; -minimum }",
            DiagCode::T046ComptimeArithmetic,
        ),
        (
            "{ let zero: int = 0; 42 / zero }",
            DiagCode::T040DivisionByZero,
        ),
        (
            "{ let zero: int = 0; 42 % zero }",
            DiagCode::T040DivisionByZero,
        ),
        (
            "{ let shift: int = 64; 1 << shift }",
            DiagCode::T046ComptimeArithmetic,
        ),
    ] {
        let source = format!("func main(): void {{ let x = comptime {body} }}");
        assert!(
            codes(&source).contains(&code),
            "{source}: {:?}",
            codes(&source)
        );
        let mut db = DatabaseImpl::new();
        let file = db.new_file("invalid.aru".into(), source);
        assert!(lower_amir(&db, file).amir.funcs.is_empty());
    }
}

#[test]
fn runaway_evaluation_and_recursive_helpers_stop_at_a_limit() {
    for source in [
        "func main(): void { comptime { while true {} } }",
        "func recurse(): int { return recurse() }\nfunc main(): int { return comptime recurse() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("limit.aru".into(), source.into());
        let checked = type_check(&db, file);
        let diag = checked
            .diagnostics
            .iter()
            .find(|d| d.code == DiagCode::T045ComptimeLimitExceeded)
            .expect("limit exceeded");
        assert!(
            diag.hints.iter().any(|h| h
                .message
                .contains("check for infinite recursion or unbounded loops")),
            "{diag:?}"
        );
    }
}

#[test]
fn pointersized_values_recompute_for_the_target_not_the_host() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "target.aru".into(),
        "func main(): void { let x: usize = comptime 42 }".into(),
    );
    for width in [4, 8, 4] {
        db.set_target_config(DataLayout::ptr_width(width));
        let checked = type_check(&db, file);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let (ConstValue::Integer(value), layout) = checked
            .type_info
            .ctfe_values
            .values()
            .next()
            .expect("value")
        else {
            panic!("integer")
        };
        assert_eq!(value.ty().primitive(), Primitive::USize);
        assert_eq!(
            value.ty(),
            arandu_middle::ctfe::IntegerType::new(Primitive::USize, *layout).expect("target type")
        );
        assert_eq!(value.value(), 42);
    }
}

#[test]
fn staged_values_are_concrete_and_do_not_bypass_narrowing_checks() {
    for value in ["42", "300"] {
        let source = format!("func take(value: i8): void {{}}\nfunc main(): void {{ let x = comptime {value}; take(x) }}");
        assert!(
            !codes(&source).is_empty(),
            "a default int result cannot silently narrow to i8: {source}"
        );
    }
    assert!(
        codes("func take(value: i8): void {}\nfunc main(): void { take(comptime 42) }").is_empty()
    );
    assert!(
        codes("func take(value: i8): void {}\nfunc main(): void { take(comptime 300) }")
            .contains(&DiagCode::T038IntegerLiteralOutOfRange)
    );
}

#[test]
fn recursive_staging_helpers_stop_at_the_shared_vm_limit() {
    assert!(codes("func helper(): int { return comptime helper() }\nfunc main(): int { return comptime helper() }")
        .contains(&DiagCode::T045ComptimeLimitExceeded));
    assert!(codes("const answer = comptime 42\nfunc main(): int { return answer }").is_empty());
}

#[test]
fn editing_runtime_siblings_preserves_the_public_value_and_callee_edits_refresh_it() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "func helper(): int { return 42 }\nfunc main(): int { let x = comptime helper(); return x }\nfunc sibling(): int { return 10 }";
    let file = db.new_file("cutoff.aru".into(), source.into());
    let before = type_check(&db, file).type_info.ctfe_values.clone();
    log.clear();
    file.set_text(&mut db)
        .to(source.replace("return 10", "return 11").into());
    assert_eq!(before, type_check(&db, file).type_info.ctfe_values);
    assert_eq!(log.count_executions_matching("ctfe_eval_root"), 0);
    log.clear();
    file.set_text(&mut db)
        .to(source.replace("return 42", "return 43").into());
    let changed = type_check(&db, file);
    assert!(changed
        .type_info
        .ctfe_values
        .values()
        .any(|(value, _)| matches!(value, ConstValue::Integer(n) if n.value() == 43)));
    assert!(log.count_executions_matching("ctfe_eval_root") > 0);
}

#[test]
fn driver_limits_invalidate_every_public_staging_family_and_recover() {
    use arandu_query::ctfe::CtfeLimits;
    for source in [
        "func main(): int { return comptime (20 + 22) }",
        "const ANSWER int = comptime (20 + 22)\nfunc main(): int { return ANSWER }",
        "func main(): int { comptime if 1 + 1 == 2 { return 42 } else { return 0 } }",
        "func answer<comptime N: uint>(): uint { return N }\nfunc main(): int { return answer<comptime (20 + 22)>() as int }",
        "func main(): int { let a: [comptime (1 + 1)]int = [20, 22]; return a[0] + a[1] }",
        "func main(): int { comptime for i in 0..2 { let x = i }; return 42 }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("limits.aru".into(), source.into());
        assert!(arandu_query::passes::parse(&db, file).is_ok(), "{source}: {:?}", arandu_query::passes::parse(&db, file).as_ref().err());
        let before = arandu_query::db::HashEq::share(type_check(&db, file));
        assert!(before.diagnostics.iter().all(|d| d.severity != Severity::Error), "{source}: {:?}", before.diagnostics);
        let exports = std::sync::Arc::clone(arandu_query::passes::exported_symbols(&db, file));
        db.set_ctfe_limits(CtfeLimits::new(1, 1, 1).expect("positive limits"));
        if let Some(symbol) = before.symbols.iter().find(|symbol| symbol.name == "ANSWER") {
            let global = arandu_query::ctfe::global_const_value(&db, file, symbol.id);
            assert!(global.result.is_err(), "global result under limit: {:?}", global.result);
            assert!(!arandu_query::passes::declaration_signatures(&db, file).diagnostics.is_empty(), "global declaration errors must be retained");
        }
        let limited = type_check(&db, file);
        assert!(limited.diagnostics.iter().any(|d| d.code == DiagCode::T045ComptimeLimitExceeded), "{source}: {:?}", limited.diagnostics);
        assert_eq!(&exports, arandu_query::passes::exported_symbols(&db, file));
        db.set_ctfe_limits(CtfeLimits::default());
        assert!(&before == type_check(&db, file), "restored policy must recover deterministically");
    }
}

#[test]
fn changing_limits_advances_analysis_revision_and_rejects_zero_ceilings() {
    use arandu_query::ctfe::CtfeLimits;
    for limits in [(0, 1, 1), (1, 0, 1), (1, 1, 0)] {
        assert!(CtfeLimits::new(limits.0, limits.1, limits.2).is_err());
    }
    let mut host = arandu_query::AnalysisHost::new();
    let revision = host.revision();
    host.set_ctfe_limits(CtfeLimits::new(50, 4, 100).expect("positive limits"));
    assert_ne!(revision, host.revision());
    let snapshot = host.snapshot();
    assert_eq!(
        snapshot
            .db
            .ctfe_config()
            .expect("registered policy")
            .limits(&snapshot.db)
            .budget()
            .fuel,
        50
    );
}
