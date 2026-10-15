//! Pre-body condition evaluation, not public `comptime if` syntax/selection.
#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{ctfe::ConstValue, types::Primitive, DataLayout, DiagCode, SymbolId};
use arandu_mir::ctfe::{Budget, EvalErrorKind};
use arandu_query::{
    ctfe::{
        ctfe_eval_root, BuildFailure, CtfeRoot, CtfeRootRequest, RootEvalError, RootExpectedType,
        RootSelector,
    },
    passes, ArandCompilerDb, DatabaseImpl, SourceFile,
};
use salsa::Setter;

fn budget() -> Budget {
    Budget {
        fuel: 100_000,
        frames: 64,
        values: 10_000,
    }
}

fn owner(db: &DatabaseImpl, file: SourceFile) -> SymbolId {
    passes::local_symbols(db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "owner")
        .expect("owner")
        .id
}

fn request(db: &DatabaseImpl, file: SourceFile, budget: Budget) -> CtfeRootRequest<'_> {
    let root = CtfeRoot::new(
        db,
        file,
        owner(db, file),
        RootSelector::IfCondition {
            block: vec![],
            statement: 0,
        },
        None,
    );
    CtfeRootRequest::new(db, root, budget)
}

fn run(db: &DatabaseImpl, file: SourceFile) -> Result<ConstValue, RootEvalError> {
    ctfe_eval_root(db, request(db, file, budget())).clone()
}

fn no_body_queries(log: &arandu_query::RebuildLog) {
    for query in [
        "resolve(",
        "declaration_signatures",
        "declaration_hir",
        "item_typing",
        "item_staged_typing",
        "function_hir",
        "instance_hir",
        "borrow_interfaces",
        "lower_amir",
        "runtime_unit",
    ] {
        assert_eq!(
            log.count_executions_matching(query),
            0,
            "unexpected query: {query}: {:?}",
            log.snapshot()
        );
    }
}

#[test]
fn a_condition_and_recursive_helpers_do_not_visit_either_owner_branch() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("condition.aru".into(),
        "func owner(): int { if choose(3) { return missing_a } else { return missing_b } }\nfunc choose(n: int): bool { if n == 0 { return true } return choose(n - 1) }\nfunc unrelated(): int { return missing_c }".into());
    assert_eq!(run(&db, file), Ok(ConstValue::Bool(true)));
    assert!(log.count_executions_matching("ctfe_header_func_amir") > 0);
    no_body_queries(&log);
    assert!(
        passes::resolve(&db, file)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::N001UndefinedValue),
        "condition evaluation does not certify the owner"
    );
}

#[test]
fn imported_and_transitive_helpers_use_the_pre_body_boundary() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file(
        "leaf.aru".into(),
        "public func answer(): bool { return true }\nfunc unrelated(): int { return missing }"
            .into(),
    );
    db.new_file(
        "helper.aru".into(),
        "from leaf import { answer }\npublic func choose(): bool { return answer() }".into(),
    );
    let file = db.new_file("condition.aru".into(), "from helper import { choose }\nfunc owner(): int { if choose() { return unknown_a } else { return unknown_b } }".into());
    assert_eq!(run(&db, file), Ok(ConstValue::Bool(true)));
    no_body_queries(&log);
}

#[test]
fn global_constants_and_target_sized_arithmetic_are_available() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("condition.aru".into(), "const base = 42\nfunc owner(): int { if (base as usize) == (42 as usize) { return missing } }".into());
    for width in [4, 8] {
        db.target_config()
            .set_data_layout(&mut db)
            .to(DataLayout::ptr_width(width));
        assert_eq!(run(&db, file), Ok(ConstValue::Bool(true)));
    }
}

#[test]
fn helpers_can_use_global_constants_without_full_body_typing() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("condition.aru".into(),
        "const base = 42\nfunc choose(): bool { return base == 42 }\nfunc owner(): int { if choose() { return missing } }".into());
    assert_eq!(run(&db, file), Ok(ConstValue::Bool(true)));
    no_body_queries(&log);
}

#[test]
fn nonterminating_condition_helpers_stop_at_fuel_or_frame_limits() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("condition.aru".into(),
        "func choose(): bool { while true {} return true }\nfunc owner(): int { if choose() { return missing } }".into());
    let limited = Budget {
        fuel: 100,
        ..budget()
    };
    assert!(
        matches!(ctfe_eval_root(&db, request(&db, file, limited)), Err(RootEvalError::Evaluation(error)) if matches!(error.kind, EvalErrorKind::FuelExhausted))
    );
    file.set_text(&mut db).to("func choose(): bool { return choose() }\nfunc owner(): int { if choose() { return missing } }".into());
    let limited = Budget {
        frames: 4,
        ..budget()
    };
    assert!(
        matches!(ctfe_eval_root(&db, request(&db, file, limited)), Err(RootEvalError::Evaluation(error)) if matches!(error.kind, EvalErrorKind::FrameLimit))
    );
}

#[test]
fn conditions_require_bool_and_reject_runtime_parameters() {
    for (source, capture) in [
        ("func owner(): int { if 42 { return missing } }", false),
        (
            "func owner(input: bool): int { if input { return missing } }",
            true,
        ),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("condition.aru".into(), source.into());
        let error = run(&db, file).expect_err("invalid condition");
        if capture {
            assert!(
                matches!(error, RootEvalError::Build(BuildFailure::RuntimeCapture(_))),
                "{error:?}"
            );
        } else {
            assert!(
                matches!(error, RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics)) if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::T004IncompatibleReturnType)),
                "{error:?}"
            );
        }
    }
}

#[test]
fn condition_errors_and_import_cycles_fail_without_a_branch_decision() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file(
        "a.aru".into(),
        "from b import { choose_b }\npublic func choose(): bool { return choose_b() }".into(),
    );
    db.new_file(
        "b.aru".into(),
        "from a import { choose }\npublic func choose_b(): bool { return choose() }".into(),
    );
    let file = db.new_file(
        "condition.aru".into(),
        "from a import { choose }\nfunc owner(): int { if choose() { return missing } }".into(),
    );
    assert!(run(&db, file).is_err());
    no_body_queries(&log);
    file.set_text(&mut db)
        .to("func owner(): int { if unknown { return missing } }".into());
    assert!(
        matches!(run(&db, file), Err(RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics))) if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::N001UndefinedValue))
    );
}

#[salsa::tracked]
fn condition_consumer<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeRootRequest<'db>,
) -> Result<ConstValue, RootEvalError> {
    ctfe_eval_root(db, request).clone()
}

#[test]
fn branch_edits_cut_off_evaluation_and_helper_edits_update_the_decision() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "func owner(): int { if choose() { return missing_a } else { return missing_b } }\nfunc choose(): bool { return true }";
    let file = db.new_file("condition.aru".into(), source.into());
    assert_eq!(
        *condition_consumer(&db, request(&db, file, budget())),
        Ok(ConstValue::Bool(true))
    );
    log.clear();
    file.set_text(&mut db)
        .to(source.replace("missing_a", "unknown_a").into());
    assert_eq!(
        *condition_consumer(&db, request(&db, file, budget())),
        Ok(ConstValue::Bool(true))
    );
    assert_eq!(log.count_executions_matching("ctfe_eval_root"), 0);
    assert_eq!(log.count_executions_matching("condition_consumer"), 0);
    log.clear();
    let changed = source.replace("return true", "return false");
    file.set_text(&mut db).to(changed.clone().into());
    assert_eq!(
        *condition_consumer(&db, request(&db, file, budget())),
        Ok(ConstValue::Bool(false))
    );
    assert_eq!(log.count_executions_matching("condition_consumer"), 1);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("condition.aru".into(), changed);
    assert_eq!(run(&db, file), run(&clean, clean_file));
    no_body_queries(&log);
}

#[test]
fn sibling_arena_changes_and_error_recovery_match_clean_evaluation() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("condition.aru".into(), "func sibling(): int { return 1 }\nfunc owner(): int { if choose() { return missing } }\nfunc choose(): bool { return true }".into());
    assert_eq!(run(&db, file), Ok(ConstValue::Bool(true)));
    for source in [
        "func sibling(): int { return 1 + 2 + 3 }\nfunc owner(): int { if choose() { return missing } }\nfunc choose(): bool { return true }",
        "func sibling(): int { return 1 + 2 + 3 }\nfunc owner(): int { if unknown() { return missing } }\nfunc choose(): bool { return true }",
        "func sibling(): int { return 7 }\nfunc owner(): int { if choose() { return missing } }\nfunc choose(): bool { return false }",
    ] {
        file.set_text(&mut db).to(source.into());
        let mut clean = DatabaseImpl::new();
        let clean_file = clean.new_file("condition.aru".into(), source.into());
        assert_eq!(run(&db, file), run(&clean, clean_file));
    }
}

#[test]
fn budgets_cancellation_and_invalid_selectors_do_not_cache_a_decision() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "condition.aru".into(),
        "func owner(): int { if true { return missing } }".into(),
    );
    for limited in [
        Budget {
            fuel: 0,
            ..budget()
        },
        Budget {
            values: 0,
            ..budget()
        },
    ] {
        assert!(
            matches!(ctfe_eval_root(&db, request(&db, file, limited)), Err(RootEvalError::Evaluation(error)) if matches!(error.kind, EvalErrorKind::FuelExhausted | EvalErrorKind::ValueLimit))
        );
    }
    let root = CtfeRoot::new(
        &db,
        file,
        owner(&db, file),
        RootSelector::IfCondition {
            block: vec![],
            statement: u32::MAX,
        },
        None,
    );
    assert_eq!(
        *ctfe_eval_root(&db, CtfeRootRequest::new(&db, root, budget())),
        Err(RootEvalError::Build(BuildFailure::InvalidRoot))
    );
    let root = CtfeRoot::new(
        &db,
        file,
        owner(&db, file),
        RootSelector::IfCondition {
            block: vec![],
            statement: 0,
        },
        Some(RootExpectedType::Primitive(Primitive::Int)),
    );
    assert_eq!(
        *ctfe_eval_root(&db, CtfeRootRequest::new(&db, root, budget())),
        Err(RootEvalError::Build(BuildFailure::InvalidRoot))
    );
    let reader = db.clone();
    reader.query_cancellation_token().cancel();
    assert!(arandu_query::catch_query_cancellation(|| run(&reader, file)).is_err());
    drop(reader);
    assert_eq!(run(&db, file), Ok(ConstValue::Bool(true)));
}

#[test]
fn extern_calls_in_condition_helpers_are_rejected_even_on_untaken_paths() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("condition.aru".into(), "extern \"C\" { func external(): bool }\nfunc choose(): bool { if false { unsafe { return external() } } return true }\nfunc owner(): int { if choose() { return missing } }".into());
    assert!(
        matches!(run(&db, file), Err(RootEvalError::Evaluation(ref error)) if matches!(error.kind, EvalErrorKind::UnavailableFunction(_) | EvalErrorKind::UnsupportedOperation))
    );
}
