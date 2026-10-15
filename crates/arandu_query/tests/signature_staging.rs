#![allow(clippy::expect_used)]

use std::sync::Arc;

use arandu_middle::{Severity, SymbolId};
use arandu_query::passes::{declaration_signatures, ide_type_check, module_signatures, parse};
use arandu_query::{DatabaseImpl, SourceFile};
use salsa::Setter;

fn function(db: &DatabaseImpl, file: SourceFile, name: &str) -> SymbolId {
    let signatures = declaration_signatures(db, file);
    let parsed = parse(db, file);
    let program = parsed.as_ref().expect("program");
    arandu_semantics::body_item_symbols(program, &signatures.resolved)
        .into_iter()
        .find(|symbol| {
            signatures
                .symbols
                .try_get(*symbol)
                .is_some_and(|symbol| symbol.name == name)
        })
        .expect("function")
}

fn loans(body: &str) -> String {
    format!("module loans\npublic func choose(left: ref int, right: ref int): ref int {{ {body} }}")
}

#[test]
fn declarations_with_imports_do_not_request_bodies_or_global_amir() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file("loans.aru".into(), loans("return right"));
    let caller = db.new_file(
        "caller.aru".into(),
        "module caller\nimport loans\nfunc foo(left: ref int, right: ref int): ref int { return loans.choose(left, right) }".into(),
    );
    let declared = declaration_signatures(&db, caller);
    assert!(
        declared.diagnostics.is_empty(),
        "{:?}",
        declared.diagnostics
    );
    let choose = function(&db, library, "choose");
    assert!(
        declared.type_info.decl_types.contains_key(&choose),
        "imported function type was lost"
    );
    // Two reference parameters cannot determine the returned origin from the
    // declaration. Reading signatures must not inspect the body to choose one.
    assert!(!declared
        .type_info
        .return_borrow_summaries
        .contains_key(&choose));
    for forbidden in [
        "module_signatures",
        "borrow_interfaces",
        "item_body_typeck",
        "lower_amir",
    ] {
        assert_eq!(
            log.count_executions_matching(forbidden),
            0,
            "declarations requested {forbidden}:\n{}",
            log.format_chain(true)
        );
    }
    let checked = module_signatures(&db, caller);
    let summary = checked
        .type_info
        .return_borrow_summaries
        .get(&choose)
        .expect("flow-derived contract");
    assert_eq!(summary.dependencies[0].sources[0].parameter_index, 1);
    assert!(
        !declared
            .type_info
            .return_borrow_summaries
            .contains_key(&choose),
        "composition mutated the declaration memo"
    );
    assert!(log.count_executions_matching("borrow_interfaces") > 0);
}

#[salsa::tracked]
fn declaration_consumer(db: &dyn arandu_query::ArandCompilerDb, file: SourceFile) -> usize {
    declaration_signatures(db, file).type_info.decl_types.len()
}

#[test]
fn body_derived_contract_changes_do_not_invalidate_declaration_consumers() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file("loans.aru".into(), loans("return left"));
    let caller = db.new_file(
        "caller.aru".into(),
        "module caller\nimport loans\nfunc foo(): int { return 42 }".into(),
    );
    let choose = function(&db, library, "choose");
    let initial = *declaration_consumer(&db, caller);
    assert_eq!(
        module_signatures(&db, caller)
            .type_info
            .return_borrow_summaries[&choose]
            .dependencies[0]
            .sources[0]
            .parameter_index,
        0
    );
    log.clear();
    // The declaration is unchanged; only the returned formal changes.
    library
        .set_text(&mut db)
        .to(Arc::from(loans("return right")));
    assert_eq!(*declaration_consumer(&db, caller), initial);
    assert_eq!(log.count_executions_matching("declaration_consumer"), 0);
    assert_eq!(log.count_executions_matching("borrow_interfaces"), 0);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    let checked = module_signatures(&db, caller);
    assert_eq!(
        checked.type_info.return_borrow_summaries[&choose].dependencies[0].sources[0]
            .parameter_index,
        1
    );
}

#[test]
fn imported_declaration_changes_invalidate_the_declaration_boundary() {
    let mut db = DatabaseImpl::new();
    let library = db.new_file(
        "math.aru".into(),
        "module math\npublic func add(a: int): int { return a }".into(),
    );
    let caller = db.new_file(
        "caller.aru".into(),
        "import math\nfunc foo(): int { return 42 }".into(),
    );
    let add = function(&db, library, "add");
    let display = |db: &DatabaseImpl| {
        let signatures = declaration_signatures(db, caller);
        let ty = signatures.type_info.decl_types[&add];
        signatures
            .type_info
            .type_interner
            .display(ty, &signatures.symbols)
    };
    let before = display(&db);
    library.set_text(&mut db).to(Arc::from(
        "module math\npublic func add(a: i64): i64 { return a }",
    ));
    assert_ne!(before, display(&db));
}

#[test]
fn malformed_recovering_ide_still_receives_imported_flow_contracts() {
    let mut db = DatabaseImpl::new();
    let library = db.new_file("loans.aru".into(), loans("return right"));
    let choose = function(&db, library, "choose");
    let caller = db.new_file("caller.aru".into(), "module caller\nimport loans\nfunc foo(): int { return 42 }\nfunc editing(): int { return 0 }ss".into());
    assert!(
        parse(&db, caller).is_err(),
        "fixture must exercise recovering parsing"
    );
    let checked = ide_type_check(&db, caller);
    let summary = checked
        .type_info
        .return_borrow_summaries
        .get(&choose)
        .expect("recovering IDE contract");
    assert_eq!(summary.dependencies[0].sources[0].parameter_index, 1);
}

#[test]
fn declaration_cycles_remain_diagnostic_after_edits_and_match_clean() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let a = db.new_file(
        "a.aru".into(),
        "module a\nimport b\npublic func fa(): int { return 1 }".into(),
    );
    db.new_file(
        "b.aru".into(),
        "module b\nimport a\npublic func fb(): int { return 2 }".into(),
    );
    let diagnostics = |db: &DatabaseImpl, file| {
        let mut diagnostics = declaration_signatures(db, file)
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.message.clone()))
            .collect::<Vec<_>>();
        diagnostics.sort();
        diagnostics
    };
    let initial = diagnostics(&db, a);
    assert!(!initial.is_empty(), "cycle must be diagnosed");
    a.set_text(&mut db).to(Arc::from(
        "module a\nimport b\npublic func fa(): int { return 3 }",
    ));
    assert_eq!(diagnostics(&db, a), initial);
    a.set_text(&mut db)
        .to(Arc::from("module a\npublic func fa(): int { return 3 }"));
    assert!(diagnostics(&db, a).is_empty());
    let mut clean = DatabaseImpl::new();
    clean.new_file(
        "b.aru".into(),
        "module b\nimport a\npublic func fb(): int { return 2 }".into(),
    );
    let clean_a = clean.new_file(
        "a.aru".into(),
        "module a\npublic func fa(): int { return 3 }".into(),
    );
    assert_eq!(diagnostics(&db, a), diagnostics(&clean, clean_a));
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    assert_eq!(log.count_executions_matching("borrow_interfaces"), 0);
}

#[test]
fn transitive_imports_preserve_the_selected_owner_and_ownership_diagnostics() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file(
        "leaf.aru".into(),
        "module leaf\npublic func choose(left: ref int, right: ref int): ref int { return right }"
            .into(),
    );
    let middle = db.new_file("middle.aru".into(), "module middle\nimport leaf\npublic func forward(left: ref int, right: ref int): ref int { return leaf.choose(left, right) }".into());
    let source = |owner: &str| {
        format!("import middle\nfunc main(): int {{ let mut left = 1\nlet mut right = 2\nlet borrowed = middle.forward(left, right)\n{owner} = 3\nreturn *borrowed }}")
    };
    let caller = db.new_file("caller.aru".into(), source("left"));
    let forward = function(&db, middle, "forward");
    let declared = declaration_signatures(&db, caller);
    assert!(declared.type_info.decl_types.contains_key(&forward));
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    let checked = module_signatures(&db, caller);
    let summary = checked
        .type_info
        .return_borrow_summaries
        .get(&forward)
        .expect("transitive contract");
    assert_eq!(summary.dependencies[0].sources[0].parameter_index, 1);
    let diagnostics = arandu_query::file_ide_diagnostics(&db, caller);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error as u8),
        "unborrowed left owner should be mutable: {:?}",
        **diagnostics
    );
    caller.set_text(&mut db).to(Arc::from(source("right")));
    let diagnostics = arandu_query::file_ide_diagnostics(&db, caller);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "O003"),
        "mutating the borrowed right owner must be rejected: {:?}",
        **diagnostics
    );
    caller.set_text(&mut db).to(Arc::from(source("left")));
    assert!(!arandu_query::file_ide_diagnostics(&db, caller)
        .iter()
        .any(|diagnostic| diagnostic.code == "O003"));
}

#[test]
fn scalar_module_contracts_do_not_request_any_body_or_hir() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "math.aru".into(),
        "module math\npublic func add(a: int, b: int): int { return a + b }".into(),
    );
    assert!(arandu_query::borrow_interfaces(&db, file)
        .entries
        .is_empty());
    for forbidden in [
        "prepare_hir",
        "item_typing",
        "item_body_typeck",
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
fn preparatory_file_typing_and_attributes_do_not_request_callee_flow() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let _ = db.new_file("loans.aru".into(), loans("return right"));
    let caller = db.new_file("caller.aru".into(),
        "module caller\nimport loans\nfunc forward(left: ref int, right: ref int): ref int { return loans.choose(left, right) }\n@Test\nfunc testSmoke(): void {}".into());
    let typed = arandu_query::passes::file_typing(&db, caller);
    assert!(
        !typed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "{:?}",
        typed.diagnostics
    );
    assert_eq!(log.count_executions_matching("item_typing"), 2);
    for forbidden in [
        "module_signatures",
        "borrow_interfaces",
        "prepare_hir",
        "item_body_typeck",
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
fn contract_projection_does_not_certify_an_unsafe_sibling_for_execution() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "loans.aru".into(),
        format!(
            "{}\nfunc unsafeCaller(): int {{
        let mut left = 1
        let mut right = 2
        let borrowed = choose(left, right)
        right = 3
        return *borrowed
    }}",
            loans("return right")
        ),
    );
    let choose = function(&db, file, "choose");
    let interfaces = arandu_query::borrow_interfaces(&db, file);
    let summary = interfaces
        .entries
        .iter()
        .find(|(symbol, _)| *symbol == choose)
        .expect("derived contract");
    assert_eq!(summary.1.dependencies[0].sources[0].parameter_index, 1);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
    assert!(arandu_query::passes::borrow_interfaces::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(&db, file)
    .is_empty());
    // Contract projection typed only the borrow-returning body. The final path
    // must independently type/validate the unsafe scalar sibling.
    log.clear();
    let lowered = arandu_query::lower_amir(&db, file);
    assert!(lowered.amir.funcs.is_empty());
    let diagnostics = arandu_query::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(&db, file);
    assert!(diagnostics
        .iter()
        .any(|d| d.0.code == arandu_middle::DiagCode::O003MutableBorrowConflict));
    assert_eq!(log.count_executions_matching("prepare_hir"), 0);
    assert_eq!(log.count_executions_matching("item_typing"), 1);
}

#[test]
fn changing_only_an_imported_origin_recomposes_metadata_without_retyping_the_caller() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let library = db.new_file("loans.aru".into(), loans("return left"));
    let caller = db.new_file("caller.aru".into(),
        "module caller\nimport loans\nfunc forward(left: ref int, right: ref int): ref int { return loans.choose(left, right) }".into());
    let forward = function(&db, caller, "forward");
    let _ = arandu_query::item_body_typeck(&db, caller, forward);
    log.clear();
    library
        .set_text(&mut db)
        .to(Arc::from(loans("return right")));
    let updated = arandu_query::item_body_typeck(&db, caller, forward);
    let choose = function(&db, library, "choose");
    assert_eq!(
        updated.type_info.return_borrow_summaries[&choose].dependencies[0].sources[0]
            .parameter_index,
        1
    );
    assert_eq!(
        log.count_executions_matching("item_typing"),
        1,
        "only the changed library body needs typing: {}",
        log.format_chain(true)
    );
    assert_eq!(log.count_executions_matching("item_body_typeck"), 1);
    assert_eq!(log.count_executions_matching("lower_amir"), 0);
}

#[test]
fn projected_recursive_and_monomorphized_contracts_equal_final_contracts() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "recursive.aru".into(),
        "
        func identity<T>(value: ref T): ref T { return value }
        func choose(depth: int, left: ref int, right: ref int): ref int {
            if depth == 0 { return identity(left) }
            return choose(depth - 1, right, left)
        }
        func wrap(value: ref int): Option<ref int> { return Option.Some(value) }
    "
        .into(),
    );
    let projected = arandu_query::borrow_interfaces(&db, file);
    let lowered = arandu_query::lower_amir(&db, file);
    assert!(!lowered.amir.funcs.is_empty());
    assert!(!projected.entries.is_empty());
    assert_eq!(
        projected.entries.len() + projected.instances.len(),
        lowered.type_check.type_info.return_borrow_summaries.len()
    );
    for (symbol, summary) in &projected.entries {
        assert_eq!(
            Some(summary),
            lowered
                .type_check
                .type_info
                .return_borrow_summaries
                .get(symbol)
        );
    }
    let composed = arandu_query::runtime::runtime_program(&db, file);
    for (key, summary) in &projected.instances {
        let (symbol, _) = composed
            .instances
            .iter()
            .find(|(_, instance)| instance == key)
            .expect("canonical instance");
        assert_eq!(
            Some(summary),
            lowered
                .type_check
                .type_info
                .return_borrow_summaries
                .get(symbol)
        );
    }
}
