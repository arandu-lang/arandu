#![allow(clippy::expect_used, clippy::panic)]
use arandu_middle::Severity;
use arandu_query::{
    passes::{lower_amir, type_check},
    DatabaseImpl,
};

fn check(source: &str) {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("concrete_headers.aru".into(), source.into());
    assert!(
        arandu_query::passes::parse(&db, file).is_ok(),
        "{:?}",
        arandu_query::passes::parse(&db, file).as_ref().err()
    );
    let typed = type_check(&db, file);
    let errors: Vec<_> = typed
        .diagnostics
        .iter()
        .filter(|error| error.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{source}: {errors:?}");
    let prepared = arandu_query::passes::prepare_hir(&db, file);
    assert!(
        prepared
            .diagnostics
            .iter()
            .all(|error| error.severity != Severity::Error),
        "{source}: {:?}",
        prepared.diagnostics
    );
    let _program = lower_amir(&db, file);
    let errors = lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file);
    assert!(
        errors
            .iter()
            .all(|error| error.0.severity != Severity::Error),
        "{source}: {:?}",
        errors.iter().map(|error| &error.0).collect::<Vec<_>>()
    );
}

#[test]
fn computed_function_headers_are_concrete_before_call_checking() {
    check("func pair<comptime N: uint>(): [comptime (N * 2)]int { return [20, 22] }\nfunc main(): int { let p: [2]int = pair<1>(); return p[0] + p[1] }");
}

#[test]
fn computed_headers_use_concrete_type_arguments() {
    check("func bytes<T>(): [comptime (@sizeOf(T))]u8 { return [1; comptime (@sizeOf(T))] }\nfunc main(): int { let a: [4]u8 = bytes<u32>(); let b: [8]u8 = bytes<u64>(); return (a[0] + b[0]) as int }");
}

#[test]
fn two_nominal_instances_keep_distinct_field_dimensions() {
    check("struct Matrix<comptime R: uint, comptime C: uint> { data: [comptime (R * C)]int }\nfunc main(): int { let a = Matrix<1, 2> { data: [20, 22] }; let b = Matrix<2, 2> { data: [1, 2, 3, 4] }; return a.data[0] + a.data[1] + b.data[0] - 1 }");
}

#[test]
fn forwarding_specializes_the_callee_header() {
    check("func pair<comptime N: uint>(): [comptime (N * 2)]int { return [20, 22] }\nfunc forward<comptime M: uint>(): [comptime (M * 2)]int { return pair<M>() }\nfunc main(): int { let p = forward<1>(); return p[0] + p[1] }");
}

#[test]
fn inferred_type_arguments_freeze_return_headers() {
    check("func bytes<T>(x: T): [comptime (@sizeOf(T))]u8 { return [1; comptime (@sizeOf(T))] }\nfunc main(): int { let x: u32 = 42; let b: [4]u8 = bytes(x); return b[0] as int }");
}

#[test]
fn computed_aliases_and_enum_payloads_are_instantiated() {
    check("type Words<comptime N: uint> = [comptime (N * 2)]int\nfunc main(): int { let x: Words<1> = [20, 22]; return x[0] + x[1] }");
    check("enum Packet<comptime N: uint> { Data([comptime (N * 2)]int), Empty }\nfunc packet(): Packet<1> { return .Data([20, 22]) }\nfunc main(): int { return match packet() { Packet.Data(x) => x[0] + x[1]\nPacket.Empty => 0 } }");
}

#[test]
fn nominal_contracts_are_available_inside_ctfe() {
    check("struct Matrix<comptime N: uint> { data: [comptime (N * 2)]int }\nfunc answer(): int { let x = Matrix<1> { data: [20, 22] }; return x.data[0] + x.data[1] }\nfunc main(): int { return comptime answer() }");
}

#[test]
fn transitive_contracts_and_layout_intrinsics_share_metadata() {
    check("struct Row<comptime N: uint> { data: [comptime (N * 2)]u8 }\nstruct Table<comptime N: uint> { rows: [comptime (N + 1)]Row<N> }\nfunc main(): int { let n = comptime @sizeOf(Table<2>); return n as int }");
}

#[test]
fn independent_headers_of_generic_functions_remain_valid() {
    check("func fixed<T>(x: T): [comptime (1 + 1)]int { return [20, 22] }\nfunc main(): int { let p = fixed<int>(0); return p[0] + p[1] }");
}

#[test]
fn imported_contracts_invalidate_by_concrete_value_and_recover() {
    use salsa::Setter;
    let mut db = DatabaseImpl::new();
    let definition = "public func width(): uint { return 2 }\npublic func pair<comptime N: uint>(): [comptime (N * width())]int { return [20, 22] }";
    let imported = db.new_file("headers.aru".into(), definition.into());
    let consumer = db.new_file(
        "consumer.aru".into(),
        "import headers as h\nfunc main(): int { let p: [2]int = h.pair<1>(); return p[0] + p[1] }"
            .into(),
    );
    let clean = |db: &DatabaseImpl| {
        type_check(db, consumer)
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error)
    };
    assert!(clean(&db), "{:?}", type_check(&db, consumer).diagnostics);
    imported
        .set_text(&mut db)
        .to(definition.replace("return 2", "return 1 + 1").into());
    assert!(clean(&db));
    imported
        .set_text(&mut db)
        .to(definition.replace("return 2", "return 3").into());
    assert!(!clean(&db));
    imported.set_text(&mut db).to(definition.into());
    assert!(clean(&db));
}

#[test]
fn declaration_cycles_and_deep_acyclic_dependencies_fail_without_ice() {
    use arandu_middle::DiagCode;
    let mut chain = String::new();
    for i in 0..arandu_query::ctfe::MAX_QUERY_DEPENDENCY_DEPTH + 2 {
        chain.push_str(&format!(
            "struct S{i}<comptime N: uint> {{ data: [comptime (@sizeOf(S{}<N>))]u8 }}\n",
            i + 1
        ));
    }
    chain.push_str(&format!("struct S{}<comptime N: uint> {{ data: [N]u8 }}\nfunc main(): int {{ return @sizeOf(S0<1>) as int }}", arandu_query::ctfe::MAX_QUERY_DEPENDENCY_DEPTH + 2));
    for (source, expected) in [
        ("struct Cycle<comptime N: uint> { data: [comptime (@sizeOf(Cycle<N>))]u8 }\nfunc main(): int { return @sizeOf(Cycle<1>) as int }".to_string(), DiagCode::T044ComptimeEvaluationFailed),
        (chain, DiagCode::T045ComptimeLimitExceeded),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("invalid_headers.aru".into(), source.clone());
        let result = type_check(&db, file);
        assert!(result.diagnostics.iter().any(|d| d.code == expected), "{source}: {:?}", result.diagnostics);
        assert!(result.diagnostics.iter().all(|d| !d.code.as_str().starts_with("ICE")), "{:?}", result.diagnostics);
        assert_eq!(result.diagnostics, type_check(&db, file).diagnostics);
    }
}

#[test]
fn discarded_branches_do_not_demand_generic_headers() {
    check("func broken<comptime N: uint>(): [comptime (N / 0)]int { return [0] }\nfunc main(): int { comptime if false { return broken<1>()[0] } else { return 42 } }");
}

#[test]
fn ctfe_return_values_carry_their_nominal_contracts() {
    check("struct Matrix<comptime N: uint> { data: [comptime (N * 2)]int }\nfunc make(): Matrix<1> { return Matrix<1> { data: [20, 22] } }\nfunc main(): int { let x = comptime make(); return x.data[0] + x.data[1] }");
}

#[test]
fn equal_header_values_cut_off_callers_and_target_edits_change_layouts() {
    use salsa::Setter;
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let definition = "public func width(): uint { return 2 }\npublic func pair<comptime N: uint>(): [comptime (N * width())]int { return [20, 22] }";
    let imported = db.new_file("headers.aru".into(), definition.into());
    let consumer = db.new_file(
        "consumer.aru".into(),
        "import headers as h\nfunc main(): int { let p: [2]int = h.pair<1>(); return p[0] + p[1] }"
            .into(),
    );
    assert!(type_check(&db, consumer)
        .diagnostics
        .iter()
        .all(|d| d.severity != Severity::Error));
    log.clear();
    imported
        .set_text(&mut db)
        .to(definition.replace("return 2", "return 1 + 1").into());
    assert!(type_check(&db, consumer)
        .diagnostics
        .iter()
        .all(|d| d.severity != Severity::Error));
    assert_eq!(
        log.count_executions_matching("item_typing"),
        0,
        "equal contract must cut off callers"
    );

    let file = db.new_file("target_headers.aru".into(), "struct Bytes<T> { data: [comptime (@sizeOf(T))]u8 }\nfunc main(): int { return comptime (@sizeOf(Bytes<usize>) as int) }".into());
    for width in [4, 8] {
        db.set_target_config(arandu_middle::layout::DataLayout::ptr_width(width));
        let typed = type_check(&db, file);
        assert!(
            typed
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{:?}",
            typed.diagnostics
        );
        assert!(typed.type_info.ctfe_values.values().any(|(value, _)| matches!(value, arandu_middle::ctfe::ConstValue::Integer(value) if value.value() == i128::from(width))));
    }
}

#[test]
fn named_enum_payload_fields_and_static_occurrences_use_concrete_dimensions() {
    check("enum Packet<comptime N: uint> { Data { bytes: [comptime (N * 2)]int }, Empty }\nfunc packet(): Packet<1> { return Packet.Data { bytes: [20, 22] } }\nfunc main(): int { return match packet() { Packet.Data { bytes } => bytes[0] + bytes[1]\nPacket.Empty => 0 } }");
    check("func pair<comptime N: uint>(): [comptime (N * 2)]int { return [20, 22] }\nfunc main(): int { let mut x = 0; comptime for i in 0..2 { let p: [2]int = pair<1>(); x += p[0] + p[1] }; return x }");
}

#[test]
fn calculated_aliases_survive_nested_generic_declarations() {
    check("type Words<comptime N: uint> = [comptime (N * 2)]int\nfunc pair<comptime N: uint>(): Words<N> { return [20, 22] }\nfunc main(): int { let p = pair<1>(); return p[0] + p[1] }");
    check("type Words<comptime N: uint> = [comptime (N * 2)]int\nstruct Wrapper<comptime N: uint> { data: Words<N> }\nfunc main(): int { let x = Wrapper<1> { data: [20, 22] }; return x.data[0] + x.data[1] }");
}

#[test]
fn failed_concrete_header_evaluations_recover_after_edits() {
    use salsa::Setter;
    let mut db = DatabaseImpl::new();
    let source = "func pair<comptime N: uint>(): [comptime (N / 0)]int { return [20, 22] }\nfunc main(): int { let p = pair<1>(); return p[0] + p[1] }";
    let file = db.new_file("header_recovery.aru".into(), source.into());
    let failed = type_check(&db, file);
    assert!(failed
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error));
    assert!(failed
        .diagnostics
        .iter()
        .all(|d| !d.code.as_str().starts_with("ICE")));
    let edited = source.replace("N / 0", "N * 2");
    file.set_text(&mut db).to(edited.clone().into());
    assert!(type_check(&db, file)
        .diagnostics
        .iter()
        .all(|d| d.severity != Severity::Error));
    check(&edited);
}

#[test]
fn non_generic_consumers_expand_instantiated_aliases() {
    check("type Words<comptime N: uint> = [comptime (N * 2)]int\nfunc pair(): Words<1> { return [20, 22] }\nfunc main(): int { let p: [2]int = pair(); return p[0] + p[1] }");
}

#[test]
fn inferred_enum_constructors_validate_the_selected_payload_dimension() {
    check("enum Packet<comptime N: uint> { Pair([comptime (N * 2)]int), Triple([comptime (N * 3)]int) }\nfunc main(): int { let p: Packet<1> = Packet.Pair([20, 22]); return match p { Packet.Pair(x) => x[0] + x[1]\nPacket.Triple(x) => x[0] } }");
    let mut db = DatabaseImpl::new();
    let file = db.new_file("wrong_payload.aru".into(), "enum Packet<comptime N: uint> { Pair([comptime (N * 2)]int), Triple([comptime (N * 3)]int) }\nfunc main(): int { let p: Packet<1> = Packet.Pair([1, 2, 3]); return 0 }".into());
    let checked = type_check(&db, file);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::T003IncompatibleCallArg),
        "{:?}",
        checked.diagnostics
    );
}

#[test]
fn inferred_generic_methods_use_their_own_header_contract() {
    check("struct Serializer { id: int }\nfunc Serializer.bytes<T>(self: Serializer, x: T): [comptime (@sizeOf(T))]u8 { return [1; comptime (@sizeOf(T))] }\nfunc main(): int { let s = Serializer { id: 0 }; let x: u32 = 42; let p: [4]u8 = s.bytes(x); return p[3] as int }");
}

#[test]
fn body_reference_composition_matches_cold_analysis_after_importer_edit() {
    use arandu_query::{AnalysisHost, StableHash};
    let dependency = "module dependency\npublic func value(): int { return 1 }\n";
    let consumer =
        "module consumer\nimport dependency\nfunc main(): int { return dependency.value() }\n";
    let edited = "module consumer\nimport dependency\nfunc main(): int { return dependency.value// edição 1 🦀\n() }\n";
    let mut warm = AnalysisHost::new();
    let dep = warm.new_file("dependency.aru".into(), dependency.into());
    let caller = warm.new_file("consumer.aru".into(), consumer.into());
    let _ = arandu_query::file_ide_diagnostics(warm.db(), dep);
    let _ = arandu_query::file_ide_diagnostics(warm.db(), caller);
    warm.set_text(
        dep,
        "module dependency\npublic func value(): int { return 0 }\n",
    );
    let _ = type_check(warm.db(), dep);
    let _ = type_check(warm.db(), caller);
    warm.set_text(caller, edited);
    let mut cold = AnalysisHost::new();
    let cold_dep = cold.new_file(
        "dependency.aru".into(),
        "module dependency\npublic func value(): int { return 0 }\n".into(),
    );
    let cold_caller = cold.new_file("consumer.aru".into(), edited.into());
    for (actual, expected) in [(dep, cold_dep), (caller, cold_caller)] {
        let actual = type_check(warm.db(), actual);
        let expected = type_check(cold.db(), expected);
        assert_eq!(actual.value.stable_hash(), expected.value.stable_hash());
    }
}

#[test]
fn concrete_header_discovery_keeps_global_initializers_acyclic() {
    for source in [
        "func pair<comptime N: uint>(): [comptime (N * 2)]int { return [20, 22] }\nconst VALUES = comptime pair<1>()\nfunc main(): int { return VALUES[0] + VALUES[1] }",
        "struct Row<comptime N: uint> { items: [comptime (N * 2)]int }\nfunc row<comptime N: uint>(): Row<N> { return Row<N> { items: [20, 22] } }\nconst VALUE = comptime row<1>()\nfunc main(): int { return VALUE.items[0] + VALUE.items[1] }",
        "struct Row<comptime N: uint> { items: [comptime (N * 2)]int }\nfunc row<comptime N: uint>(): Row<N> { return Row<N> { items: [20, 22] } }\nconst VALUE = comptime row<1>()\nconst ANSWER = comptime (VALUE.items[0] + VALUE.items[1])\nfunc main(): int { return ANSWER }",
    ] {
        check(source);
    }
}
