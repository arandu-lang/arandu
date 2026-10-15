//! Internal staging proofs, not examples of public `comptime` grammar. The
//! selected block comes from the canonical parser and is checked/lowered without
//! typing or executing the containing runtime function.
#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{DataLayout, Severity, SymbolId, hir::HirDecl, types::ArType};
use arandu_mir::ctfe::{Budget, CtfeFunction, EvalErrorKind, FunctionProvider};
use arandu_parser::{Program, Stmt, TopLevelDecl};
use arandu_semantics::{TargetInfo, TypeCheckResult, TypeChecker};
use arandu_typeck::type_checker::check::{CtfeBlockType, check_ctfe_block};
use std::sync::Arc;

struct NoCalls;
impl FunctionProvider for NoCalls {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        Err(EvalErrorKind::MissingFunction(symbol))
    }
}

fn source(body: &str) -> String {
    format!("func owner(): bool {{\nif false {{\n{body}\n}}\nreturn true\n}}")
}

fn block(program: &Program) -> &arandu_parser::Block {
    let TopLevelDecl::Func(function) = program.pool.decl(program.decls[0]) else {
        panic!("owner function")
    };
    let Stmt::If { then_block, .. } = program
        .pool
        .stmt(program.pool.stmt_list(function.body.statements)[0])
    else {
        panic!("isolated block")
    };
    then_block
}

fn typed(program: &Program, expected: Option<ArType>) -> (TypeCheckResult, CtfeBlockType) {
    let resolution = arandu_semantics::resolve_for_test(0, program);
    let mut checker = TypeChecker::new(
        resolution.symbols,
        resolution.resolved,
        resolution.diagnostics,
        &program.pool,
        TargetInfo { pointer_width: 64 },
    );
    arandu_semantics::check_signatures(&mut checker, program);
    // The containing function/loop must not constrain or absorb root returns.
    let outer = checker.intern(ArType::Primitive(arandu_middle::types::Primitive::Bool));
    checker.ctx.push_return(outer, program.span);
    checker.ctx.enter_loop();
    checker.current_observed_effects = arandu_middle::EffectFlags::THREAD;
    let expected = expected.map(|ty| checker.intern(ty));
    let result = check_ctfe_block(&mut checker, block(program), expected);
    assert_eq!(
        checker.current_observed_effects,
        arandu_middle::EffectFlags::THREAD
    );
    assert_eq!(checker.ctx.current_return(), Some(outer));
    assert!(checker.ctx.is_in_loop(), "outer loop restored");
    checker.ctx.exit_loop();
    checker.ctx.pop_return();
    checker.finalize_literal_vars();
    (checker.finish(), result)
}

fn lowered(
    body: &str,
) -> Result<(TypeCheckResult, arandu_mir::FunctionUnit), Vec<arandu_middle::Diagnostic>> {
    let program = arandu_parser::parse(&source(body)).expect("canonical parse");
    let (mut tc, ty) = typed(&program, None);
    assert!(
        !tc.diagnostics.iter().any(|d| d.severity == Severity::Error),
        "{:?}",
        tc.diagnostics
    );
    let mut hir = arandu_semantics::lower_declarations_to_hir(&mut tc, &program)?;
    let block =
        arandu_semantics::lower_block_to_hir(&mut tc, &program.pool, &mut hir, block(&program))?;
    let owner = hir
        .decls
        .iter()
        .find_map(|&id| match hir.pool.decl(id) {
            HirDecl::Func(function) => Some(function),
            _ => None,
        })
        .expect("owner header");
    assert!(owner.body.is_none(), "runtime body not lowered");
    let unit = arandu_mir::lower_block_unit(
        &tc,
        &hir,
        owner,
        block,
        ty.return_type,
        ty.value_tail.is_some(),
        DataLayout::ptr_width(8),
    )?;
    assert!(unit.function.params.is_empty());
    assert_eq!(unit.function.temps[0].span, hir.pool.block(block).span);
    Ok((tc, unit))
}

fn evaluate(body: &str) -> arandu_middle::ctfe::ConstValue {
    let (tc, unit) = lowered(body).expect("isolated AMIR");
    let unit = CtfeFunction::new(
        Arc::unwrap_or_clone(unit.function),
        unit.literals,
        &tc.type_info.type_interner,
        DataLayout::ptr_width(8),
    )
    .expect("scalar block");
    arandu_mir::ctfe::evaluate_unit(
        &NoCalls,
        Arc::new(unit),
        &[],
        Budget {
            fuel: 10_000,
            frames: 16,
            values: 1_000,
        },
        || false,
    )
    .expect("isolated evaluation")
}

fn number(value: arandu_middle::ctfe::ConstValue) -> i128 {
    let arandu_middle::ctfe::ConstValue::Integer(integer) = value else {
        panic!("integer")
    };
    integer.value()
}

#[test]
fn explicit_return_ends_only_the_isolated_evaluation() {
    assert_eq!(number(evaluate("return 42\n99")), 42);
    assert_eq!(number(evaluate("return 42")), 42);
    assert_eq!(number(evaluate("if true { return 42 }\n99")), 42);
    assert_eq!(number(evaluate("if false { return 99 }\n42")), 42);
}

#[test]
fn returns_in_branches_and_loops_share_the_root_type() {
    assert_eq!(
        number(evaluate("if true { return 42 } else { return 99 }")),
        42
    );
    assert_eq!(
        number(evaluate(
            "let mut n = 0\nwhile n < 3 { n = n + 1\nif n == 3 { return 42 } }\n99"
        )),
        42
    );
    assert_eq!(
        number(evaluate(
            "let mut n = 0\nwhile n < 5 { n = n + 1\nif n < 3 { continue }\nbreak }\nn + 39"
        )),
        42
    );
}

#[test]
fn bare_return_and_empty_block_yield_unit() {
    use arandu_middle::ctfe::ConstValue;
    assert_eq!(evaluate("return"), ConstValue::Void);
    assert_eq!(evaluate(""), ConstValue::Void);
    assert_eq!(evaluate("42;"), ConstValue::Void);
    assert_eq!(evaluate("if false { return }"), ConstValue::Void);
}

#[test]
fn unreachable_tail_and_all_explicit_returns_are_checked() {
    for body in [
        "return 42\ntrue",
        "return;\n42",
        "if true { return 42 } else { return false }\n42",
    ] {
        let program = arandu_parser::parse(&source(body)).expect("parse");
        let (tc, _) = typed(&program, None);
        assert!(
            tc.diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::T004IncompatibleReturnType),
            "{body}: {:?}",
            tc.diagnostics
        );
    }
}

#[test]
fn expected_type_checks_empty_bare_and_semicolon_results() {
    for body in ["", "return", "42;"] {
        let program = arandu_parser::parse(&source(body)).expect("parse");
        let (tc, _) = typed(
            &program,
            Some(ArType::Primitive(arandu_middle::types::Primitive::Int)),
        );
        assert!(
            tc.diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::T004IncompatibleReturnType),
            "{body}: {:?}",
            tc.diagnostics
        );
    }
}

#[test]
fn concrete_result_constrains_literal_returns_and_rejects_out_of_range() {
    use arandu_middle::types::Primitive;
    for body in ["return 256", "if true { return 256 }\n42", "256"] {
        let program = arandu_parser::parse(&source(body)).expect("parse");
        let (tc, ty) = typed(&program, Some(ArType::Primitive(Primitive::U8)));
        assert_eq!(
            tc.type_info.type_interner.resolve(ty.return_type),
            ArType::Primitive(Primitive::U8)
        );
        assert!(
            tc.diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::T038IntegerLiteralOutOfRange),
            "{body}: {:?}",
            tc.diagnostics
        );
    }
    assert_eq!(number(evaluate("if true { return 42 }\n99 as u64")), 42);
}

#[test]
fn loop_exits_cannot_cross_the_isolated_root_boundary() {
    for body in ["break", "continue"] {
        let program = arandu_parser::parse(&source(body)).expect("parse");
        let (tc, _) = typed(&program, None);
        assert!(
            tc.diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::N011BreakContinueOutsideLoop)
        );
    }
}

#[test]
fn a_value_return_does_not_mask_a_unit_fallthrough_path() {
    for body in ["if false { return 42 }", "while false { return 42 }"] {
        let diagnostics = lowered(body).expect_err("a fallthrough exit needs a value");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::T004IncompatibleReturnType),
            "{body}: {diagnostics:?}"
        );
    }
}

#[test]
fn block_diagnostics_do_not_recommend_changing_the_enclosing_signature() {
    let program = arandu_parser::parse(&source("return;\n42")).expect("parse");
    let (tc, _) = typed(&program, None);
    let mismatch = tc
        .diagnostics
        .iter()
        .find(|d| d.code == arandu_middle::DiagCode::T004IncompatibleReturnType)
        .expect("block type mismatch");
    assert!(mismatch.primary_label.is_some());
    assert!(
        mismatch
            .labels
            .iter()
            .any(|label| label.span == block(&program).span)
    );
    assert!(
        mismatch
            .hints
            .iter()
            .all(|hint| !hint.message.contains("function's return type"))
    );
}

#[test]
fn initial_effects_belong_to_the_evaluation_not_the_runtime_owner() {
    let program = arandu_parser::parse(&format!(
        "{}\n@Effects(Heap)\nfunc allocates(): void {{}}",
        source("allocates()\nreturn;")
    ))
    .expect("parse");
    let (tc, ty) = typed(&program, None);
    assert!(
        !tc.diagnostics.iter().any(|d| d.severity == Severity::Error),
        "{:?}",
        tc.diagnostics
    );
    assert!(
        ty.observed_effects
            .contains(arandu_middle::EffectFlags::HEAP)
    );
    assert!(
        !ty.observed_effects
            .contains(arandu_middle::EffectFlags::THREAD)
    );
}
