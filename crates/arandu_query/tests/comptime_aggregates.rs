#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{amir::AmirStmt, ctfe::ConstValue, Severity};
use arandu_query::{
    passes::{lower_amir, type_check},
    DatabaseImpl,
};
use salsa::Setter;

fn checked(db: &DatabaseImpl, file: arandu_query::SourceFile) -> Vec<ConstValue> {
    assert!(
        arandu_query::passes::parse(db, file).is_ok(),
        "{:?}",
        arandu_query::passes::parse(db, file).as_ref().err()
    );
    let typed = type_check(db, file);
    let errors: Vec<_> = typed
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let prepared = arandu_query::passes::prepare_hir(db, file);
    assert!(
        prepared
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{:?}",
        prepared.diagnostics
    );
    let result = lower_amir(db, file);
    let emitted = lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(db, file);
    assert!(
        emitted.iter().all(|d| d.0.severity != Severity::Error),
        "{:?}; source: {}",
        emitted.iter().map(|d| &d.0).collect::<Vec<_>>(),
        file.text(db)
    );
    let errors: Vec<_> = result
        .type_check
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let main = result
        .amir
        .funcs
        .iter()
        .find(|f| result.type_check.symbols.get(f.symbol).name == "main")
        .unwrap_or_else(|| {
            panic!(
                "main missing: {:?}, funcs: {:?}",
                arandu_query::runtime::runtime_program(db, file).diagnostics,
                result
                    .amir
                    .funcs
                    .iter()
                    .map(|f| (f.symbol, &result.type_check.symbols.get(f.symbol).name))
                    .collect::<Vec<_>>()
            )
        });
    assert!(main
        .stmts
        .iter_ids()
        .all(|id| !matches!(main.stmt(id), AmirStmt::Call { .. })));
    let mut values: Vec<_> = typed
        .type_info
        .ctfe_values
        .iter()
        .map(|(span, (value, _))| (span.start, value.clone()))
        .collect();
    values.sort_by_key(|(start, _)| *start);
    values.into_iter().map(|(_, value)| value).collect()
}

#[test]
fn arrays_nested_products_and_closed_structs_materialize_in_destination_pools() {
    for source in [
        "func table(): [2]int { return [20, 22] }\nfunc main(): int { let a = comptime table(); return a[0] + a[1] }",
        "func pair(): (int, bool) { return 42, true }\nfunc main(): int { let a, enabled = comptime pair(); if enabled { return a }; return 0 }",
        "struct Point { x: int y: int }\nfunc make(): Point { return Point { y: 22, x: 20 } }\nfunc main(): int { let point = comptime make(); return point.x + point.y }",
        "func grid(): [2][2]int { return [[1, 2], [20, 22]] }\nfunc main(): int { let a = comptime grid(); return a[1][0] + a[1][1] }",
        "func main(): int { let a = comptime { let mut a: [2]int = [2, 3]; set a[1] = 40; return a }; return a[0] + a[1] }",
        "func main(): int { let a = comptime [20, 22]; return a[0] + a[1] }",
        "func main(): int { let a = comptime { let mut a = [2, 3]; set a[1] = 40; return a }; return a[0] + a[1] }",
        "func empty(): [0]int { return [] }\nfunc main(): int { let a = comptime empty(); return 42 }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("products.aru".into(), source.into());
        let values = checked(&db, file);
        assert_eq!(values.len(), 1, "{source}");
        assert!(matches!(values[0], ConstValue::Aggregate(_)), "{values:?}");
    }
}

#[test]
fn frozen_products_follow_target_width_and_are_revalidated_after_target_edits() {
    use arandu_middle::layout::DataLayout;

    let source = "struct Pair { small: usize wide: u64 }\nfunc make(): Pair { return Pair { small: 20, wide: 22 } }\nfunc main(): int { let pair = comptime make(); return 42 }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("target-products.aru".into(), source.into());
    let mut values = Vec::new();
    for width in [4, 8] {
        db.set_target_config(DataLayout::ptr_width(width));
        let result = checked(&db, file);
        let ConstValue::Aggregate(aggregate) = &result[0] else {
            panic!("expected frozen struct");
        };
        let ConstValue::Integer(small) = aggregate.values()[0] else {
            panic!("expected target-sized field");
        };
        let ConstValue::Integer(wide) = aggregate.values()[1] else {
            panic!("expected fixed-width field");
        };
        assert_eq!(u64::from(small.ty().bit_width()), width * 8);
        assert_eq!(wide.ty().bit_width(), 64);
        let mut clean = DatabaseImpl::new();
        clean.set_target_config(DataLayout::ptr_width(width));
        let clean_file = clean.new_file("target-products.aru".into(), source.into());
        assert_eq!(result, checked(&clean, clean_file));
        values.push(result);
    }
    assert_ne!(
        values[0], values[1],
        "target width is part of frozen identity"
    );
}

#[test]
fn frozen_array_helper_edits_match_clean_and_keep_equal_values_equal() {
    let source = "func table(): [2]int { return [20, 22] }\nfunc main(): int { let a = comptime table(); return a[0] + a[1] }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("products.aru".into(), source.into());
    let first = checked(&db, file);
    let equal = source.replace("[20, 22]", "[20, 11 + 11]");
    file.set_text(&mut db).to(equal.into());
    assert_eq!(checked(&db, file), first);
    let changed = source.replace("[20, 22]", "[20, 23]");
    file.set_text(&mut db).to(changed.clone().into());
    let incremental = checked(&db, file);
    assert_ne!(incremental, first);
    let mut clean = DatabaseImpl::new();
    let file = clean.new_file("products.aru".into(), changed);
    assert_eq!(incremental, checked(&clean, file));
}

#[test]
fn resource_structs_and_out_of_bounds_products_fail_without_ice() {
    for source in [
        "struct Resource { x: int }\n@Destructor\nfunc Resource.destroy(self: Resource): void {}\nfunc make(): Resource { return Resource { x: 42 } }\nfunc main(): void { let value = comptime make() }",
        "func main(): int { return comptime { let a = [20, 22]; a[4] } }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("invalid.aru".into(), source.into());
        let typed = type_check(&db, file);
        let errors: Vec<_> = typed
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(!errors.is_empty(), "{source}");
        assert!(
            errors.iter().all(|d| !d.code.as_str().starts_with("ICE")),
            "{errors:?}"
        );
        assert!(typed.type_info.ctfe_values.is_empty());
    }
}

#[test]
fn equal_field_types_reordered_in_nominal_metadata_do_not_alias_cached_units() {
    let source = "struct Point { x: int y: int }\nfunc make(): Point { return Point { x: 20, y: 22 } }\nfunc main(): int { let point = comptime make(); return point.x + point.y }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("fields.aru".into(), source.into());
    let before = checked(&db, file);
    let edited = source.replace("x: int y: int", "y: int x: int");
    file.set_text(&mut db).to(edited.clone().into());
    let incremental = checked(&db, file);
    assert_ne!(
        before, incremental,
        "frozen fields follow declaration order"
    );
    let mut clean = DatabaseImpl::new();
    let file = clean.new_file("fields.aru".into(), edited);
    assert_eq!(incremental, checked(&clean, file));
}

#[test]
fn enums_options_results_and_named_payloads_evaluate_and_materialize() {
    for source in [
        "enum Choice { First, Second }\nfunc choose(): Choice { return Choice.Second }\nfunc main(): int { let c = comptime choose(); return match c { Choice.First => 0\n Choice.Second => 42 } }",
        "enum Choice { Value(int), Empty }\nfunc choose(): Choice { return Choice.Value(42) }\nfunc main(): int { let c = comptime choose(); return match c { Choice.Value(x) => x\n Choice.Empty => 0 } }",
        "enum Choice { Pair(int, bool), Empty }\nfunc choose(): Choice { return Choice.Pair(42, true) }\nfunc main(): int { let c = comptime choose(); return match c { Choice.Pair(x, enabled) => x\n Choice.Empty => 0 } }",
        "func choose(): Option<int> { return Option.Some(42) }\nfunc main(): int { let c = comptime choose(); return match c { Option.Some(x) => x\n Option.None => 0 } }",
        "func choose(): Option<int> { return Option.None }\nfunc main(): int { let c = comptime choose(); return match c { Option.Some(x) => x\n Option.None => 42 } }",
        "func choose(): Result<int, str> { return Result.Ok(42) }\nfunc main(): int { let c = comptime choose(); return match c { Result.Ok(x) => x\n Result.Err(e) => 0 } }",
        "enum Choice { Pair(int, bool), Empty }\nfunc answer(): int { let c = Choice.Pair(42, true); return match c { Choice.Pair(x, enabled) => x\n Choice.Empty => 0 } }\nfunc main(): int { return comptime answer() }",
        "func answer(): int { let c: Option<int> = Option.Some(42); return match c { Option.Some(x) => x\n Option.None => 0 } }\nfunc main(): int { return comptime answer() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("enums.aru".into(), source.into());
        let values = checked(&db, file);
        assert_eq!(values.len(), 1, "{source}");
    }
}

#[test]
fn computed_array_dimensions_are_frozen_before_local_typing() {
    for source in [
        "func main(): int { let values: [comptime (1 + 1)]int = [20, 22]; return values[0] + values[1] }",
        "func size(): uint { return 2 }\nfunc main(): int { let values: [comptime (size())]int = [20, 22]; return values[0] + values[1] }",
        "func main(): int { let values: [comptime (@sizeOf(u16))]int = [20, 22]; return values[0] + values[1] }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("dimensions.aru".into(), source.into());
        checked(&db, file);
    }
}

#[test]
fn nested_value_roots_have_local_returns_and_share_the_enclosing_vm() {
    for source in [
        "func main(): int { return comptime { let x = comptime 20; x + 22 } }",
        "func main(): int { return comptime { let x = 20; let y = comptime (x + 1); y + 21 } }",
        "func main(): int { return comptime { let x = comptime { return 20 }; x + 22 } }",
        "func main(): int { return comptime { let x = comptime { if true { return 20 } else { return 10 } }; x + 22 } }",
        "func main(): int { return comptime { let x = comptime { 20 }; x + 22 } }",
        "func main(): int { return comptime { comptime { 20; }; 42 } }",
        "func helper(): int { let x = comptime { return 20 }; return x + 22 }\nfunc main(): int { return comptime helper() }",
        "func helper(): int { return comptime (20 + 22) }\nfunc main(): int { return comptime helper() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("nested.aru".into(), source.into());
        let values = checked(&db, file);
        assert!(values.iter().any(|value| matches!(value, ConstValue::Integer(integer) if integer.value() == 42)), "{source}: {values:?}");
    }
}

#[test]
fn global_initializers_are_evaluated_without_runtime_calls() {
    for source in [
        "const answer = comptime (20 + 22)\nfunc main(): int { return answer }",
        "func answer(): int { return 42 }\nconst VALUE = comptime answer()\nfunc main(): int { return VALUE }",
        "func answer(): int { return 42 }\nconst VALUE = answer()\nfunc main(): int { return VALUE }",
        "func count<comptime N: uint>(): uint { return N }\nconst VALUE = count<comptime (42)>()\nfunc main(): uint { return VALUE }",
        "const BASE = 20\nconst VALUE = comptime (BASE + 22)\nfunc main(): int { return VALUE }",
        "const BASE = comptime 20\nconst VALUE = comptime (BASE + 22)\nfunc main(): int { return VALUE }",
        "func table(): [2]int { return [20, 22] }\nconst VALUES = comptime table()\nfunc main(): int { return VALUES[0] + VALUES[1] }",
        "enum Choice { Value(int), Empty }\nfunc choose(): Choice { return Choice.Value(42) }\nconst VALUE = comptime choose()\nfunc main(): int { return match VALUE { Choice.Value(x) => x\nChoice.Empty => 0 } }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("globals.aru".into(), source.into());
        checked(&db, file);
    }
}

#[test]
fn cyclic_global_constants_fail_deterministically_without_ice() {
    for source in [
        "const A = comptime A\nfunc main(): int { return A }",
        "const A = comptime (B + 1)\nconst B = comptime (A + 1)\nfunc main(): int { return A }",
        "func value(): int { return A }\nconst A = comptime value()\nfunc main(): int { return A }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("cycles.aru".into(), source.into());
        let first = type_check(&db, file);
        assert!(
            first
                .diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::T044ComptimeEvaluationFailed),
            "{source}: {:?}",
            first.diagnostics
        );
        assert!(first
            .diagnostics
            .iter()
            .all(|d| !d.code.as_str().starts_with("ICE")));
        assert_eq!(first.diagnostics, type_check(&db, file).diagnostics);
    }
}

#[test]
fn imported_global_values_follow_helper_edits_and_cut_off_equal_values() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "module tables\nfunc value(): int { return 20 + 22 }\npublic const ANSWER = comptime value()";
    let definitions = db.new_file("tables.aru".into(), source.into());
    let file = db.new_file(
        "consumer.aru".into(),
        "import tables\nfunc main(): int { return comptime (tables.ANSWER) }".into(),
    );
    let before = checked(&db, file);
    log.clear();
    definitions
        .set_text(&mut db)
        .to(source.replace("20 + 22", "21 + 21").into());
    // Runtime preparation also rebuilds the edited helper itself. Measure
    // only the consumer typing boundary before requesting runtime modules.
    let _ = type_check(&db, file);
    assert_eq!(
        log.count_executions_matching("item_typing"),
        0,
        "{}",
        log.format_chain(false)
    );
    assert_eq!(before, checked(&db, file));
    definitions
        .set_text(&mut db)
        .to(source.replace("20 + 22", "21 + 22").into());
    let changed = checked(&db, file);
    assert_ne!(before, changed);
    let mut clean = DatabaseImpl::new();
    clean.new_file("tables.aru".into(), source.replace("20 + 22", "21 + 22"));
    let fresh = clean.new_file(
        "consumer.aru".into(),
        "import tables\nfunc main(): int { return comptime (tables.ANSWER) }".into(),
    );
    assert_eq!(changed, checked(&clean, fresh));
}

#[test]
fn imported_signatures_track_aggregate_argument_contents_with_equal_type_display() {
    use arandu_middle::types::TypeShape;
    let source = "module tokens\npublic struct Policy { enabled: bool }\npublic struct Token<comptime P: Policy> { value: int }\npublic func make(): Token<comptime (Policy { enabled: true })> { return Token<comptime (Policy { enabled: true })> { value: 42 } }";
    let consumer =
        "import tokens\nfunc main(): int { let token = tokens.make(); return token.value }";
    let mut db = DatabaseImpl::new();
    let definitions = db.new_file("tokens.aru".into(), source.into());
    let file = db.new_file("consumer.aru".into(), consumer.into());
    let signature = |db: &DatabaseImpl, file| {
        let typed = type_check(db, file);
        assert!(
            typed
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error),
            "{:?}",
            typed.diagnostics
        );
        let (&symbol, &ty) = typed
            .type_info
            .decl_types
            .iter()
            .find(|(symbol, _)| {
                typed
                    .symbols
                    .try_get(**symbol)
                    .is_some_and(|entry| entry.name == "make")
            })
            .expect("imported function signature");
        let _ = symbol;
        (
            TypeShape::from_id(ty, &typed.type_info.type_interner).expect("structural signature"),
            typed.type_info.type_interner.display(ty, &typed.symbols),
        )
    };
    let before = signature(&db, file);
    let edited = source.replace("enabled: true", "enabled: false");
    definitions.set_text(&mut db).to(edited.clone().into());
    let after = signature(&db, file);
    assert_eq!(before.1, after.1);
    assert_ne!(before.0, after.0);
    let mut fresh_db = DatabaseImpl::new();
    fresh_db.new_file("tokens.aru".into(), edited);
    let fresh_file = fresh_db.new_file("consumer.aru".into(), consumer.into());
    assert_eq!(after, signature(&fresh_db, fresh_file));
}

#[test]
fn target_properties_use_explicit_inputs_and_invalidate_staged_values() {
    use arandu_middle::{db::TargetIdentity, layout::DataLayout};
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    db.set_target_identity(TargetIdentity {
        os: "windows".into(),
        arch: "x86".into(),
    });
    let file = db.new_file("target.aru".into(), "func main(): int { let os = comptime @targetOS(); let arch = comptime @targetArch(); let bits = comptime @targetPointerWidth(); comptime if @targetOS() == \"windows\" { return 42 } else { return unknown } }".into());
    let first = checked(&db, file);
    assert!(first
        .iter()
        .any(|value| matches!(value, ConstValue::String(s) if s.as_str() == "windows")));
    assert!(first
        .iter()
        .any(|value| matches!(value, ConstValue::Integer(n) if n.to_const_generic() == Ok(32))));
    db.set_target_identity(TargetIdentity {
        os: "linux".into(),
        arch: "aarch64".into(),
    });
    let changed = type_check(&db, file);
    assert!(changed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error));
    let mut clean = DatabaseImpl::new();
    clean.set_target_config(DataLayout::ptr_width(4));
    clean.set_target_identity(TargetIdentity {
        os: "linux".into(),
        arch: "aarch64".into(),
    });
    let fresh = clean.new_file("target.aru".into(), file.text(&db).to_string());
    assert_eq!(changed.diagnostics, type_check(&clean, fresh).diagnostics);
}

#[test]
fn typed_constant_arguments_specialize_bool_negative_and_copy_values() {
    for source in [
        "func choose<comptime FLAG: bool>(): int { if FLAG { return 42 }; return 0 }\nfunc main(): int { return comptime choose<comptime (true)>() }",
        "func choose<comptime N: int>(): int { return N + 43 }\nfunc main(): int { return comptime choose<comptime (-1)>() }",
        "struct Policy { enabled: bool value: int }\nfunc choose<comptime P: Policy>(): int { if P.enabled { return P.value }; return 0 }\nfunc main(): int { return comptime choose<comptime (Policy { enabled: true, value: 42 })>() }",
        "enum Policy { Value(int), Empty }\nfunc choose<comptime P: Policy>(): int { return match P { Policy.Value(x) => x\nPolicy.Empty => 0 } }\nfunc main(): int { return comptime choose<comptime (Policy.Value(42))>() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("typed_keys.aru".into(), source.into());
        let values = checked(&db, file);
        assert!(values.iter().any(|value| matches!(value, ConstValue::Integer(n) if n.value() == 42)), "{source}: {values:?}");
    }
}

#[test]
fn computed_array_headers_are_frozen_before_body_typing() {
    for source in [
        "func main(): int { let a: [comptime (1 + 1)]int = [20, 22]; return a[0] + a[1] }",
        "func table(): [comptime (1 + 1)]int { return [20, 22] }\nfunc main(): int { let a = comptime table(); return a[0] + a[1] }",
        "struct Table { entries: [comptime (1 + 1)]int }\nfunc make(): Table { return Table { entries: [20, 22] } }\nconst VALUE = comptime make()\nfunc main(): int { return VALUE.entries[0] + VALUE.entries[1] }",
        "struct Table { entries: [comptime (1 + 1)]int }\nfunc main(): int { let a = Table { entries: [20, 22] }; return a.entries[0] + a.entries[1] }",
        "type Table = [comptime (1 + 1)]int\nfunc main(): int { let a: Table = [20, 22]; return a[0] + a[1] }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("headers.aru".into(), source.into());
        checked(&db, file);
    }
}

#[test]
fn inline_namespaces_do_not_duplicate_child_header_obligations() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("namespace_headers.aru".into(), "module tables { public func pair(): [comptime (1 + 1)]int { return [20, 22] } }\nfunc main(): int { return 0 }".into());
    let typed = type_check(&db, file);
    assert!(
        typed
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "{:?}",
        typed.diagnostics
    );
    assert_eq!(
        log.count_executions_matching("item_header_arguments"),
        1,
        "{}",
        log.format_chain(true)
    );
}

#[test]
fn result_and_option_propagation_preserve_variant_paths_in_ctfe() {
    for source in [
        "func input(): Result<int, int> { return Result.Ok(42) }\nfunc forward(): Result<int, int> { let value = input()?; return Result.Ok(value) }\nfunc main(): int { let value = comptime forward(); return match value { Result.Ok(x) => x\nResult.Err(x) => x } }",
        "func input(): Result<int, int> { return Result.Err(42) }\nfunc forward(): Result<int, int> { let value = input()?; return Result.Ok(value) }\nfunc main(): int { let value = comptime forward(); return match value { Result.Ok(x) => x\nResult.Err(x) => x } }",
        "func input(): Option<int> { return Option.None }\nfunc forward(): Option<int> { let value = input()?; return Option.Some(value) }\nfunc main(): int { let value = comptime forward(); return match value { Option.Some(x) => x\nOption.None => 42 } }",
        "func input(): Result<int, int> { return Result.Err(42) }\nfunc forward(): int { let inner: Result<int, int> = comptime { let value = input()?; return Result.Ok(value) }; return match inner { Result.Ok(x) => x\nResult.Err(x) => x } }\nfunc main(): int { return comptime forward() }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("propagation.aru".into(), source.into());
        checked(&db, file);
    }
}

#[test]
fn unannotated_nested_comptime_blocks_validate_propagation_after_result_inference() {
    for variant in ["Ok", "Err"] {
        let mut db = DatabaseImpl::new();
        let source = format!(
            "func input(): Result<int, int> {{ return Result.{variant}(42) }}\nfunc output(value: int): Result<int, int> {{ return Result.Ok(value) }}\nfunc forward(): int {{ let inner = comptime {{ let value = input()?; return output(value) }}; return match inner {{ Result.Ok(x) => x\nResult.Err(x) => x }} }}\nfunc main(): int {{ return comptime forward() }}"
        );
        let file = db.new_file("inferred_propagation.aru".into(), source);
        checked(&db, file);
    }
    let mut db = DatabaseImpl::new();
    let file = db.new_file("inferred_option.aru".into(), "func input(): Option<int> { return Option.None }\nfunc output(value: int): Option<int> { return Option.Some(value) }\nfunc forward(): int { let inner = comptime { let value = input()?; return output(value) }; return match inner { Option.Some(x) => x\nOption.None => 42 } }\nfunc main(): int { return comptime forward() }".into());
    checked(&db, file);
}

#[test]
fn inferred_comptime_results_cannot_hide_incompatible_propagation() {
    for source in [
        "func input(): Result<int, int> { return Result.Err(42) }\nfunc main(): int { let inner = comptime { let value = input()?; return value }; return inner }",
        "func input(): Result<int, int> { return Result.Err(42) }\nfunc output(value: int): Result<int, bool> { return Result.Ok(value) }\nfunc main(): int { let inner = comptime { let value = input()?; return output(value) }; return 0 }",
        "func input(): Option<int> { return Option.None }\nfunc main(): int { let inner = comptime { let value = input()?; return value }; return inner }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("invalid_propagation.aru".into(), source.into());
        let typed = type_check(&db, file);
        assert!(typed.diagnostics.iter().any(|diagnostic| diagnostic.severity == Severity::Error), "{source}");
        assert!(typed.diagnostics.iter().all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")), "{:?}", typed.diagnostics);
    }
}

#[test]
fn inferred_propagation_does_not_constrain_the_success_channel() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("different_success.aru".into(), "func input(): Result<int, int> { return Result.Err(42) }\nfunc output(value: int): Result<bool, int> { return Result.Ok(value == 42) }\nfunc forward(): int { let inner = comptime { let value = input()?; return output(value) }; return match inner { Result.Ok(value) => if value { 42 } else { 0 }\nResult.Err(value) => value } }\nfunc main(): int { return comptime forward() }".into());
    checked(&db, file);
}

#[test]
fn failed_nominal_header_evaluation_propagates_without_ice() {
    for source in [
        "const N = comptime N\nstruct Table { values: [comptime (N)]int }\nfunc main(): int { let values = comptime Table { values: [42] }; return values.values[0] }",
        "func size(): uint { return 1 / 0 }\nstruct Table { values: [comptime (size())]int }\nfunc main(): int { let values = comptime Table { values: [42] }; return values.values[0] }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("failed_header.aru".into(), source.into());
        let typed = type_check(&db, file);
        assert!(typed.diagnostics.iter().any(|diagnostic| diagnostic.severity == Severity::Error), "{source}");
        assert!(typed.diagnostics.iter().all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")), "{:?}", typed.diagnostics);
    }
}

#[test]
fn signed_constant_keys_normalize_to_the_declared_parameter_domain() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("canonical_keys.aru".into(), "func value<comptime N: i64>(): i64 { return N }\nfunc main(): i64 { return value<comptime (-1)>() + value<comptime (-1 as i64)>() }".into());
    let typed = type_check(&db, file);
    assert!(
        typed
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "{:?}",
        typed.diagnostics
    );
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
    let instances: Vec<_> = program
        .instances
        .iter()
        .filter(|(_, key)| !key.arguments.is_empty())
        .collect();
    assert_eq!(instances.len(), 1, "{instances:?}");
}

#[test]
fn imported_global_cycles_report_ctfe_failure_in_both_registration_orders() {
    let sources = [
        (
            "left.aru",
            "module left\nimport right\npublic const VALUE = comptime (right.VALUE + 1)",
        ),
        (
            "right.aru",
            "module right\nimport left\npublic const VALUE = comptime (left.VALUE + 1)",
        ),
    ];
    for order in [[0, 1], [1, 0]] {
        let mut db = DatabaseImpl::new();
        for index in order {
            db.new_file(sources[index].0.into(), sources[index].1.into());
        }
        let file = db.new_file(
            "consumer.aru".into(),
            "import left\nfunc main(): int { return left.VALUE }".into(),
        );
        let typed = type_check(&db, file);
        assert!(
            typed.diagnostics.iter().any(|diagnostic| diagnostic.code
                == arandu_middle::DiagCode::T044ComptimeEvaluationFailed),
            "{:?}",
            typed.diagnostics
        );
        assert!(typed
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")));
        assert_eq!(typed.diagnostics, type_check(&db, file).diagnostics);
    }
}

#[test]
fn generic_local_array_dimensions_are_staged_per_instance() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("dependent_array.aru".into(), "func total<comptime N: uint>(): int { let entries: [comptime (N + 1)]int = [20, 22]; return entries[0] + entries[1] }\nfunc main(): int { return comptime total<1>() }".into());
    checked(&db, file);
}

#[test]
fn nested_computed_generic_arguments_freeze_inside_out() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("nested_arguments.aru".into(), "func count<comptime N: uint>(): uint { return N }\nfunc main(): uint { return count<comptime (count<comptime (40 + 2)>())>() }".into());
    let typed = type_check(&db, file);
    assert!(typed.diagnostics.is_empty(), "{:?}", typed.diagnostics);
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
}

#[test]
fn computed_generic_arguments_in_expression_conditions_are_staged() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("condition_arguments.aru".into(), "func count<comptime N: uint>(): uint { return N }\nfunc main(): int { if count<comptime (40 + 2)>() == 42 { return 42 } return 0 }".into());
    let typed = type_check(&db, file);
    assert!(typed.diagnostics.is_empty(), "{:?}", typed.diagnostics);
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
}

#[test]
fn imported_target_identity_is_available_before_static_selection() {
    let mut db = DatabaseImpl::new();
    db.new_file(
        "targets.aru".into(),
        include_str!("../../../stdlib/std/target.aru")
            .replace("module std.target", "module targets"),
    );
    let file = db.new_file("target_consumer.aru".into(), "import targets as target\nfunc main(): int { comptime if target.arch == \"wasm32\" { return 1 } else { return 0 } }".into());
    let symbol = arandu_query::passes::local_symbols(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "main")
        .expect("main")
        .id;
    let root = arandu_query::ctfe::CtfeRoot::new(
        &db,
        file,
        symbol,
        arandu_query::ctfe::RootSelector::StaticIfCondition(0),
        None,
    );
    let lowered = arandu_query::ctfe::ctfe_root_amir(&db, root);
    assert!(lowered.result.is_ok(), "{:?}", lowered.result);
    checked(&db, file);
}

#[test]
fn signed_nominal_constant_arguments_use_the_declared_domain() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("nominal_keys.aru".into(), "struct Marker<comptime N: i64> { value: int }\nfunc main(): int { let value: Marker<comptime (-1)> = Marker<comptime (-1 as i64)> { value: 42 }; return value.value }".into());
    checked(&db, file);
}
