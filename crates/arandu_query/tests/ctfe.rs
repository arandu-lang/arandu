#![allow(clippy::expect_used)]

use arandu_middle::ctfe::{ConstInt, ConstValue, IntegerType};
use arandu_middle::types::Primitive;
use arandu_middle::{DataLayout, SymbolId};
use arandu_mir::ctfe::{Budget, CtfeFunction, EvalError, EvalErrorKind, FunctionProvider};
use arandu_query::ctfe::{ctfe_eval, ctfe_func_amir, BuildFailure, CtfeRequest};
use arandu_query::{ArandCompilerDb, DatabaseImpl, SourceFile};
use salsa::Setter;
use std::sync::Arc;

fn budget() -> Budget {
    Budget {
        fuel: 100_000,
        frames: 64,
        values: 10_000,
    }
}

fn symbol(db: &DatabaseImpl, file: SourceFile, name: &str) -> SymbolId {
    arandu_query::passes::declaration_signatures(db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .expect("function symbol")
        .id
}

fn run(
    db: &DatabaseImpl,
    file: SourceFile,
    symbol: SymbolId,
    arguments: Vec<ConstValue>,
    budget: Budget,
) -> Result<ConstValue, EvalError> {
    ctfe_eval(db, CtfeRequest::new(db, file, symbol, arguments, budget)).clone()
}

fn number(value: ConstValue) -> i128 {
    let ConstValue::Integer(value) = value else {
        panic!("expected integer")
    };
    value.value()
}

struct QueryUnits<'a>(&'a DatabaseImpl);

impl FunctionProvider for QueryUnits<'_> {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        let file = arandu_middle::db::SourceDatabase::source_file_by_id(self.0, symbol.file_id)
            .ok_or(EvalErrorKind::MissingFunction(symbol))?;
        ctfe_func_amir(self.0, file, symbol)
            .result
            .as_ref()
            .map(Arc::clone)
            .map_err(|_| EvalErrorKind::UnavailableFunction(symbol))
    }
}

fn expression_root(db: &DatabaseImpl, file: SourceFile, owner: SymbolId) -> Arc<CtfeFunction> {
    use arandu_middle::hir::{HirDecl, HirStmtKind};
    let prepared = arandu_query::runtime::function_hir(db, file, owner);
    let hir = prepared.hir.as_ref().expect("typed owner HIR");
    let function = hir
        .decls
        .iter()
        .find_map(|&id| match hir.pool.decl(id) {
            HirDecl::Func(function) if function.symbol == owner => Some(function),
            _ => None,
        })
        .expect("owner function");
    let expression = hir
        .pool
        .stmts
        .iter()
        .find_map(|statement| match statement.kind {
            HirStmtKind::VarDecl { value, .. } => Some(value),
            _ => None,
        })
        .expect("root initializer");
    let layout = *db.target_config().data_layout(db);
    let root =
        arandu_mir::lower_expression_unit(&prepared.type_check, hir, function, expression, layout)
            .expect("isolated expression lowering");
    assert!(
        root.function.params.is_empty(),
        "runtime parameters are not root arguments"
    );
    assert_eq!(root.function.temps[0].span, hir.pool.expr(expression).span);
    Arc::new(
        CtfeFunction::new(
            Arc::unwrap_or_clone(root.function),
            root.literals,
            &prepared.type_check.type_info.type_interner,
            layout,
        )
        .expect("scalar root"),
    )
}

#[test]
fn isolated_expression_uses_its_own_type_without_running_other_owner_statements() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(),
        "extern \"C\" { func external(): void }\nfunc add(a: int, b: int): int { return a + b }\nfunc owner(runtime: int): bool {\nlet folded = add(20, 22)\nunsafe { external() }\nreturn false\n}".into());
    let owner = symbol(&db, file, "owner");
    let result = arandu_mir::ctfe::evaluate_unit(
        &QueryUnits(&db),
        expression_root(&db, file, owner),
        &[],
        budget(),
        || false,
    )
    .expect("expression evaluation");
    assert_eq!(number(result), 42);
}

#[test]
fn isolated_expression_cannot_capture_a_runtime_parameter() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func owner(runtime: int): int {\nlet folded = runtime + 1\nreturn 0\n}".into(),
    );
    let owner = symbol(&db, file, "owner");
    let error = arandu_mir::ctfe::evaluate_unit(
        &QueryUnits(&db),
        expression_root(&db, file, owner),
        &[],
        budget(),
        || false,
    )
    .expect_err("runtime capture rejected");
    assert_eq!(error.kind, EvalErrorKind::UnsupportedOperation);
}

#[test]
fn isolated_expression_calling_its_owner_resolves_the_real_function() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func owner(): int {\nif false { let folded = owner() + 41 }\nreturn 1\n}".into(),
    );
    let owner = symbol(&db, file, "owner");
    let result = arandu_mir::ctfe::evaluate_unit(
        &QueryUnits(&db),
        expression_root(&db, file, owner),
        &[],
        budget(),
        || false,
    )
    .expect("owner is not the expression root");
    assert_eq!(number(result), 42);
}

fn source(helper: &str, unused: &str) -> String {
    format!("func foo(): int {{ return add(20, 22) }}\nfunc add(a: int, b: int): int {{ {helper} }}\nfunc unused(): int {{ {unused} }}\n")
}

#[test]
fn evaluates_a_real_call_without_whole_program_lowering_or_sibling_typeck() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("ctfe.aru".into(), source("return a + b", "return missing"));
    let foo = symbol(&db, file, "foo");
    log.clear();
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("CTFE call")),
        42
    );
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 2);
    assert_eq!(log.count_executions_matching("item_typing"), 2);
    assert_eq!(log.count_executions_matching("item_body_typeck"), 0);
    let lowered = ctfe_func_amir(&db, file, foo);
    assert_eq!(
        lowered.result.as_ref().expect("unit").function().symbol,
        foo
    );
}

#[test]
fn sibling_body_edit_does_not_relower_or_reevaluate_the_entry() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("ctfe.aru".into(), source("return a + b", "return 1"));
    let foo = symbol(&db, file, "foo");
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("initial")),
        42
    );
    log.clear();
    file.set_text(&mut db)
        .to(Arc::from(source("return a + b", "return 999")));
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("after edit")),
        42
    );
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 0);
    assert_eq!(log.count_executions_matching("ctfe_eval"), 0);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
}

#[salsa::tracked]
fn consumer<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeRequest<'db>,
) -> Result<ConstValue, EvalError> {
    ctfe_eval(db, request).clone()
}

fn consume(db: &DatabaseImpl, file: SourceFile, symbol: SymbolId) -> Result<ConstValue, EvalError> {
    consumer(db, CtfeRequest::new(db, file, symbol, vec![], budget())).clone()
}

#[test]
fn helper_body_changes_recompute_only_that_unit_and_cut_off_equal_values() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("ctfe.aru".into(), source("return a + b", "return 1"));
    let foo = symbol(&db, file, "foo");
    assert_eq!(number(consume(&db, file, foo).expect("initial")), 42);
    log.clear();
    file.set_text(&mut db)
        .to(Arc::from(source("return a + b + 0", "return 1")));
    assert_eq!(number(consume(&db, file, foo).expect("equal result")), 42);
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 1);
    assert_eq!(log.count_executions_matching("ctfe_eval"), 1);
    assert_eq!(log.count_executions_matching("consumer"), 0);
    log.clear();
    file.set_text(&mut db)
        .to(Arc::from(source("return a + b + 1", "return 1")));
    assert_eq!(number(consume(&db, file, foo).expect("changed result")), 43);
    assert_eq!(log.count_executions_matching("consumer"), 1);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("ctfe.aru".into(), source("return a + b + 1", "return 1"));
    assert_eq!(
        consume(&db, file, foo),
        run(
            &clean,
            clean_file,
            symbol(&clean, clean_file, "foo"),
            vec![],
            budget()
        )
    );
}

#[test]
fn loops_and_recursive_calls_run_on_bounded_frames() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("ctfe.aru".into(), "func sum(): int { let mut i = 0\n let mut total = 0\n while i < 10 { total = total + i\n i = i + 1 }\n return total }\nfunc recurse(n: int): int { if n == 0 { return 7 }\n return recurse(n - 1) }".into());
    assert_eq!(
        number(run(&db, file, symbol(&db, file, "sum"), vec![], budget()).expect("loop")),
        45
    );
    let int = IntegerType::new(Primitive::Int, DataLayout::ptr_width(8)).expect("int");
    let argument = ConstValue::Integer(ConstInt::new(int, 20).expect("argument"));
    let recurse = symbol(&db, file, "recurse");
    assert_eq!(
        number(run(&db, file, recurse, vec![argument.clone()], budget()).expect("recursive call")),
        7
    );
    let mut limited = budget();
    limited.frames = 4;
    assert_eq!(
        run(&db, file, recurse, vec![argument], limited)
            .expect_err("frame bound")
            .kind,
        EvalErrorKind::FrameLimit
    );
}

#[test]
fn fuel_and_values_are_limits_not_fallback_results_and_policy_is_in_the_key() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "ctfe.aru".into(),
        "func foo(): int { return 42 }\nfunc endless(): int { while true {}\n return 1 }".into(),
    );
    let foo = symbol(&db, file, "foo");
    let mut limited = budget();
    limited.fuel = 0;
    assert_eq!(
        run(&db, file, foo, vec![], limited).expect_err("fuel").kind,
        EvalErrorKind::FuelExhausted
    );
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("not poisoned")),
        42
    );
    limited = budget();
    limited.values = 0;
    assert_eq!(
        run(&db, file, foo, vec![], limited)
            .expect_err("values")
            .kind,
        EvalErrorKind::ValueLimit
    );
    limited = budget();
    limited.fuel = 200;
    assert_eq!(
        run(&db, file, symbol(&db, file, "endless"), vec![], limited)
            .expect_err("loop bound")
            .kind,
        EvalErrorKind::FuelExhausted
    );
}

struct One(Arc<CtfeFunction>);
impl FunctionProvider for One {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        if self.0.function().symbol != symbol {
            return Err(EvalErrorKind::MissingFunction(symbol));
        }
        Ok(Arc::clone(&self.0))
    }
}

#[test]
fn cancellation_interrupts_an_infinite_loop_and_does_not_poison_a_later_run() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "ctfe.aru".into(),
        "func foo(): int { while true {}\n return 1 }".into(),
    );
    let foo = symbol(&db, file, "foo");
    let unit = ctfe_func_amir(&db, file, foo)
        .result
        .as_ref()
        .expect("unit")
        .clone();
    let mut polls = 0;
    let result = arandu_mir::ctfe::evaluate(&One(unit), foo, &[], budget(), || {
        polls += 1;
        polls >= 50
    });
    assert_eq!(
        result.expect_err("cancelled").kind,
        EvalErrorKind::Cancelled
    );
    assert_eq!(polls, 50);
    let mut limited = budget();
    limited.fuel = 100;
    assert_eq!(
        run(&db, file, foo, vec![], limited)
            .expect_err("ordinary fuel error")
            .kind,
        EvalErrorKind::FuelExhausted
    );
}

#[test]
fn full_u64_and_target_sized_values_do_not_use_the_host_width() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(8));
    let file = db.new_file("ctfe.aru".into(), "func wide(): u64 { return 18446744073709551615 }\nfunc sized(): usize { return 4294967296 }".into());
    assert_eq!(
        number(run(&db, file, symbol(&db, file, "wide"), vec![], budget()).expect("u64")),
        i128::from(u64::MAX)
    );
    let sized = symbol(&db, file, "sized");
    assert_eq!(
        number(run(&db, file, sized, vec![], budget()).expect("usize64")),
        1_i128 << 32
    );
    db.set_target_config(DataLayout::ptr_width(4));
    assert!(run(&db, file, sized, vec![], budget()).is_err());
    assert_eq!(
        number(run(&db, file, symbol(&db, file, "wide"), vec![], budget()).expect("u64 on ptr4")),
        i128::from(u64::MAX)
    );
}

#[test]
fn unsupported_generics_and_external_calls_never_get_a_dummy_body() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("ctfe.aru".into(), "func identity<T>(value: T): T { return value }\nextern \"C\" { func external(): int }\nfunc foo(): int { return external() }".into());
    let identity = symbol(&db, file, "identity");
    assert!(matches!(
        ctfe_func_amir(&db, file, identity).result,
        Err(BuildFailure::GenericFunction)
    ));
    assert!(matches!(
        run(&db, file, symbol(&db, file, "foo"), vec![], budget())
            .expect_err("external")
            .kind,
        EvalErrorKind::UnavailableFunction(_) | EvalErrorKind::MissingFunction(_)
    ));
}

#[test]
fn untaken_helpers_are_dependencies_and_effect_changes_invalidate_evaluation() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = |body: &str| {
        format!(
        "extern \"C\" {{ func external(): int }}\nfunc foo(): int {{ if false {{ return helper() }}\n return 42 }}\nfunc helper(): int {{ {body} }}\nfunc sibling(): int {{ return missing }}\n"
    )
    };
    let file = db.new_file("ctfe.aru".into(), source("return 9"));
    let foo = symbol(&db, file, "foo");
    assert_eq!(number(consume(&db, file, foo).expect("pure closure")), 42);
    log.clear();
    file.set_text(&mut db)
        .to(Arc::from(source("unsafe { return external() }")));
    let error = consume(&db, file, foo).expect_err("untaken transitive extern");
    assert!(matches!(
        error.kind,
        EvalErrorKind::UnavailableFunction(_) | EvalErrorKind::MissingFunction(_)
    ));
    assert_eq!(error.trace.len(), 1);
    assert_eq!(error.trace[0].function, foo);
    assert_eq!(log.count_executions_matching("ctfe_eval"), 1);
    assert_eq!(log.count_executions_matching("consumer"), 1);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    file.set_text(&mut db).to(Arc::from(source("return 10")));
    assert_eq!(
        number(consume(&db, file, foo).expect("recovered closure")),
        42
    );
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("ctfe.aru".into(), source("return 10"));
    assert_eq!(
        consume(&db, file, foo),
        consume(&clean, clean_file, symbol(&clean, clean_file, "foo"))
    );
}

#[test]
fn changes_to_untaken_pure_helpers_cut_off_equal_result_consumers() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = |value| {
        format!("func foo(): int {{ if false {{ return helper() }}\n return 42 }}\nfunc helper(): int {{ return {value} }}")
    };
    let file = db.new_file("ctfe.aru".into(), source(1));
    let foo = symbol(&db, file, "foo");
    assert_eq!(number(consume(&db, file, foo).expect("initial")), 42);
    log.clear();
    file.set_text(&mut db).to(Arc::from(source(2)));
    assert_eq!(number(consume(&db, file, foo).expect("equal result")), 42);
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 1);
    assert_eq!(log.count_executions_matching("ctfe_eval"), 1);
    assert_eq!(log.count_executions_matching("consumer"), 0);
}

#[test]
fn inconsistent_layouts_are_rejected_and_do_not_poison_valid_target_results() {
    use arandu_middle::layout::{DataLayoutError, SizeAlign};
    let mut db = DatabaseImpl::new();
    let file = db.new_file("ctfe.aru".into(), "func foo(): int { return 42 }".into());
    let foo = symbol(&db, file, "foo");
    let valid = DataLayout::ptr_width(8);
    for invalid in [
        DataLayout {
            pointer: SizeAlign::natural(2),
            ..valid
        },
        DataLayout {
            i64: SizeAlign::new(8, 0),
            ..valid
        },
        DataLayout {
            float: SizeAlign::new(8, 4),
            ..valid
        },
    ] {
        db.set_target_config(invalid);
        assert!(matches!(
            ctfe_func_amir(&db, file, foo).result,
            Err(BuildFailure::Evaluation(EvalErrorKind::InvalidLayout(
                DataLayoutError::PointerWidth
                    | DataLayoutError::Alignment
                    | DataLayoutError::FloatLayout
            )))
        ));
        assert!(run(&db, file, foo, vec![], budget()).is_err());
    }
    db.set_target_config(DataLayout::i686_sysv());
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("valid i686 layout")),
        42
    );
}

fn run_instance(
    db: &DatabaseImpl,
    file: SourceFile,
    definition: SymbolId,
    types: Vec<arandu_middle::types::TypeShape>,
    arguments: Vec<ConstValue>,
) -> Result<ConstValue, EvalError> {
    use arandu_query::ctfe::{ctfe_eval_instance, CtfeInstanceRequest};
    let key = arandu_middle::types::FunctionInstance {
        definition,
        arguments: types,
    };
    let instance = arandu_query::runtime::Instance::new(db, file, key);
    ctfe_eval_instance(
        db,
        CtfeInstanceRequest::new(db, instance, arguments, budget()),
    )
    .clone()
}

#[test]
fn concrete_scalar_type_and_value_parameters_reuse_the_existing_monomorphizer() {
    use arandu_middle::types::TypeShape;
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("generic-ctfe.aru".into(), "func identity<T>(value: T): T { return value }\nfunc count<const N: uint>(): uint { return N }\nfunc caller(): int { return identity<int>(42) }".into());
    let identity = symbol(&db, file, "identity");
    let count = symbol(&db, file, "count");
    let int = IntegerType::new(Primitive::Int, DataLayout::host()).expect("int");
    let wide = IntegerType::new(Primitive::U64, DataLayout::host()).expect("u64");
    log.clear();
    assert_eq!(
        number(
            run_instance(
                &db,
                file,
                identity,
                vec![TypeShape::Primitive(Primitive::Int)],
                vec![ConstValue::Integer(ConstInt::new(int, 42).expect("int"))]
            )
            .expect("integer identity")
        ),
        42
    );
    assert_eq!(
        number(
            run_instance(
                &db,
                file,
                identity,
                vec![TypeShape::Primitive(Primitive::U64)],
                vec![ConstValue::Integer(
                    ConstInt::new(wide, i128::from(u64::MAX)).expect("u64")
                )]
            )
            .expect("wide identity")
        ),
        i128::from(u64::MAX)
    );
    for value in [3, 4, 3] {
        assert_eq!(
            number(
                run_instance(&db, file, count, vec![TypeShape::Const(value)], vec![])
                    .expect("value parameter")
            ),
            i128::from(value)
        );
    }
    assert_eq!(
        number(
            run_instance(&db, file, symbol(&db, file, "caller"), vec![], vec![])
                .expect("generic callee")
        ),
        42
    );
    for query in [
        "lower_amir",
        "runtime_unit",
        "instance_contracts",
        "borrow_interfaces",
    ] {
        assert_eq!(
            log.count_executions_matching(query),
            0,
            "forbidden final stage {query}"
        );
    }
}

#[test]
fn different_instantiations_of_one_callee_do_not_alias_local_synthetic_ids() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("generic-ctfe.aru".into(), "func count<const N: uint>(): uint { return N }\nfunc caller(): uint { return count<3>() * 10 + count<4>() }".into());
    assert_eq!(
        number(
            run_instance(&db, file, symbol(&db, file, "caller"), vec![], vec![])
                .expect("distinct values")
        ),
        34
    );
}

#[test]
fn comptime_parameter_spelling_reuses_const_identity_and_evaluation() {
    use arandu_middle::types::{FunctionInstance, TypeShape};
    use arandu_query::ctfe::CtfeInstanceRequest;
    let source = |marker| {
        format!("func count<{marker} N: uint>(): uint {{ return N }}\nfunc caller(): uint {{ return count<20>() + count<22>() }}")
    };
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("value-parameter.aru".into(), source("const"));
    let definition = symbol(&db, file, "count");
    let key = FunctionInstance {
        definition,
        arguments: vec![TypeShape::Const(42)],
    };
    let consume = |db: &DatabaseImpl| {
        let instance = arandu_query::runtime::Instance::new(db, file, key.clone());
        instance_consumer(db, CtfeInstanceRequest::new(db, instance, vec![], budget())).clone()
    };
    assert_eq!(number(consume(&db).expect("const parameter")), 42);
    log.clear();
    file.set_text(&mut db).to(source("comptime").into());
    assert_eq!(symbol(&db, file, "count"), definition);
    assert_eq!(number(consume(&db).expect("comptime parameter")), 42);
    assert_eq!(
        log.count_executions_matching("instance_consumer"),
        0,
        "equal semantic value must cut off consumers"
    );
    for value in [20, 22, u64::from(u32::MAX)] {
        assert_eq!(
            number(
                run_instance(&db, file, definition, vec![TypeShape::Const(value)], vec![])
                    .expect("same monomorphizer")
            ),
            i128::from(value)
        );
    }
    assert!(
        run_instance(
            &db,
            file,
            definition,
            vec![TypeShape::Const(u64::MAX)],
            vec![]
        )
        .is_err(),
        "uint parameters must not accept a value outside their declared width"
    );
    assert_eq!(
        number(
            run_instance(&db, file, symbol(&db, file, "caller"), vec![], vec![])
                .expect("concrete calls")
        ),
        42
    );
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
}

#[salsa::tracked]
fn instance_consumer<'db>(
    db: &'db dyn ArandCompilerDb,
    request: arandu_query::ctfe::CtfeInstanceRequest<'db>,
) -> Result<ConstValue, EvalError> {
    arandu_query::ctfe::ctfe_eval_instance(db, request).clone()
}

#[test]
fn concrete_ctfe_preserves_value_cutoff_and_clean_equivalence() {
    use arandu_middle::types::FunctionInstance;
    use arandu_query::ctfe::CtfeInstanceRequest;
    let source = |body: &str| {
        format!("func caller(): uint {{ return count<3>() * 10 + count<4>() }}\nfunc count<const N: uint>(): uint {{ {body} }}")
    };
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("generic-ctfe.aru".into(), source("return N"));
    let definition = symbol(&db, file, "caller");
    let key = FunctionInstance {
        definition,
        arguments: vec![],
    };
    let consume = |db: &DatabaseImpl| {
        let instance = arandu_query::runtime::Instance::new(db, file, key.clone());
        instance_consumer(db, CtfeInstanceRequest::new(db, instance, vec![], budget())).clone()
    };
    assert_eq!(number(consume(&db).expect("initial")), 34);
    log.clear();
    file.set_text(&mut db).to(Arc::from(source("return N + 0")));
    assert_eq!(number(consume(&db).expect("same value")), 34);
    assert_eq!(log.count_executions_matching("ctfe_eval_instance"), 1);
    assert_eq!(log.count_executions_matching("instance_consumer"), 0);
    log.clear();
    file.set_text(&mut db).to(Arc::from(source("return N + 1")));
    assert_eq!(number(consume(&db).expect("new value")), 45);
    assert_eq!(log.count_executions_matching("instance_consumer"), 1);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("generic-ctfe.aru".into(), source("return N + 1"));
    assert_eq!(
        consume(&db),
        run_instance(
            &clean,
            clean_file,
            symbol(&clean, clean_file, "caller"),
            vec![],
            vec![]
        )
    );
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
}

#[test]
fn concrete_ctfe_rejects_stale_file_identity_and_invalid_instantiation() {
    use arandu_middle::types::TypeShape;
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "generic-ctfe.aru".into(),
        "func count<const N: uint>(): uint { return N }\nfunc plain(): int { return 42 }".into(),
    );
    let other = db.new_file("other.aru".into(), "func other(): int { return 1 }".into());
    let count = symbol(&db, file, "count");
    assert_eq!(
        run_instance(&db, other, count, vec![TypeShape::Const(3)], vec![])
            .expect_err("wrong file")
            .kind,
        EvalErrorKind::MissingFunction(count)
    );
    assert!(run_instance(&db, file, count, vec![], vec![]).is_err());
    assert!(run_instance(
        &db,
        file,
        symbol(&db, file, "plain"),
        vec![TypeShape::Const(3)],
        vec![]
    )
    .is_err());
    assert_eq!(
        number(
            run_instance(&db, file, count, vec![TypeShape::Const(3)], vec![])
                .expect("valid request")
        ),
        3
    );
}

#[test]
fn literal_pool_changes_invalidate_even_when_the_amir_ids_are_identical() {
    use arandu_query::StableHash;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("literal.aru".into(), "func foo(): int { return 42 }".into());
    let foo = symbol(&db, file, "foo");
    let before = ctfe_func_amir(&db, file, foo)
        .result
        .as_ref()
        .expect("unit")
        .clone();
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("before")),
        42
    );
    file.set_text(&mut db)
        .to(Arc::from("func foo(): int { return 43 }"));
    let after = ctfe_func_amir(&db, file, foo)
        .result
        .as_ref()
        .expect("unit")
        .clone();
    assert_eq!(
        before.function().stable_hash(),
        after.function().stable_hash()
    );
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("after")),
        43
    );
}

#[test]
fn missing_import_is_rejected_without_a_backdoor_into_global_lowering() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "imports.aru".into(),
        "import missing\nfunc foo(): int { return 42 }".into(),
    );
    // Obtain the local identity without querying imported module signatures.
    let locals = arandu_query::passes::local_symbols(&db, file);
    let foo = locals
        .symbols
        .iter()
        .find(|symbol| symbol.name == "foo")
        .expect("local function")
        .id;
    log.clear();
    assert!(matches!(
        ctfe_func_amir(&db, file, foo).result,
        Err(BuildFailure::Diagnostics(_))
    ));
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    assert_eq!(log.count_executions_matching("item_body_typeck"), 0);
}

#[test]
fn imported_callees_are_staged_on_demand_and_preserve_value_cutoff() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library_source = |body: &str, sibling: &str| {
        format!(
        "module math\npublic func add(a: int, b: int): int {{ {body} }}\nfunc unused(): int {{ {sibling} }}"
    )
    };
    let library = db.new_file(
        "math.aru".into(),
        library_source("return a + b", "return missing"),
    );
    let file = db.new_file(
        "caller.aru".into(),
        "module caller\nimport math\nfunc foo(): int { return math.add(20, 22) }".into(),
    );
    let foo = symbol(&db, file, "foo");
    log.clear();
    assert_eq!(number(consume(&db, file, foo).expect("imported call")), 42);
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 2);
    assert_eq!(log.count_executions_matching("item_typing"), 2);
    for forbidden in [
        "lower_amir",
        "prepare_hir",
        "borrow_interfaces",
        "item_body_typeck",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{forbidden}: {}",
            log.format_chain(true)
        );
    }
    log.clear();
    library
        .set_text(&mut db)
        .to(Arc::from(library_source("return a + b", "return 999")));
    assert_eq!(
        number(consume(&db, file, foo).expect("unused sibling edit")),
        42
    );
    // A sibling diagnostic stays outside the selected unit's resolution shard.
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 0);
    assert_eq!(log.count_executions_matching("ctfe_eval"), 0);
    log.clear();
    library
        .set_text(&mut db)
        .to(Arc::from(library_source("return a + b", "return 998")));
    assert_eq!(
        number(consume(&db, file, foo).expect("body-only sibling edit")),
        42
    );
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 0);
    assert_eq!(log.count_executions_matching("ctfe_eval"), 0);
    log.clear();
    library
        .set_text(&mut db)
        .to(Arc::from(library_source("return a + b + 0", "return 999")));
    assert_eq!(
        number(consume(&db, file, foo).expect("same imported value")),
        42
    );
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 1);
    assert_eq!(log.count_executions_matching("consumer"), 0);
    log.clear();
    library
        .set_text(&mut db)
        .to(Arc::from(library_source("return a + b + 1", "return 999")));
    assert_eq!(
        number(consume(&db, file, foo).expect("changed imported value")),
        43
    );
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 1);
    assert_eq!(log.count_executions_matching("consumer"), 1);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("caller.aru".into(), file.text(&db).to_string());
    let _ = clean.new_file("math.aru".into(), library.text(&db).to_string());
    assert_eq!(
        consume(&db, file, foo),
        run(
            &clean,
            clean_file,
            symbol(&clean, clean_file, "foo"),
            vec![],
            budget()
        )
    );
}

#[test]
fn transitive_imports_use_each_defining_function_pool() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let _ = db.new_file(
        "leaf.aru".into(),
        "module leaf\npublic func value(): int { return 21 }".into(),
    );
    let _ = db.new_file(
        "middle.aru".into(),
        "module middle\nimport leaf as base\npublic func double(): int { return base.value() * 2 }"
            .into(),
    );
    let file = db.new_file(
        "root.aru".into(),
        "module root\nfrom middle import { double }\nfunc foo(): int { return double() }".into(),
    );
    let foo = symbol(&db, file, "foo");
    log.clear();
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("transitive CTFE")),
        42
    );
    assert_eq!(log.count_executions_matching("ctfe_func_amir"), 3);
    assert_eq!(log.count_executions_matching("prepare_hir"), 0);
    assert_eq!(log.count_executions_matching("borrow_interfaces"), 0);
}

#[test]
fn imported_callee_failures_recover_and_share_the_callers_frame_budget() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file(
        "math.aru".into(),
        "module math\npublic func answer(): int { return missing }".into(),
    );
    let file = db.new_file(
        "caller.aru".into(),
        "module caller\nimport math\nfunc foo(): int { return math.answer() }".into(),
    );
    let foo = symbol(&db, file, "foo");
    let answer = symbol(&db, library, "answer");
    assert_eq!(
        run(&db, file, foo, vec![], budget())
            .expect_err("invalid callee")
            .kind,
        EvalErrorKind::UnavailableFunction(answer)
    );
    library.set_text(&mut db).to(Arc::from(
        "module math\npublic func answer(): int { return 42 }",
    ));
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("recovered imported callee")),
        42
    );
    let mut limited = budget();
    limited.frames = 1;
    assert_eq!(
        run(&db, file, foo, vec![], limited)
            .expect_err("shared frame limit")
            .kind,
        EvalErrorKind::FrameLimit
    );
    limited.frames = 2;
    assert_eq!(
        number(run(&db, file, foo, vec![], limited).expect("root and imported frame")),
        42
    );
    for forbidden in [
        "lower_amir",
        "prepare_hir",
        "borrow_interfaces",
        "item_body_typeck",
    ] {
        assert_eq!(log.count_executions_matching(forbidden), 0);
    }
}

#[test]
fn cyclic_imports_are_rejected_before_ctfe_execution() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "a.aru".into(),
        "module a\nimport b\nfunc foo(): int { return 42 }".into(),
    );
    let _ = db.new_file(
        "b.aru".into(),
        "module b\nimport a\npublic func bar(): int { return 1 }".into(),
    );
    let foo = arandu_query::passes::local_symbols(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "foo")
        .expect("local function")
        .id;
    assert!(matches!(&ctfe_func_amir(&db, file, foo).result,
        Err(BuildFailure::Diagnostics(diagnostics)) if diagnostics.iter().any(|d| d.code == arandu_middle::DiagCode::N006ImportConflict)));
}

#[test]
fn cancelled_salsa_reader_unwinds_and_does_not_memoize_a_ctfe_failure() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("cancel.aru".into(), "func foo(): int { return 42 }".into());
    let foo = symbol(&db, file, "foo");
    let reader = db.clone();
    reader.query_cancellation_token().cancel();
    assert!(
        arandu_query::catch_query_cancellation(|| run(&reader, file, foo, vec![], budget()))
            .is_err()
    );
    drop(reader);
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("uncancelled reader")),
        42
    );
}

#[test]
fn relevant_resolution_errors_are_preserved_and_disappear_after_an_edit() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "error.aru".into(),
        "func foo(): int { return missing }".into(),
    );
    let foo = symbol(&db, file, "foo");
    assert!(
        matches!(&ctfe_func_amir(&db, file, foo).result, Err(BuildFailure::Diagnostics(diagnostics))
        if diagnostics.iter().any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::N001UndefinedValue))
    );
    file.set_text(&mut db)
        .to(Arc::from("func foo(): int { return 42 }"));
    assert_eq!(
        number(run(&db, file, foo, vec![], budget()).expect("fixed")),
        42
    );
}

#[test]
fn typed_use_casts_are_checked_instead_of_wrapping() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "cast.aru".into(),
        "func narrow(x: i64): u8 { return x as u8 }".into(),
    );
    let narrow = symbol(&db, file, "narrow");
    let ty = IntegerType::new(Primitive::I64, DataLayout::ptr_width(8)).expect("i64");
    let argument = |value| ConstValue::Integer(ConstInt::new(ty, value).expect("argument"));
    assert_eq!(
        number(run(&db, file, narrow, vec![argument(42)], budget()).expect("cast in range")),
        42
    );
    assert!(matches!(
        run(&db, file, narrow, vec![argument(256)], budget())
            .expect_err("out of range")
            .kind,
        EvalErrorKind::Value(arandu_middle::ctfe::ConstValueError::OutOfRange { .. })
    ));
}
