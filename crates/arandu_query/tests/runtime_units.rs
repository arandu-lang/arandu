#![allow(clippy::expect_used)]

use arandu_middle::types::{FunctionInstance, Primitive, TypeShape};
use arandu_middle::{hir::HirDecl, SymbolId};
use arandu_query::passes::{declaration_signatures, parse};
use arandu_query::runtime::{
    function_hir, instance_contracts, instance_hir, runtime_raw_unit, runtime_unit, Instance,
};
use arandu_query::{DatabaseImpl, SourceFile};
use salsa::Setter;
use std::sync::Arc;

#[test]
fn repeated_arrays_and_mutable_ctfe_roots_preserve_ssa_types() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "repeat.aru".into(),
        r#"
func repeated<T, comptime N: uint>(value: T): [N]T { return [value; N] }
func main(): int {
    let generic = repeated<int, 3>(42)
    let nested = comptime [[7; 2]; 3]
    let frozen = comptime { let mut table = [false; 256]; table[32] = true; table }
    if !frozen[32] || frozen[33] { return 2 }
    if generic[2] != 42 || nested[2][1] != 7 { return 1 }
    return 0
}
"#
        .into(),
    );
    let id = instance(&db, file, "main", Vec::new());
    let concrete = instance_hir(&db, id);
    let types = &concrete.artifacts.type_check.type_info.type_interner;
    if let Some(hir) = &concrete.artifacts.hir {
        for statement in hir.pool.stmts.iter() {
            if let arandu_middle::hir::HirStmtKind::VarDecl {
                bindings, value, ..
            } = &statement.kind
            {
                for binding in hir.pool.bindings_list(*bindings) {
                    assert_eq!(
                        TypeShape::from_id(binding.ty, types),
                        TypeShape::from_id(hir.pool.expr(*value).ty, types),
                        "binding and frozen initializer must have the same structural type",
                    );
                }
            }
        }
    }
    let raw = runtime_raw_unit(&db, id);
    assert!(raw.result.is_ok(), "{:?}", raw.result);
}

fn instance<'db>(
    db: &'db DatabaseImpl,
    file: SourceFile,
    name: &str,
    arguments: Vec<TypeShape>,
) -> Instance<'db> {
    Instance::new(
        db,
        file,
        FunctionInstance {
            definition: symbol(db, file, name),
            arguments,
        },
    )
}

#[test]
fn typed_integer_modified_through_a_reference_is_reloaded_from_memory() {
    use arandu_middle::amir::{AmirRvalue, AmirStmt};

    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "extern \"C\" { func update(n: mut ref usize): void }\nfunc main(): usize {\nlet mut n: usize = 0\nunsafe { update(n) }\nreturn n\n}".into(),
    );
    let raw = runtime_raw_unit(&db, instance(&db, file, "main", Vec::new()));
    let unit = raw.result.as_ref().expect("reference argument unit");
    let function = &unit.function;
    let local = function
        .locals
        .iter()
        .enumerate()
        .find(|(_, local)| local.is_memory)
        .map(|(id, _)| arandu_middle::amir::LocalId::from_usize(id))
        .expect("address-taken integer local");
    assert!(
        function.stmts.payloads.iter().any(|statement| matches!(
            statement,
            AmirStmt::Assign { rhs: AmirRvalue::Load(place), .. }
                if place.local == local && place.projections.is_empty()
        )),
        "an external write must not be replaced by the integer's initial constant"
    );
}

#[test]
fn returned_wrapper_discovers_its_field_destructor_without_reading_callee_bodies() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        r#"
struct Counter<T> { value: T }
@Destructor
func Counter.destroy<T>(own self: Counter<T>): void {}
struct Wrapper { counter: Counter<int> }
func make(): Wrapper { return Wrapper { counter: Counter<int> { value: 1 } } }
func main(): int { let wrapped = make() return 0 }
"#
        .into(),
    );
    let main = runtime_raw_unit(&db, instance(&db, file, "main", Vec::new()));
    let body = main.result.as_ref().expect("valid caller");
    assert!(body.function.stmts.payloads.iter().any(|statement| matches!(
        statement, arandu_middle::amir::AmirStmt::Destroy(place) if !place.projections.is_empty()
    )), "a field-owned destructor must be visible before composing callees");
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
    assert!(program.instances.iter().any(|(_, key)| {
        key.definition == symbol(&db, file, "Counter.destroy")
            && key.arguments == [TypeShape::Primitive(Primitive::Int)]
    }));
}

#[test]
fn final_unit_uses_imported_borrow_contracts_without_global_lowering() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file(
        "loans.aru".into(),
        "module loans\npublic func choose(a: ref int, b: ref int): ref int { return a }".into(),
    );
    let file = db.new_file("main.aru".into(), "import loans\nfunc selected(a: ref int, b: ref int): ref int { return loans.choose(a, b) }\nfunc broken(): int { return absent }".into());
    let definition = symbol(&db, file, "selected");
    let key = FunctionInstance {
        definition,
        arguments: Vec::new(),
    };
    {
        let id = Instance::new(&db, file, key.clone());
        let contracts = instance_contracts(&db, id);
        assert!(
            contracts.diagnostics.is_empty(),
            "{:?}",
            contracts.diagnostics
        );
        let own = contracts
            .entries
            .iter()
            .find(|(key, _)| key.definition == definition)
            .expect("contract");
        assert_eq!(own.1.dependencies[0].sources[0].parameter_index, 0);
        let checked = runtime_unit(&db, id);
        assert!(checked.result.is_ok(), "{:?}", checked.result);
    }
    log.clear();
    library.set_text(&mut db).to(Arc::from(
        "module loans\npublic func choose(a: ref int, b: ref int): ref int { return b }",
    ));
    let id = Instance::new(&db, file, key);
    let checked = runtime_unit(&db, id);
    let own = instance_contracts(&db, id)
        .entries
        .iter()
        .find(|(key, _)| key.definition == definition)
        .expect("updated contract");
    assert_eq!(own.1.dependencies[0].sources[0].parameter_index, 1);
    assert!(checked.result.is_ok(), "{:?}", checked.result);
    assert_eq!(
        log.count_executions_matching("runtime_raw_unit"),
        1,
        "only the callee body is lowered: {}",
        log.format_chain(true)
    );
    assert_eq!(
        log.count_executions_matching("function_hir"),
        1,
        "unrelated HIR bodies must not be re-lowered: {}",
        log.format_chain(true)
    );
    assert_eq!(log.count_executions_matching("runtime_unit"), 1);
    for forbidden in [
        "prepare_hir",
        "lower_amir",
        "borrow_interfaces",
        "file_typing",
        "module_signatures",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{forbidden}: {}",
            log.format_chain(true)
        );
    }
}

#[test]
fn recursive_and_generic_instance_contracts_converge_without_query_cycles() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "func identity<T>(value: ref T): ref T { return value }\nfunc selected(depth: int, left: ref int, right: ref int): ref int { if depth == 0 { return identity(left) } return selected(depth - 1, right, left) }".into());
    let id = instance(&db, file, "selected", Vec::new());
    let contracts = instance_contracts(&db, id);
    assert!(
        contracts.diagnostics.is_empty(),
        "{:?}",
        contracts.diagnostics
    );
    let own = contracts
        .entries
        .iter()
        .find(|(key, _)| key.definition == id.key(&db).definition)
        .expect("recursive contract");
    assert_eq!(
        own.1.dependencies[0]
            .sources
            .iter()
            .map(|source| source.parameter_index)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let checked = runtime_unit(&db, id);
    assert!(checked.result.is_ok(), "{:?}", checked.result);
}

#[test]
fn final_unit_rejects_a_borrow_without_formal_origin() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "@NoFallback\nfunc selected(): ref int { let local = 42 return ref local }".into(),
    );
    let checked = runtime_unit(&db, instance(&db, file, "selected", Vec::new()));
    assert!(checked.result.as_ref().expect_err("invalid escaping borrow").iter().any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::O010EscapeOfBorrowedValue), "{:?}", checked.result);
}

#[test]
fn a_recursive_call_alone_cannot_invent_a_formal_borrow_origin() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func selected(value: ref int): ref int { return selected(value) }".into(),
    );
    let id = instance(&db, file, "selected", Vec::new());
    let contracts = instance_contracts(&db, id);
    assert!(
        contracts.diagnostics.is_empty(),
        "{:?}",
        contracts.diagnostics
    );
    assert!(contracts
        .entries
        .iter()
        .all(|(_, summary)| summary.dependencies.is_empty()));
    let checked = runtime_unit(&db, id);
    assert!(checked.result.as_ref().expect_err("unproven recursive borrow").iter().any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::O010EscapeOfBorrowedValue), "{:?}", checked.result);
}

#[test]
fn generic_calls_in_loop_initializer_and_step_are_both_specialized() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "func identity<T>(x: T): T { return x }\nfunc selected(): int { let mut i = 0 for i = identity<int>(0); i < 3; i = identity<int>(i + 1) {} return i }".into());
    let checked = runtime_raw_unit(&db, instance(&db, file, "selected", Vec::new()));
    let raw = checked.result.as_ref().expect("loop unit");
    let identity = symbol(&db, file, "identity");
    let specialized = checked
        .instances
        .iter()
        .find(|(_, key)| key.definition == identity)
        .expect("identity header")
        .0;
    let calls = raw
        .function
        .stmts
        .iter_ids()
        .filter_map(|id| match raw.function.stmts.get(id) {
            Some(arandu_middle::amir::AmirStmt::Call {
                callee: arandu_middle::amir::AmirOperand::FunctionRef(symbol),
                ..
            }) => Some(*symbol),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, vec![specialized, specialized]);
}

#[test]
fn a_generic_instance_lowers_only_its_body_with_concrete_callee_headers() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("main.aru".into(),
        "func inner<T>(x: T): T { return x }\nfunc outer<T>(x: T): T { return inner<T>(x) }\nfunc broken(): int { return absent }".into());
    let definition = symbol(&db, file, "outer");
    let helper = symbol(&db, file, "inner");
    let key = FunctionInstance {
        definition,
        arguments: vec![TypeShape::Primitive(Primitive::Int)],
    };
    log.clear();
    let id = Instance::new(&db, file, key.clone());
    let hir = instance_hir(&db, id);
    assert!(
        hir.artifacts.diagnostics.is_empty(),
        "{:?}",
        hir.artifacts.diagnostics
    );
    let lowered = hir.artifacts.hir.as_ref().expect("instance HIR");
    assert_eq!(
        lowered
            .decls
            .iter()
            .filter(
                |&&decl| matches!(lowered.pool.decl(decl), HirDecl::Func(f) if f.body.is_some())
            )
            .count(),
        1
    );
    assert!(hir.instances.iter().any(|(_, key)| key.definition == helper
        && key.arguments == vec![TypeShape::Primitive(Primitive::Int)]));
    let raw = runtime_raw_unit(&db, id);
    let unit = raw.result.as_ref().expect("instance AMIR");
    assert_eq!(
        TypeShape::from_id(
            unit.function.return_type,
            &raw.context.type_info.type_interner
        ),
        Ok(TypeShape::Primitive(Primitive::Int))
    );
    assert_eq!(
        log.count_executions_matching("item_typing"),
        1,
        "{}",
        log.format_chain(true)
    );
    for forbidden in [
        "prepare_hir",
        "lower_amir",
        "borrow_interfaces",
        "file_typing",
        "module_signatures",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{forbidden}: {}",
            log.format_chain(true)
        );
    }
}

#[test]
fn changing_a_callee_body_does_not_rebuild_the_callers_raw_instance() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let text = "func inner<T>(x: T): int { return 1 }\nfunc outer(): int { return inner<int>(42) }";
    let file = db.new_file("main.aru".into(), text.into());
    let definition = symbol(&db, file, "outer");
    let key = FunctionInstance {
        definition,
        arguments: Vec::new(),
    };
    {
        let raw = runtime_raw_unit(&db, Instance::new(&db, file, key.clone()));
        assert!(raw.result.is_ok(), "{:?}", raw.result);
    }
    log.clear();
    // Same byte length and signature, different callee behavior.
    file.set_text(&mut db)
        .to(Arc::from(text.replace("return 1", "return 2")));
    let raw = runtime_raw_unit(&db, Instance::new(&db, file, key));
    assert!(raw.result.is_ok(), "{:?}", raw.result);
    assert_eq!(
        log.count_executions_matching("runtime_raw_unit"),
        0,
        "{}",
        log.format_chain(true)
    );
}

#[test]
fn instances_of_the_same_template_keep_separate_concrete_return_types() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func identity<T>(x: T): T { return x }".into(),
    );
    for primitive in [Primitive::Int, Primitive::Bool] {
        let raw = runtime_raw_unit(
            &db,
            instance(&db, file, "identity", vec![TypeShape::Primitive(primitive)]),
        );
        let unit = raw.result.as_ref().expect("concrete unit");
        assert_eq!(
            TypeShape::from_id(
                unit.function.return_type,
                &raw.context.type_info.type_interner
            ),
            Ok(TypeShape::Primitive(primitive))
        );
    }
}

#[test]
fn an_instance_with_nested_free_or_unknown_type_symbols_is_rejected() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func identity<T>(x: T): T { return x }".into(),
    );
    let definition = symbol(&db, file, "identity");
    let parameter = declaration_signatures(&db, file).type_info.generic_params[&definition][0];
    for symbol in [parameter, SymbolId::new(999_999, 42)] {
        let key = FunctionInstance {
            definition,
            arguments: vec![TypeShape::Option(Box::new(TypeShape::Named(
                symbol,
                Vec::new(),
            )))],
        };
        let raw = runtime_raw_unit(&db, Instance::new(&db, file, key));
        assert!(
            raw.result
                .as_ref()
                .expect_err("unresolved structural argument")
                .iter()
                .any(|diagnostic| diagnostic.code
                    == arandu_middle::DiagCode::G002GenericInstantiationLimit),
            "{:?}",
            raw.result
        );
    }
}

#[test]
fn generic_headers_with_the_same_short_name_retain_distinct_module_identities() {
    let mut db = DatabaseImpl::new();
    let a = db.new_file(
        "a.aru".into(),
        "module a\npublic func identity<T>(x: T): T { return x }".into(),
    );
    let b = db.new_file(
        "b.aru".into(),
        "module b\npublic func identity<T>(x: T): T { return x }".into(),
    );
    let file = db.new_file("main.aru".into(), "import a\nimport b\nfunc selected(): int { return a.identity<int>(20) + b.identity<int>(22) }".into());
    let raw = runtime_raw_unit(&db, instance(&db, file, "selected", Vec::new()));
    assert!(raw.result.is_ok(), "{:?}", raw.result);
    let a_definition = symbol(&db, a, "identity");
    let b_definition = symbol(&db, b, "identity");
    let a_header = raw
        .instances
        .iter()
        .find(|(_, key)| key.definition == a_definition)
        .expect("a header")
        .0;
    let b_header = raw
        .instances
        .iter()
        .find(|(_, key)| key.definition == b_definition)
        .expect("b header")
        .0;
    assert_ne!(a_header, b_header);
    assert_ne!(
        raw.context.symbols.host_function_names[&a_header],
        raw.context.symbols.host_function_names[&b_header]
    );
}

#[test]
fn changing_a_returned_origin_invalidates_only_final_caller_validation() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file(
        "loans.aru".into(),
        "module loans\npublic func choose(a: ref int, b: ref int): ref int { return a }".into(),
    );
    let file = db.new_file("main.aru".into(), "import loans\nfunc selected(): int { let mut a = 1\nlet mut b = 2\nlet borrowed = loans.choose(a, b)\nb = 3\nreturn *borrowed }".into());
    let definition = symbol(&db, file, "selected");
    let key = FunctionInstance {
        definition,
        arguments: Vec::new(),
    };
    {
        let checked = runtime_unit(&db, Instance::new(&db, file, key.clone()));
        assert!(checked.result.is_ok(), "{:?}", checked.result);
    }
    log.clear();
    library.set_text(&mut db).to(Arc::from(
        "module loans\npublic func choose(a: ref int, b: ref int): ref int { return b }",
    ));
    let checked = runtime_unit(&db, Instance::new(&db, file, key));
    assert!(
        checked
            .result
            .as_ref()
            .expect_err("mutation of the newly borrowed input")
            .iter()
            .any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::O003MutableBorrowConflict),
        "{:?}",
        checked.result
    );
    assert_eq!(
        log.count_executions_matching("runtime_raw_unit"),
        1,
        "{}",
        log.format_chain(true)
    );
    assert_eq!(log.count_executions_matching("runtime_unit"), 1);
}

fn symbol(db: &DatabaseImpl, file: SourceFile, name: &str) -> SymbolId {
    let declarations = declaration_signatures(db, file);
    arandu_semantics::body_item_symbols(
        parse(db, file).as_ref().expect("source"),
        &declarations.resolved,
    )
    .into_iter()
    .find(|id| declarations.symbols.get(*id).name == name)
    .expect("function")
}

#[test]
fn runtime_composition_rebases_generic_symbols_types_and_literal_pools() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "func identity<T>(value: T): T { return value }\nfunc left(): int { return identity<int>(41) }\nfunc right(): bool { return identity<bool>(true) }\nfunc main(): int { if right() { return left() + 1 } return 0 }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    let output = &composed.artifacts;
    assert_eq!(output.amir.funcs.len(), 5);
    let unique = output
        .amir
        .funcs
        .iter()
        .map(|function| function.symbol)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique.len(), 5);
    let definition = symbol(&db, file, "identity");
    for primitive in [Primitive::Int, Primitive::Bool] {
        let (id, _) = composed
            .instances
            .iter()
            .find(|(_, key)| {
                key.definition == definition
                    && key.arguments == vec![TypeShape::Primitive(primitive)]
            })
            .expect("canonical instance");
        let function = output
            .amir
            .funcs
            .iter()
            .find(|function| function.symbol == *id)
            .expect("instantiated body");
        assert_eq!(
            TypeShape::from_id(
                function.return_type,
                &output.type_check.type_info.type_interner
            ),
            Ok(TypeShape::Primitive(primitive))
        );
    }
    for function in &output.amir.funcs {
        for stmt in function
            .stmts
            .iter_ids()
            .filter_map(|id| function.stmts.get(id))
        {
            arandu_middle::amir::visit::for_each_stmt_operand_mut(&mut stmt.clone(), |operand| {
                if let arandu_middle::amir::AmirOperand::FunctionRef(id) = operand {
                    assert!(unique.contains(id), "unlinked callee {id:?}");
                }
            });
        }
    }
    assert!(output.amir.literal_pool.entries.iter().any(|literal| matches!(literal, arandu_middle::literal_pool::AmirLiteralEntry::Int(value) if value == "41")));
}

#[test]
fn nominal_arguments_with_equal_short_names_keep_distinct_backend_names() {
    let mut db = DatabaseImpl::new();
    db.new_file(
        "a.aru".into(),
        "module a\npublic struct User { public value: int }".into(),
    );
    db.new_file(
        "b.aru".into(),
        "module b\npublic struct User { public value: int }".into(),
    );
    let file = db.new_file("main.aru".into(), "import a\nimport b\nfunc identity<T>(value: T): T { return value }\nfunc main(): int { let left = identity(a.User { value: 20 })\nlet right = identity(b.User { value: 22 })\nreturn left.value + right.value }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    let definition = symbol(&db, file, "identity");
    let names = composed
        .instances
        .iter()
        .filter(|(_, key)| key.definition == definition)
        .map(|(id, _)| {
            composed
                .artifacts
                .type_check
                .symbols
                .host_func_name(composed.artifacts.type_check.symbols.get(*id))
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn nested_nominal_definitions_keep_qualified_backend_identity() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "module app\nmodule left { public struct User { public value: int } }\nmodule right { public struct User { public value: int } }\nfunc identity<T>(value: T): T { return value }\nfunc main(): int { let a = identity(left.User { value: 20 }) let b = identity(right.User { value: 22 }) return a.value + b.value }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    let definition = symbol(&db, file, "identity");
    let names = composed
        .instances
        .iter()
        .filter(|(_, key)| key.definition == definition)
        .map(|(id, _)| {
            composed
                .artifacts
                .type_check
                .symbols
                .host_func_name(composed.artifacts.type_check.symbols.get(*id))
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn composing_a_callee_edit_does_not_relower_unmodified_callers_or_siblings() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "func callee(): int { return 41 }\nfunc caller(): int { return callee() }\nfunc sibling(): int { return 42 }";
    let file = db.new_file("main.aru".into(), source.into());
    assert!(arandu_query::runtime::runtime_program(&db, file)
        .diagnostics
        .is_empty());
    log.clear();
    file.set_text(&mut db)
        .to(Arc::from(source.replace("41", "40")));
    assert!(arandu_query::runtime::runtime_program(&db, file)
        .diagnostics
        .is_empty());
    assert_eq!(
        log.count_executions_matching("runtime_raw_unit"),
        1,
        "{}",
        log.format_chain(true)
    );
    assert_eq!(
        log.count_executions_matching("runtime_unit"),
        1,
        "{}",
        log.format_chain(true)
    );
    for forbidden in [
        "prepare_hir",
        "lower_amir",
        "file_typing",
        "module_signatures",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{}",
            log.format_chain(true)
        );
    }
}

#[test]
fn imported_effect_metadata_survives_expression_shard_removal() {
    let mut db = DatabaseImpl::new();
    let helper = db.new_file(
        "helper.aru".into(),
        "module helper\n@Effects(Pure)\npublic func answer(): int { return 42 }".into(),
    );
    let entry = db.new_file(
        "main.aru".into(),
        "import helper\nfunc main(): int { return helper.answer() }".into(),
    );
    let key = FunctionInstance {
        definition: symbol(&db, helper, "answer"),
        arguments: Vec::new(),
    };
    let unit = runtime_unit(&db, Instance::new(&db, helper, key.clone()));
    let mut aggregate = arandu_query::runtime::declaration_hir(&db, entry)
        .artifacts
        .type_check
        .clone();
    aggregate.type_info_mut().function_effects.clear();
    arandu_mir::compose_function_units(
        &mut aggregate,
        &[arandu_mir::ContextualFunctionUnit {
            key: &key,
            unit: unit.result.as_ref().expect("validated callee"),
            context: &unit.context,
            generated_symbols: &unit.generated_symbols,
            instances: &unit.instances,
            borrow_summary: unit.borrow_summary.as_ref().expect("summary"),
        }],
        || {},
    )
    .expect("valid composition");
    assert!(aggregate
        .type_info
        .function_effects
        .get(&key.definition)
        .expect("imported effects")
        .contains(arandu_middle::EffectFlags::PURE));
}

#[test]
fn later_header_merges_cannot_overwrite_proven_borrow_contracts() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "func second(a: ref int, b: ref int): ref int { return b }\nfunc caller(a: ref int, b: ref int): ref int { return second(a, b) }\nfunc scalar(): int { return 42 }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    for name in ["second", "caller"] {
        let summary = composed
            .artifacts
            .type_check
            .type_info
            .return_borrow_summaries
            .get(&symbol(&db, file, name))
            .expect("proven summary");
        assert_eq!(
            summary.dependencies[0].sources[0].parameter_index, 1,
            "{name}: {summary:?}"
        );
    }
}

#[test]
fn invalid_declaration_metadata_blocks_executable_publication() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "struct Broken { value: Absent }\nfunc main(): int { return 42 }".into(),
    );
    let result = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        !result.diagnostics.is_empty(),
        "invalid declaration must not be certified by a valid body"
    );
    assert!(result.artifacts.amir.funcs.is_empty());
}

#[test]
fn composition_does_not_publish_a_partial_program_with_an_invalid_entry_body() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func valid(): int { return 42 }\nfunc broken(): int { return absent }".into(),
    );
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(composed.artifacts.amir.funcs.is_empty());
    assert!(composed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::N001UndefinedValue));
}

#[test]
fn an_invalid_reachable_dependency_cannot_publish_executable_mir() {
    let mut db = DatabaseImpl::new();
    let helper = db.new_file(
        "helper.aru".into(),
        "module helper\nimport missing\npublic func answer(): int { return 42 }".into(),
    );
    let file = db.new_file(
        "main.aru".into(),
        "import helper\nfunc main(): int { return helper.answer() }".into(),
    );
    let result = arandu_query::runtime::runtime_program(&db, file);
    assert!(result.artifacts.amir.funcs.is_empty());
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.as_str() == "M001"
            && diagnostic.span.file_id == *helper.file_id(&db)));
}

#[test]
fn composition_links_only_reachable_imported_bodies() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file("helper.aru".into(), "module helper\npublic func good(): int { return 42 }\nfunc broken(): int { return absent }".into());
    let file = db.new_file(
        "main.aru".into(),
        "import helper\nfunc main(): int { return helper.good() }".into(),
    );
    log.clear();
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    assert_eq!(composed.artifacts.amir.funcs.len(), 2);
    assert_eq!(log.count_executions_matching("item_typing"), 2);
}

#[test]
fn inferred_generic_struct_receiver_is_concrete_before_instance_lowering() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "struct BoxG<T> { v: T }\nfunc BoxG.get(shared self): T { return self.v }\nfunc main(): int { let b = BoxG { v: 42 } return b.get() }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
}

#[test]
fn a_concrete_type_from_the_caller_brings_declarations_not_sibling_bodies() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file(
        "generic.aru".into(),
        "module generic\npublic func identity<T>(value: T): T { return value }".into(),
    );
    db.new_file("owner.aru".into(), "module owner\npublic struct Owner { public value: int }\nfunc broken(): int { return absent }".into());
    let file = db.new_file("main.aru".into(), "import generic\nimport owner\nfunc main(): int { let value = generic.identity(owner.Owner { value: 42 }) return value.value }".into());
    log.clear();
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    assert_eq!(composed.artifacts.amir.funcs.len(), 2);
    assert_eq!(log.count_executions_matching("item_typing"), 2);
}

#[test]
fn specialized_function_values_are_executable_dependencies_too() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "extern \"C\" { func sink(callback: ptr[u8]): void }\nfunc identity<T>(value: T): T { return value }\nfunc main(): int { unsafe { sink(identity<int> as ptr[u8]) } return 42 }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    assert_eq!(composed.artifacts.amir.funcs.len(), 2);
}

#[test]
fn equivalent_literal_receiver_shapes_share_one_destructor_instance() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "struct Counter<T, A = int> { value: T tag: A }\n@Destructor\nfunc Counter.destroy<T, A>(self: Counter<T, A>): void {}\nfunc inspect<T>(c: ref Counter<T>): T { return c.value }\nfunc main(): int { let c: Counter<int> = Counter { value: 42, tag: 1 } return inspect(c) }".into());
    let composed = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        composed.diagnostics.is_empty(),
        "{:?}",
        composed.diagnostics
    );
    let destructors = composed
        .instances
        .iter()
        .filter(|(_, key)| {
            composed
                .artifacts
                .type_check
                .symbols
                .get(key.definition)
                .name
                .ends_with("destroy")
        })
        .collect::<Vec<_>>();
    assert_eq!(destructors.len(), 1, "{destructors:?}");
}

#[test]
fn source_func_amir_and_ide_analysis_do_not_request_program_wide_stages() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "func selected(): int { return 42 }\nfunc broken(): int { return absent }";
    let file = db.new_file("main.aru".into(), source.into());
    let selected = symbol(&db, file, "selected");
    log.clear();
    assert!(!arandu_query::func_amir(&db, file, selected)
        .blocks
        .is_empty());
    assert!(arandu_query::dataflow::item_ide_diagnostics(&db, file, selected).is_empty());
    for forbidden in [
        "runtime_program",
        "prepare_hir",
        "lower_amir",
        "file_typing",
        "module_signatures",
        "borrow_interfaces",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{}",
            log.format_chain(true)
        );
    }
}

#[test]
fn function_hir_retains_only_its_body_and_headers_are_shared_separately() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file("math.aru".into(), "module math\npublic func helper(x: int): int { return x + 1 }\nfunc bad(): int { return absent }".into());
    let entry = db.new_file("main.aru".into(), "import math\nfunc selected(): int { return math.helper(41) }\nfunc sibling(): int { return absent }".into());
    let selected = symbol(&db, entry, "selected");
    let helper = symbol(&db, library, "helper");
    log.clear();
    let lowered = function_hir(&db, entry, selected);
    assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);
    let hir = lowered.hir.as_ref().expect("selected HIR");
    let bodies = hir
        .decls
        .iter()
        .filter_map(|&id| match hir.pool.decl(id) {
            HirDecl::Func(function) if function.body.is_some() => Some(function.symbol),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(bodies, vec![selected]);
    assert_eq!(hir.decls.len(), 1, "function memo must not retain headers");
    let headers = arandu_query::runtime::declaration_context(&db, entry);
    let headers_hir = headers.hir.as_ref().expect("declaration closure");
    assert!(headers_hir.decls.iter().any(|&id| matches!(headers_hir.pool.decl(id), HirDecl::Func(function) if function.symbol == helper && function.body.is_none())));
    assert_eq!(log.count_executions_matching("item_typing"), 1);
    for forbidden in [
        "prepare_hir",
        "file_typing",
        "module_signatures",
        "borrow_interfaces",
        "lower_amir",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{forbidden}: {}",
            log.format_chain(true)
        );
    }
}

#[test]
fn a_selected_function_with_own_error_does_not_get_a_dummy_hir() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "main.aru".into(),
        "func selected(): int { return absent }\nfunc sibling(): int { return 42 }".into(),
    );
    let selected = symbol(&db, file, "selected");
    let lowered = function_hir(&db, file, selected);
    assert!(lowered.hir.is_none());
    assert!(lowered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::N001UndefinedValue));
}

#[test]
fn declaration_context_preserves_constants_and_consuming_receiver_modes() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("main.aru".into(), "const ANSWER = 42\nstruct Owner { value: int }\nfunc Owner.consume(self: own Owner): int { return self.value }\nfunc selected(): int { return ANSWER }".into());
    let selected = symbol(&db, file, "selected");
    let lowered = instance_hir(&db, instance(&db, file, "selected", Vec::new()));
    let lowered = &lowered.artifacts;
    assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);
    let hir = lowered.hir.as_ref().expect("selected HIR");
    assert!(hir
        .decls
        .iter()
        .any(|&id| matches!(hir.pool.decl(id), HirDecl::Const(_))));
    assert!(hir.decls.iter().any(|&id| match hir.pool.decl(id) {
        HirDecl::Func(function)
            if lowered
                .type_check
                .symbols
                .get(function.symbol)
                .name
                .ends_with("consume") =>
        {
            hir.pool.params_list(function.params)[0].receiver_kind
                == Some(arandu_middle::hir::ReceiverKind::Own)
        }
        _ => false,
    }));
    let function = hir
        .decls
        .iter()
        .find_map(|&id| match hir.pool.decl(id) {
            HirDecl::Func(function) if function.symbol == selected && function.body.is_some() => {
                Some(function)
            }
            _ => None,
        })
        .expect("function");
    let unit = arandu_mir::lower_function_unit(
        &lowered.type_check,
        hir,
        function,
        arandu_middle::DataLayout::ptr_width(8),
    )
    .expect("MIR unit");
    assert_eq!(unit.literals.entries.len(), 1);
    assert_eq!(
        arandu_middle::literal_pool::parse_int_literal(match &unit.literals.entries[0] {
            arandu_middle::literal_pool::AmirLiteralEntry::Int(value) => value,
            other => panic!("unexpected literal: {other:?}"),
        }),
        Some(42)
    );
}

#[test]
fn synthetic_ids_cannot_trigger_aggregate_lowering_in_source_analysis() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "main.aru".into(),
        "func identity<T>(x: T): T { return x }\nfunc main(): int { return identity<int>(42) }"
            .into(),
    );
    let program = arandu_query::runtime::runtime_program(&db, file);
    let synthetic = program
        .instances
        .iter()
        .find(|(_, key)| !key.arguments.is_empty())
        .expect("concrete identity")
        .0;
    log.clear();
    assert!(arandu_query::func_amir(&db, file, synthetic)
        .blocks
        .is_empty());
    assert!(
        arandu_query::func_amir(&db, file, SymbolId::new(*file.file_id(&db), u32::MAX))
            .blocks
            .is_empty()
    );
    let sources = arandu_query::file_func_symbols(&db, file);
    assert_eq!(sources.as_slice(), [symbol(&db, file, "main")]);
    for forbidden in ["runtime_program", "lower_amir", "runtime_unit"] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{}",
            log.format_chain(true)
        );
    }
}

#[test]
fn concrete_analysis_uses_structural_identity_and_shares_the_unit() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "main.aru".into(),
        "func identity<T>(x: T): T { return x }".into(),
    );
    for primitive in [Primitive::Int, Primitive::Bool] {
        let id = instance(&db, file, "identity", vec![TypeShape::Primitive(primitive)]);
        let unit = runtime_unit(&db, id);
        let function = arandu_query::runtime::instance_amir(&db, id)
            .as_ref()
            .expect("concrete analysis");
        assert!(Arc::ptr_eq(
            &function.value,
            unit.analysis_function.as_ref().expect("shared function")
        ));
        assert_eq!(
            unit.context.type_info.resolve_type_id(function.return_type),
            arandu_middle::types::ArType::Primitive(primitive)
        );
    }
    assert_eq!(log.count_executions_matching("declaration_context"), 1);
    for forbidden in ["runtime_program", "lower_amir", "prepare_hir"] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "{}",
            log.format_chain(true)
        );
    }
}
