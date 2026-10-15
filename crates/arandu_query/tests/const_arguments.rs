#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{types::TypeShape, DiagCode, Severity, SymbolId};
use arandu_query::{ctfe::item_const_arguments, passes, runtime, DatabaseImpl, SourceFile};
use salsa::Setter;

const COUNT: &str = "func count<comptime N: uint>(): uint { return N }\n";

fn symbol(db: &DatabaseImpl, file: SourceFile, name: &str) -> SymbolId {
    passes::resolved_headers(db, file)
        .declarations
        .symbols
        .iter()
        .find(|s| s.name == name)
        .expect("symbol")
        .id
}

fn errors(db: &DatabaseImpl, file: SourceFile) -> Vec<arandu_middle::Diagnostic> {
    passes::type_check(db, file)
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect()
}

fn values(db: &DatabaseImpl, file: SourceFile) -> Vec<Option<u64>> {
    let staged = item_const_arguments(db, file, symbol(db, file, "main"));
    let mut values: Vec<_> = staged.values.iter().collect();
    values.sort_by_key(|(key, _)| key.start);
    values.into_iter().map(|(_, value)| *value).collect()
}

#[test]
fn nested_module_arguments_track_the_member_not_the_enclosing_module() {
    let source = "module math {\npublic func count<comptime N: uint>(): uint { return N }\npublic func value(): uint { return count<comptime (20 + 22)>() }\npublic func sibling(): int { return 10 }\n}\nfunc main(): int { return math.value() as int }";
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("nested.aru".into(), source.into());
    let owner = symbol(&db, file, "value");
    let frozen = |db: &DatabaseImpl| {
        item_const_arguments(db, file, owner)
            .values
            .values()
            .copied()
            .collect::<Vec<_>>()
    };
    assert_eq!(frozen(&db), [Some(42)]);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    let input = passes::item_source_input(&db, file, owner);
    assert_eq!(
        input.item_start,
        u32::try_from(source.find("func value").expect("member start")).expect("source offset")
    );
    let _ = passes::item_typing(&db, file, owner);
    log.clear();
    let sibling_edit = source.replace("return 10", "return 11");
    file.set_text(&mut db).to(sibling_edit.clone().into());
    assert_eq!(frozen(&db), [Some(42)]);
    let _ = passes::item_typing(&db, file, owner);
    assert_eq!(log.count_executions_matching("item_typing"), 0);
    assert_eq!(log.count_executions_matching("ctfe_eval_root"), 0);

    let member_edit = sibling_edit.replace("20 + 22", "20 + 23");
    file.set_text(&mut db).to(member_edit.clone().into());
    assert_eq!(frozen(&db), [Some(43)]);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("nested.aru".into(), member_edit);
    assert_eq!(errors(&db, file), errors(&clean, clean_file));
    let staged = item_const_arguments(&clean, clean_file, symbol(&clean, clean_file, "value"));
    assert_eq!(
        staged.values.values().copied().collect::<Vec<_>>(),
        [Some(43)]
    );
    assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
}

#[test]
fn computed_values_reuse_literal_instance_identity() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("arguments.aru".into(), format!("{COUNT}func add(a: int, b: int): int {{ return a + b }}\nfunc main(): uint {{ return count<comptime (add(20, 22))>() + count<42>() + count<comptime (43)>() }}"));
    assert_eq!(values(&db, file), [Some(42), Some(43)]);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    let program = runtime::runtime_program(&db, file);
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
    let count = symbol(&db, file, "count");
    let instances: Vec<_> = program
        .instances
        .iter()
        .filter(|(_, key)| key.definition == count)
        .map(|(_, key)| key.arguments.clone())
        .collect();
    assert_eq!(instances.len(), 2, "{instances:?}");
    assert!(instances.contains(&vec![TypeShape::Const(42)]));
    assert!(instances.contains(&vec![TypeShape::Const(43)]));
    let unit = runtime::runtime_raw_unit(
        &db,
        runtime::Instance::new(
            &db,
            file,
            arandu_middle::types::FunctionInstance {
                definition: symbol(&db, file, "main"),
                arguments: Vec::new(),
            },
        ),
    );
    let add = symbol(&db, file, "add");
    assert!(!unit.result.as_ref().expect("runtime caller").function.stmts.payloads.iter().any(|stmt| matches!(stmt,
        arandu_middle::amir::AmirStmt::Call { callee: arandu_middle::amir::AmirOperand::FunctionRef(callee), .. } if *callee == add
    )), "the argument helper must not run in the residual caller");
}

#[test]
fn instance_independent_arguments_in_templates_are_frozen_once() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = format!(
        "{COUNT}func add(a: int, b: int): int {{ return a + b }}\nfunc wrapper<T>(x: T): uint {{ return count<comptime (add(20, 22))>() }}\nfunc main(): uint {{ return wrapper<int>(0) + wrapper<bool>(false) }}"
    );
    let file = db.new_file("template.aru".into(), source.clone());
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    let wrapper = symbol(&db, file, "wrapper");
    assert_eq!(
        item_const_arguments(&db, file, wrapper)
            .values
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [Some(42)]
    );
    let program = runtime::runtime_program(&db, file);
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
    assert_eq!(
        program
            .instances
            .iter()
            .filter(|(_, key)| key.definition == symbol(&db, file, "count"))
            .count(),
        1
    );
    log.clear();
    let _ = runtime::runtime_program(&db, file);
    assert_eq!(log.count_executions_matching("ctfe_eval_root"), 0);
    file.set_text(&mut db)
        .to(source.replace("20, 22", "20, 23").into());
    assert_eq!(
        item_const_arguments(&db, file, wrapper)
            .values
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [Some(43)]
    );
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("template.aru".into(), source.replace("20, 22", "20, 23"));
    assert_eq!(errors(&db, file), errors(&clean, clean_file));
    assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
}

#[test]
fn dependent_arguments_in_templates_are_frozen_in_the_concrete_instance() {
    let mut db = DatabaseImpl::new();
    let source = format!(
        "{COUNT}func wrapper<comptime N: uint>(): uint {{ return count<comptime (N + 1)>() }}\nfunc main(): uint {{ return wrapper<41>() }}"
    );
    let file = db.new_file("dependent.aru".into(), source);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
}

#[test]
fn forwarded_constant_parameters_must_fit_the_complete_destination_domain() {
    for (source_type, destination_type, width, valid) in [
        ("u8", "u16", 8, true),
        ("u16", "u8", 8, false),
        ("i8", "u8", 8, true),
        ("u8", "i8", 8, false),
        ("uint", "usize", 4, true),
        ("usize", "uint", 4, true),
        ("usize", "uint", 8, false),
        ("uint", "usize", 8, true),
        ("u64", "u64", 8, true),
    ] {
        let mut db = DatabaseImpl::new();
        db.set_target_config(arandu_middle::DataLayout::ptr_width(width));
        let source = format!(
            "func leaf<comptime N: {destination_type}>(): uint {{ return 42 }}\nfunc wrapper<comptime M: {source_type}>(): uint {{ return leaf<M>() }}\nfunc main(): uint {{ return wrapper<1>() }}"
        );
        let file = db.new_file("forward.aru".into(), source);
        let diagnostics = errors(&db, file);
        assert_eq!(
            diagnostics.is_empty(),
            valid,
            "{source_type}->{destination_type}, ptr{width}: {diagnostics:?}"
        );
        if valid {
            assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
        } else {
            let diagnostic = diagnostics
                .iter()
                .find(|d| d.code == DiagCode::T011GenericConstraintNotSatisfied)
                .expect("domain error");
            assert_eq!(diagnostic.labels.len(), 2);
            assert!(!runtime::runtime_program(&db, file).diagnostics.is_empty());
        }
    }
}

#[test]
fn conditional_literal_roots_finish_with_concrete_integer_types() {
    for expression in [
        "if 1 < 2 { 42 } else { 0 }",
        "(if 2 > 1 { (42) } else { (0) })",
        "(if true { 20 } else { 0 }) + 22",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file(
            "conditional.aru".into(),
            format!("{COUNT}func main(): uint {{ return count<comptime ({expression})>() }}"),
        );
        assert_eq!(
            values(&db, file),
            [Some(42)],
            "{expression}: {:?}",
            errors(&db, file)
        );
        assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    }
}

#[test]
fn computed_arguments_in_local_type_annotations_are_frozen_before_typing() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("annotation.aru".into(), "struct Box<comptime N: uint> { value: int }\nfunc main(): int { let x: Box<comptime (20 + 22)> = Box<42> { value: 42 }; return x.value }".into());
    assert_eq!(values(&db, file), [Some(42)], "{:?}", errors(&db, file));
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
}

#[test]
fn parameter_ranges_are_checked_for_literals_and_target_sized_arguments() {
    for (parameter, bad) in [
        ("u8", "256"),
        ("i8", "128"),
        ("uint", "4294967296"),
        ("int", "2147483648"),
        ("usize", "4294967296"),
        ("isize", "2147483648"),
    ] {
        let mut db = DatabaseImpl::new();
        db.set_target_config(arandu_middle::DataLayout::ptr_width(4));
        let file = db.new_file("bounds.aru".into(), format!("func count<comptime N: {parameter}>(): {parameter} {{ return N }}\nfunc main(): {parameter} {{ return count<{bad}>() }}"));
        assert!(
            errors(&db, file)
                .iter()
                .any(|d| d.code == DiagCode::T011GenericConstraintNotSatisfied),
            "{parameter}: {:?}",
            errors(&db, file)
        );
    }
    let mut db = DatabaseImpl::new();
    let file = db.new_file("target.aru".into(), "func wide(): u64 { return 4294967296 }\nfunc count<comptime N: usize>(): usize { return N }\nfunc main(): usize { return count<comptime (wide())>() }".into());
    for width in [8, 4, 8] {
        db.set_target_config(arandu_middle::DataLayout::ptr_width(width));
        assert_eq!(values(&db, file), [Some(4294967296)]);
        assert_eq!(errors(&db, file).is_empty(), width == 8);
    }
}

#[test]
fn evaluation_limits_and_self_dependencies_fail_closed() {
    for (helper, call, code) in [
        (
            "func spin(): int { while true {} return 42 }",
            "spin()",
            DiagCode::T045ComptimeLimitExceeded,
        ),
        (
            "func recur(): int { return recur() }",
            "recur()",
            DiagCode::T045ComptimeLimitExceeded,
        ),
        ("", "main()", DiagCode::T044ComptimeEvaluationFailed),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file(
            "limits.aru".into(),
            format!("{COUNT}{helper}\nfunc main(): uint {{ return count<comptime ({call})>() }}"),
        );
        let diagnostics = errors(&db, file);
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "{call}: {diagnostics:?}"
        );
        assert!(diagnostics
            .iter()
            .all(|d| !d.code.as_str().starts_with("ICE")));
    }
}

#[test]
fn helper_and_sibling_edits_cut_off_unchanged_value_consumers() {
    #[salsa::tracked]
    fn answer_consumer(
        db: &dyn arandu_query::ArandCompilerDb,
        file: SourceFile,
        owner: SymbolId,
    ) -> Option<u64> {
        item_const_arguments(db, file, owner)
            .values
            .values()
            .copied()
            .flatten()
            .next()
    }
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let helper = db.new_file(
        "helper.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let source = format!(
        "from helper import {{ answer }}\n{COUNT}func main(): uint {{ return count<comptime (answer())>() }}\nfunc sibling(): int {{ return 12 }}"
    );
    let file = db.new_file("arguments.aru".into(), source.clone());
    let owner = symbol(&db, file, "main");
    assert_eq!(*answer_consumer(&db, file, owner), Some(42));
    log.clear();
    file.set_text(&mut db)
        .to(source.replace("return 12", "return 13").into());
    assert_eq!(*answer_consumer(&db, file, owner), Some(42));
    // Current header continuation can revalidate lowering after a sibling
    // edit. Equal AMIR cuts off the VM, and equal values cut off consumers.
    assert_eq!(
        log.count_executions_matching("ctfe_eval_root"),
        0,
        "{:?}",
        log.snapshot()
    );
    assert_eq!(
        log.count_executions_matching("answer_consumer"),
        0,
        "{:?}",
        log.snapshot()
    );
    log.clear();
    helper
        .set_text(&mut db)
        .to("public func answer(): int { return 40 + 2 }".into());
    assert_eq!(*answer_consumer(&db, file, owner), Some(42));
    assert_eq!(
        log.count_executions_matching("answer_consumer"),
        0,
        "{:?}",
        log.snapshot()
    );
    helper
        .set_text(&mut db)
        .to("public func answer(): int { return 43 }".into());
    assert_eq!(*answer_consumer(&db, file, owner), Some(43));
}

#[test]
fn staging_uses_headers_and_never_runtime_owner_queries() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file(
        "helper.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let file = db.new_file("arguments.aru".into(), format!("from helper import {{ answer }}\n{COUNT}func main(): uint {{ return count<comptime (answer())>() }}"));
    let owner = symbol(&db, file, "main");
    log.clear();
    assert_eq!(
        item_const_arguments(&db, file, owner)
            .values
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [Some(42)]
    );
    for query in [
        "item_typing",
        "runtime_unit",
        "instance_hir",
        "declaration_signatures",
        "resolve(",
    ] {
        assert_eq!(
            log.count_executions_matching(query),
            0,
            "{query}: {:?}",
            log.snapshot()
        );
    }
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
}

#[test]
fn wide_values_are_preserved_and_declared_parameter_bounds_are_checked() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("arguments.aru".into(), "func wide(): u64 { return 18446744073709551615 }\nfunc count<comptime N: u64>(): u64 { return N }\nfunc main(): u64 { return count<comptime (wide())>() }".into());
    assert_eq!(values(&db, file), [Some(u64::MAX)]);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
    file.set_text(&mut db).to("func wide(): u64 { return 256 }\nfunc count<comptime N: u8>(): u8 { return N }\nfunc main(): u8 { return count<comptime (wide())>() }".into());
    assert!(!errors(&db, file).is_empty());
}

#[test]
fn invalid_values_arithmetic_and_runtime_captures_are_diagnosed() {
    for (expression, prefix, code) in [
        ("-1", "", DiagCode::T011GenericConstraintNotSatisfied),
        ("true", "", DiagCode::T011GenericConstraintNotSatisfied),
        (
            "unit()",
            "func unit(): void {}\n",
            DiagCode::T003IncompatibleCallArg,
        ),
        ("1 / 0", "", DiagCode::T040DivisionByZero),
        ("2147483647 + 1", "", DiagCode::T046ComptimeArithmetic),
        ("n", "const n = 42\n", DiagCode::T043ComptimeRuntimeCapture),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("arguments.aru".into(), format!("{prefix}{COUNT}func main(n: int): uint {{ return count<comptime ({expression})>() }}"));
        let diagnostics = errors(&db, file);
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "{expression}: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.code.as_str().starts_with("ICE")),
            "{diagnostics:?}"
        );
    }
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "shadow.aru".into(),
        format!(
            "const n = 42\n{COUNT}func main(): uint {{ let n = 1; return count<comptime (n)>() }}"
        ),
    );
    assert!(errors(&db, file)
        .iter()
        .any(|d| d.code == DiagCode::T043ComptimeRuntimeCapture));
}

#[test]
fn discarded_branches_have_no_argument_obligations() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("arguments.aru".into(), format!("{COUNT}func main(): uint {{ comptime if true {{ return count<comptime (42)>() }} else {{ return count<comptime (missing / 0)>() }} }}"));
    assert_eq!(values(&db, file), [Some(42)]);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
}

#[test]
fn computed_arguments_in_match_arms_keep_their_lexical_scope() {
    {
        let body = "func main(): uint { let value = Option.Some(0 as uint)\nreturn match value { Option.Some(x) => count<comptime (20 + 22)>() + x Option.None => 0 } }";
        let mut db = DatabaseImpl::new();
        let file = db.new_file("arguments.aru".into(), format!("{COUNT}{body}"));
        assert_eq!(values(&db, file), [Some(42)]);
        assert!(
            errors(&db, file).is_empty(),
            "{body}: {:?}",
            errors(&db, file)
        );
    }
    for body in [
        "func main(): uint { let f = |value uint| { return count<comptime (value + 1)>() }\nreturn f(0) }",
        "func main(): uint { let value = Option.Some(0 as uint)\nreturn match value { Option.Some(value) => count<comptime (value + 1)>() Option.None => 0 } }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file(
            "arguments.aru".into(),
            format!("{COUNT}const value uint = 41\n{body}"),
        );
        assert!(
            errors(&db, file)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::T043ComptimeRuntimeCapture),
            "{body}: {:?}",
            errors(&db, file)
        );
    }
}

#[test]
fn lambda_arguments_are_frozen_without_claiming_closure_execution_support() {
    let body = "func main(): uint { let f = |x uint| { return count<comptime (20 + 22)>() + x }\nreturn f(0) }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("lambda-arguments.aru".into(), format!("{COUNT}{body}"));
    assert_eq!(values(&db, file), [Some(42)]);
    let staged = item_const_arguments(&db, file, symbol(&db, file, "main"));
    assert!(staged.diagnostics.is_empty(), "{:?}", staged.diagnostics);
    let diagnostics = errors(&db, file);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, DiagCode::U001FeatureNotSupported);
    assert!(diagnostics[0].message.contains("lambda/closure"));
    assert!(!runtime::runtime_program(&db, file).diagnostics.is_empty());
}

#[test]
fn computed_arguments_in_loop_and_pattern_headers_preserve_lexical_scope() {
    for owner in [
        "func main(): uint { for i in 0..count<comptime (42)>() { return i } return 0 }",
        "func main(): uint { let mut sum: uint = 0; comptime for i in 0..count<comptime (2)>() { sum += i }; return sum }",
        "func main(): uint { let mut i = 0; for i = count<comptime (0)>() as int; i < count<comptime (2)>() as int; i = i + count<comptime (1)>() as int {} return i as uint }",
        "func main(): uint { if Option.Some(count<comptime (42)>()) is Option.Some(value) { return value } return 0 }",
        "func main(): uint { while Option.Some(count<comptime (42)>()) is Option.Some(value) { return value } return 0 }",
        "func main(): uint { let x = Option.Some(42); if x is Option.Some(value) && count<comptime (1)>() == 1 { return value } return 0 }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("header-arguments.aru".into(), format!("{COUNT}{owner}"));
        assert!(passes::parse(&db, file).is_ok(), "{owner}: {:?}", passes::parse(&db, file).as_ref().err());
        assert!(errors(&db, file).is_empty(), "{owner}: {:?}", errors(&db, file));
        let output = runtime::runtime_program(&db, file);
        assert!(output.diagnostics.is_empty(), "{owner}: {:?}", output.diagnostics);
    }
    for owner in [
        "func main(): uint { for value in 0..count<comptime (value)>() { return value } return 0 }",
        "func main(): uint { for let value = 1; count<comptime (value)>() > 0; value = 0 {} return 0 }",
        "func main(): uint { if Option.Some(1) is Option.Some(value) && count<comptime (value)>() > 0 { return 1 } return 0 }",
        "func main(): uint { while Option.Some(1) is Option.Some(value) && count<comptime (value)>() > 0 { return 1 } return 0 }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("capture-header-arguments.aru".into(), format!("{COUNT}const value uint = 42\n{owner}"));
        assert!(passes::parse(&db, file).is_ok(), "{owner}: {:?}", passes::parse(&db, file).as_ref().err());
        let diagnostics = errors(&db, file);
        // The incoming range expression sees the global; the new range binding
        // enters scope only in the body. C-style and pattern later clauses see
        // their runtime bindings and must reject them as captures.
        if owner.contains("for value in") {
            assert!(diagnostics.is_empty(), "{owner}: {diagnostics:?}");
        } else {
            assert!(diagnostics.iter().any(|d| d.code == DiagCode::T043ComptimeRuntimeCapture), "{owner}: {diagnostics:?}");
        }
    }
}

#[test]
fn root_count_limit_reports_one_error_instead_of_unstaged_cascades() {
    let mut db = DatabaseImpl::new();
    let calls = "count<comptime (0)>();\n".repeat(4097);
    let file = db.new_file(
        "many.aru".into(),
        format!("{COUNT}func main(): uint {{ {calls}return 0 }}"),
    );
    let diagnostics = errors(&db, file);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, DiagCode::T045ComptimeLimitExceeded);
}

#[test]
fn incremental_argument_and_helper_edits_match_clean() {
    let mut db = DatabaseImpl::new();
    let source = format!(
        "{COUNT}func answer(): int {{ return 42 }}\nfunc main(): uint {{ return count<comptime (answer())>() }}"
    );
    let file = db.new_file("arguments.aru".into(), source.clone());
    assert_eq!(values(&db, file), [Some(42)]);
    for edited in [
        source.replace("return 42", "return 43"),
        source.replace("answer())", "20 + 22)"),
        source.replace("answer())", "1 / 0)"),
    ] {
        file.set_text(&mut db).to(edited.clone().into());
        let mut clean = DatabaseImpl::new();
        let clean_file = clean.new_file("arguments.aru".into(), edited);
        assert_eq!(values(&db, file), values(&clean, clean_file));
        assert_eq!(errors(&db, file), errors(&clean, clean_file));
    }
}
