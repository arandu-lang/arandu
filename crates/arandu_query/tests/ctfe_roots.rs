//! Isolated initial typing -> AMIR -> evaluation without final runtime queries.
//! The source paths below exercise staging APIs, not public `comptime` syntax.
#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{ctfe::ConstValue, types::Primitive, DataLayout, DiagCode, SymbolId};
use arandu_mir::ctfe::{Budget, EvalErrorKind};
use arandu_query::{
    ctfe::{
        ctfe_eval_root, ctfe_root_amir, BlockBranch, BlockStep, BuildFailure, CtfeRoot,
        CtfeRootRequest, RootEvalError, RootExpectedType, RootSelector,
    },
    ArandCompilerDb, DatabaseImpl, SourceFile,
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
    arandu_query::passes::declaration_signatures(db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "owner")
        .expect("owner")
        .id
}

fn initializer() -> RootSelector {
    RootSelector::Initializer {
        block: vec![],
        statement: 0,
    }
}

fn branch() -> RootSelector {
    RootSelector::Block(vec![BlockStep {
        statement: 0,
        branch: BlockBranch::IfThen,
    }])
}

fn run(
    db: &DatabaseImpl,
    file: SourceFile,
    selector: RootSelector,
    expected: Option<RootExpectedType>,
    budget: Budget,
) -> Result<ConstValue, RootEvalError> {
    let root = CtfeRoot::new(db, file, owner(db, file), selector, expected);
    ctfe_eval_root(db, CtfeRootRequest::new(db, root, budget)).clone()
}

fn number(result: Result<ConstValue, RootEvalError>) -> i128 {
    let ConstValue::Integer(value) = result.expect("evaluation") else {
        panic!("integer")
    };
    value.value()
}

#[test]
fn signed_minimum_literal_is_one_value_not_an_out_of_range_intermediate() {
    use arandu_middle::ctfe::IntegerType;
    for width in [4, 8] {
        let layout = DataLayout::ptr_width(width);
        for primitive in [
            Primitive::Int,
            Primitive::I8,
            Primitive::I16,
            Primitive::I32,
            Primitive::I64,
            Primitive::ISize,
        ] {
            let ty = IntegerType::new(primitive, layout).expect("signed integer");
            for expression in [
                ty.min().to_string(),
                format!("-({})", -ty.min()),
                format!("-0x{:x}", -ty.min()),
            ] {
                let mut db = DatabaseImpl::new();
                db.set_target_config(layout);
                let file = db.new_file("minimum.aru".into(), format!("func owner(): void {{ let folded: {} = minimum() }}\nfunc minimum(): {} {{ return {expression} }}", primitive.as_str(), primitive.as_str()));
                let ConstValue::Integer(value) = run(
                    &db,
                    file,
                    initializer(),
                    Some(RootExpectedType::Primitive(primitive)),
                    budget(),
                )
                .expect("signed minimum literal") else {
                    panic!("integer")
                };
                assert_eq!(value.ty(), ty);
                assert_eq!(value.value(), ty.min());
            }
        }
    }
}

#[test]
fn negating_a_computed_signed_minimum_still_overflows() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("minimum.aru".into(), "func owner(): void { let folded = -minimum() }\nfunc minimum(): i64 { return -9223372036854775808 }".into());
    assert!(
        matches!(run(&db, file, initializer(), None, budget()), Err(RootEvalError::Evaluation(error)) if matches!(error.kind, EvalErrorKind::Arithmetic(arandu_mir::ctfe::ScalarEvalError::Overflow(_))))
    );
}

#[test]
fn an_initializer_is_typed_without_its_invalid_runtime_owner_or_siblings() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("root.aru".into(),
        "extern \"C\" { func external(): void }\nfunc add(a: int, b: int): int { return a + b }\nfunc owner(runtime: int): bool {\nlet folded = add(20, 22)\nunsafe { external() }\nreturn missing\n}\nfunc sibling(): int { return also_missing }".into());
    log.clear();
    assert_eq!(number(run(&db, file, initializer(), None, budget())), 42);
    assert_eq!(log.count_executions_matching("function_hir"), 0);
    assert_eq!(log.count_executions_matching("instance_hir"), 0);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    assert_eq!(log.count_executions_matching("borrow_interfaces"), 0);
    // Only the ordinary helper enters item typing; the invalid owner and
    // sibling remain outside the demanded root.
    assert_eq!(log.count_executions_matching("item_typing"), 1);
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 1);
}

#[test]
fn a_block_has_local_returns_loops_and_a_type_independent_of_its_owner() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("root.aru".into(),
        "func owner(): bool { if false {\nlet mut n = 0\nwhile n < 3 { n = n + 1 }\nreturn n + 39\n99\n}\nreturn missing }".into());
    assert_eq!(number(run(&db, file, branch(), None, budget())), 42);
}

#[test]
fn a_runtime_capture_is_rejected_even_if_it_has_a_literal_initializer() {
    for source in [
        "func owner(input: int): bool { let folded = input + 1\nreturn false }",
        "func owner(): bool { let outside = 41\nif false { outside + 1 }\nreturn false }",
        "func owner(): bool { let mut outside = 0\nif false { outside = 42 }\nreturn false }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("root.aru".into(), source.into());
        let selector = if source.contains("input:") {
            initializer()
        } else {
            RootSelector::Block(vec![BlockStep {
                statement: 1,
                branch: BlockBranch::IfThen,
            }])
        };
        let error = run(&db, file, selector, None, budget()).expect_err("capture");
        let RootEvalError::Build(BuildFailure::RuntimeCapture(capture)) = error else {
            panic!("capture failure: {error:?}")
        };
        assert_eq!(capture.symbol.file_id, *file.file_id(&db));
        assert!(capture.use_span.start > capture.declaration_span.start);
        assert!(capture.use_span.start < capture.use_span.end);
    }
}

#[test]
fn root_result_context_is_pool_independent_and_part_of_the_query_key() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): bool { if false { 256 }\nreturn false }".into(),
    );
    let error = run(
        &db,
        file,
        branch(),
        Some(RootExpectedType::Primitive(Primitive::U8)),
        budget(),
    )
    .expect_err("literal must fit");
    assert!(
        matches!(error, RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::T038IntegerLiteralOutOfRange))
    );
    assert_eq!(
        number(run(
            &db,
            file,
            branch(),
            Some(RootExpectedType::Primitive(Primitive::U64)),
            budget()
        )),
        256
    );
    assert_eq!(number(run(&db, file, branch(), None, budget())), 256);
}

#[test]
fn bare_returns_require_unit_and_do_not_escape_into_the_owner() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): bool { if false { return; }\nreturn missing }".into(),
    );
    assert_eq!(
        run(&db, file, branch(), Some(RootExpectedType::Unit), budget()).expect("unit"),
        ConstValue::Void
    );
    let error = run(
        &db,
        file,
        branch(),
        Some(RootExpectedType::Primitive(Primitive::Int)),
        budget(),
    )
    .expect_err("non-unit");
    assert!(
        matches!(error, RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::T004IncompatibleReturnType))
    );
}

#[test]
fn a_partial_return_needs_a_value_on_the_fallthrough_path() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): void { if false { if false { return 42 } } }".into(),
    );
    assert!(matches!(run(&db, file, branch(), None, budget()),
        Err(RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics)))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::T004IncompatibleReturnType)));
}

#[test]
fn selectors_are_bounded_checked_and_cannot_cross_source_identity() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): void { let x = 42 }".into(),
    );
    for selector in [
        RootSelector::Initializer {
            block: vec![],
            statement: u32::MAX,
        },
        RootSelector::Block(vec![BlockStep {
            statement: 0,
            branch: BlockBranch::IfThen,
        }]),
        RootSelector::Block(vec![
            BlockStep {
                statement: 0,
                branch: BlockBranch::LoopBody
            };
            65
        ]),
    ] {
        assert_eq!(
            run(&db, file, selector, None, budget()),
            Err(RootEvalError::Build(BuildFailure::InvalidRoot))
        );
    }
    let other = db.new_file("other.aru".into(), "func owner(): int { return 7 }".into());
    let root = CtfeRoot::new(&db, file, owner(&db, other), initializer(), None);
    assert!(matches!(
        ctfe_root_amir(&db, root).result,
        Err(BuildFailure::MissingFunction)
    ));
}

#[salsa::tracked]
fn root_consumer<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeRootRequest<'db>,
) -> Result<ConstValue, RootEvalError> {
    ctfe_eval_root(db, request).clone()
}

fn consume(db: &DatabaseImpl, file: SourceFile) -> Result<ConstValue, RootEvalError> {
    let root = CtfeRoot::new(db, file, owner(db, file), initializer(), None);
    root_consumer(db, CtfeRootRequest::new(db, root, budget())).clone()
}

fn source(helper: &str, owner_tail: &str) -> String {
    format!("func owner(): bool {{ let folded = helper()\n{owner_tail}\n}}\nfunc helper(): int {{ {helper} }}")
}

#[test]
fn editing_other_runtime_statements_cuts_off_before_evaluation() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("root.aru".into(), source("return 42", "return true"));
    assert_eq!(number(consume(&db, file)), 42);
    log.clear();
    file.set_text(&mut db)
        .to(source("return 42", "return nope").into());
    assert_eq!(number(consume(&db, file)), 42);
    assert_eq!(log.count_executions_matching("ctfe_eval_root"), 0);
    assert_eq!(log.count_executions_matching("root_consumer"), 0);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
}

#[test]
fn helper_edits_recompute_evaluation_but_equal_values_cut_off_consumers() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("root.aru".into(), source("return 42", "return true"));
    assert_eq!(number(consume(&db, file)), 42);
    log.clear();
    file.set_text(&mut db)
        .to(source("return 40 + 2", "return true").into());
    assert_eq!(number(consume(&db, file)), 42);
    assert_eq!(log.count_executions_matching("ctfe_eval_root"), 1);
    assert_eq!(log.count_executions_matching("root_consumer"), 0);
    log.clear();
    let changed = source("return 43", "return true");
    file.set_text(&mut db).to(changed.clone().into());
    assert_eq!(number(consume(&db, file)), 43);
    assert_eq!(log.count_executions_matching("root_consumer"), 1);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("root.aru".into(), changed);
    assert_eq!(consume(&db, file), consume(&clean, clean_file));
}

#[test]
fn moving_a_callee_span_can_revalidate_evaluation_but_not_equal_value_consumers() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("root.aru".into(), source("return 42", "return true"));
    assert_eq!(number(consume(&db, file)), 42);
    log.clear();
    // This longer runtime edit shifts the helper's real spans. Its AMIR may
    // need revalidation; successful value consumers still get early cutoff.
    file.set_text(&mut db)
        .to(source("return 42", "return missing").into());
    assert_eq!(number(consume(&db, file)), 42);
    assert_eq!(log.count_executions_matching("root_consumer"), 0);
}

#[test]
fn error_success_transitions_and_span_changes_match_a_clean_analysis() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): bool { let folded = missing\nreturn true }".into(),
    );
    let first = run(&db, file, initializer(), None, budget()).expect_err("resolution");
    assert!(
        matches!(first, RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::N001UndefinedValue))
    );
    file.set_text(&mut db)
        .to("func owner(): bool { let folded = 42\nreturn true }".into());
    assert_eq!(number(run(&db, file, initializer(), None, budget())), 42);
    let changed = "\n\nfunc owner(): bool { let folded = missing\nreturn true }";
    file.set_text(&mut db).to(changed.into());
    let result = run(&db, file, initializer(), None, budget());
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("root.aru".into(), changed.into());
    assert_eq!(
        result,
        run(&clean, clean_file, initializer(), None, budget())
    );
    assert_ne!(
        result.expect_err("resolution again"),
        first,
        "current spans"
    );
}

#[test]
fn root_fuel_and_values_limits_do_not_poison_a_different_budget() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): bool { let folded = 42\nreturn false }".into(),
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
            matches!(run(&db, file, initializer(), None, limited), Err(RootEvalError::Evaluation(error))
            if matches!(error.kind, EvalErrorKind::FuelExhausted | EvalErrorKind::ValueLimit))
        );
    }
    assert_eq!(number(run(&db, file, initializer(), None, budget())), 42);
}

#[test]
fn root_layout_is_explicit_and_constants_use_the_shared_declaration_context() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "const base = 42\nfunc owner(): void { if false { return base as usize } }".into(),
    );
    for width in [4, 8] {
        let target = db.target_config();
        target
            .set_data_layout(&mut db)
            .to(DataLayout::ptr_width(width));
        let ConstValue::Integer(value) =
            run(&db, file, branch(), None, budget()).expect("constant/layout")
        else {
            panic!("integer")
        };
        assert_eq!(value.value(), 42);
        assert_eq!(value.ty().primitive(), Primitive::USize);
        assert_eq!(u64::from(value.ty().bit_width()), width * 8);
    }
}

#[test]
fn expression_result_context_is_checked_not_just_an_inference_hint() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): void { let value = answer() }\nfunc answer(): int { return 42 }".into(),
    );
    assert!(matches!(run(&db, file, initializer(),
        Some(RootExpectedType::Primitive(Primitive::Bool)), budget()),
        Err(RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics)))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::T004IncompatibleReturnType)));
    assert_eq!(number(run(&db, file, initializer(), None, budget())), 42);
}

#[test]
fn an_unsupported_expected_type_cannot_silently_widen_the_value_domain() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): void { let value = true }".into(),
    );
    for primitive in [Primitive::Any, Primitive::Char] {
        assert!(matches!(run(&db, file, initializer(),
            Some(RootExpectedType::Primitive(primitive)), budget()),
            Err(RootEvalError::Build(BuildFailure::Evaluation(EvalErrorKind::Value(
                arandu_middle::ctfe::ConstValueError::UnsupportedIntegerType(found)))))
            if found == primitive));
    }
    for primitive in [Primitive::Str, Primitive::Float] {
        assert!(matches!(run(&db, file, initializer(),
            Some(RootExpectedType::Primitive(primitive)), budget()),
            Err(RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics)))
            if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::T004IncompatibleReturnType)));
    }
    assert_eq!(
        run(&db, file, initializer(), None, budget()).expect("boolean"),
        ConstValue::Bool(true)
    );
}

#[test]
fn a_root_call_to_its_owner_uses_the_real_callable_definition() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): int { if false { let folded = owner() + 41 }\nreturn 1 }".into(),
    );
    let selector = RootSelector::Initializer {
        block: vec![BlockStep {
            statement: 0,
            branch: BlockBranch::IfThen,
        }],
        statement: 0,
    };
    assert_eq!(number(run(&db, file, selector, None, budget())), 42);
}

#[test]
fn cancelled_root_queries_unwind_without_memoizing_a_language_failure() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): bool { let folded = 42\nreturn false }".into(),
    );
    let reader = db.clone();
    reader.query_cancellation_token().cancel();
    assert!(arandu_query::catch_query_cancellation(|| run(
        &reader,
        file,
        initializer(),
        None,
        budget()
    ))
    .is_err());
    drop(reader);
    assert_eq!(number(run(&db, file, initializer(), None, budget())), 42);
}

#[test]
fn a_prohibited_untaken_helper_is_rejected_before_root_execution() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("root.aru".into(),
        "extern \"C\" { func external(): int }\nfunc bad(): int { unsafe { return external() } }\nfunc owner(): bool { if false { if false { bad() }\n42 }\nreturn false }".into());
    assert!(matches!(run(&db, file, branch(), None, budget()),
        Err(RootEvalError::Evaluation(ref error))
        if matches!(error.kind, EvalErrorKind::UnavailableFunction(_) | EvalErrorKind::UnsupportedOperation)));
}

#[test]
fn shadowed_local_belongs_to_the_root_frame_not_the_outer_binding() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): bool { let n = 7\nif false { let n = 41\nn + 1 }\nreturn false }".into(),
    );
    let selector = RootSelector::Block(vec![BlockStep {
        statement: 1,
        branch: BlockBranch::IfThen,
    }]);
    assert_eq!(number(run(&db, file, selector, None, budget())), 42);
}

#[test]
fn selecting_a_complete_body_still_uses_an_independent_return_target() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("root.aru".into(), "func owner(): bool { return 42 }".into());
    assert_eq!(
        number(run(&db, file, RootSelector::Block(vec![]), None, budget())),
        42
    );
}

#[test]
fn a_selected_block_cannot_break_out_of_an_enclosing_runtime_loop() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "root.aru".into(),
        "func owner(): void { while false { if false { break } } }".into(),
    );
    let selector = RootSelector::Block(vec![
        BlockStep {
            statement: 0,
            branch: BlockBranch::LoopBody,
        },
        BlockStep {
            statement: 0,
            branch: BlockBranch::IfThen,
        },
    ]);
    assert!(matches!(run(&db, file, selector, None, budget()),
        Err(RootEvalError::Build(BuildFailure::Diagnostics(ref diagnostics)))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == DiagCode::N011BreakContinueOutsideLoop)));
}

#[test]
fn imported_helpers_are_tracked_without_importing_unrelated_runtime_bodies() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file("math.aru".into(),
        "module math\npublic func answer(): int { return 42 }\nfunc sibling(): int { return missing }".into());
    let file = db.new_file(
        "root.aru".into(),
        "import math\nfunc owner(): bool { let folded = math.answer()\nreturn also_missing }"
            .into(),
    );
    log.clear();
    assert_eq!(number(run(&db, file, initializer(), None, budget())), 42);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    assert_eq!(log.count_executions_matching("function_hir"), 0);
    assert_eq!(log.count_executions_matching("borrow_interfaces"), 0);
}
