#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{DiagCode, Severity};
use arandu_query::{passes, DatabaseImpl};
use salsa::Setter;

fn errors(db: &DatabaseImpl, file: arandu_query::SourceFile) -> Vec<arandu_middle::Diagnostic> {
    passes::type_check(db, file)
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect()
}

#[test]
fn a_static_sibling_does_not_defer_ordinary_generic_bodies() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("siblings.aru".into(), "func identity<T>(value: T): T { return value }\nfunc static_value(): int { comptime if true { return 42 } else { return absent } }\nfunc main(): int { return identity<int>(static_value()) }".into());
    let output = arandu_query::runtime::runtime_program(&db, file);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(output.artifacts.amir.funcs.len() >= 3);
}

#[test]
fn only_the_selected_branch_is_resolved_typed_and_lowered() {
    for (condition, then, otherwise) in [
        ("true", "return 42", "return unavailable()"),
        ("false", "return unavailable()", "return 42"),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file(
            "static.aru".into(),
            format!(
                "func main(): int {{ comptime if {condition} {{ {then} }} else {{ {otherwise} }} }}"
            ),
        );
        assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
        assert!(passes::lower_amir(&db, file)
            .type_check
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error));
        let resolved = passes::resolve(&db, file);
        assert_eq!(resolved.resolved.comptime_branches.len(), 1);
    }
}

#[test]
fn discarded_nested_conditions_are_not_evaluated() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("static.aru".into(), "func main(): int { comptime if true { comptime if false { return nope } else { return 42 } } else { comptime if missing { return nope } } }".into());
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    assert!(passes::lower_amir(&db, file)
        .type_check
        .diagnostics
        .iter()
        .all(|d| d.severity != Severity::Error));
    assert_eq!(
        passes::resolve(&db, file).resolved.comptime_branches.len(),
        2
    );
}

#[test]
fn helpers_may_contain_static_branches_without_visiting_discarded_code() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("static.aru".into(), "func choose(): bool { comptime if true { return true } else { return missing } }\nfunc main(): int { comptime if choose() { return 42 } else { return absent() } }".into());
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    assert!(passes::lower_amir(&db, file)
        .type_check
        .diagnostics
        .iter()
        .all(|d| d.severity != Severity::Error));
}

#[test]
fn failed_conditions_and_cycles_never_select_a_branch() {
    for condition in ["42", "missing", "choose()"] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("static.aru".into(), format!("func choose(): bool {{ comptime if choose() {{ return true }} else {{ return false }} }}\nfunc main(): int {{ comptime if {condition} {{ return unknown_a }} else {{ return unknown_b }} }}"));
        let diagnostics = errors(&db, file);
        assert!(!diagnostics.is_empty());
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.code.as_str().starts_with("ICE")),
            "{diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.message.contains("unknown_a") && !d.message.contains("unknown_b")),
            "{diagnostics:?}"
        );
    }
}

#[test]
fn lexical_runtime_shadowing_is_a_capture_not_a_global_constant() {
    for source in [
        "const flag = true\nfunc main(input: bool): int { let flag = input\ncomptime if flag { return 1 } else { return 2 } }",
        "const flag = true\nfunc main(): int { while false { let flag = false\ncomptime if flag { return 1 } } return 2 }",
        "const flag = true\nfunc main(): int { let input = Option.Some(false)\nif input is Option.Some(flag) && flag == false { comptime if flag { return 1 } } return 2 }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("static.aru".into(), source.into());
        assert!(
            errors(&db, file)
                .iter()
                .any(|d| d.code == DiagCode::T043ComptimeRuntimeCapture),
            "{:?}",
            errors(&db, file)
        );
    }
}

#[test]
fn discarded_ctfe_roots_and_bad_types_are_not_obligations() {
    for source in [
        "func main(): int { comptime if false { let x = comptime \"unsupported\"; return true } return 42 }",
        "func main(): int { comptime if false { return missing } else comptime if true && !false { return comptime 42 } else { return false } }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("static.aru".into(), source.into());
        assert!(
            errors(&db, file).is_empty(),
            "{source}: {:?}",
            errors(&db, file)
        );
        assert!(
            passes::lower_amir(&db, file)
                .type_check
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error)
        );
    }
}

#[test]
fn static_selection_in_match_arms_preserves_runtime_bindings() {
    {
        let source = "func main(): int { let value = Option.Some(42)\nmatch value { Option.Some(x) => { comptime if true { return x } else { return unavailable() } } Option.None => { return 0 } } }";
        let mut db = DatabaseImpl::new();
        let file = db.new_file("contexts.aru".into(), source.into());
        assert!(
            passes::parse(&db, file).is_ok(),
            "{source}: {:?}",
            **passes::parse(&db, file)
        );
        assert!(
            errors(&db, file).is_empty(),
            "{source}: {:?}",
            errors(&db, file)
        );
        assert!(
            passes::lower_amir(&db, file)
                .type_check
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error),
            "{source}: {:?}",
            passes::lower_amir(&db, file).type_check.diagnostics
        );
    }
}

#[test]
fn lambda_selection_is_lexical_but_closure_execution_remains_explicitly_unsupported() {
    for (source, selected) in [
        ("func main(): int { let f = |x int| { comptime if true { return x } else { return unavailable() } }\nreturn f(42) }", true),
        ("func main(): int { let f = |x int| { let g = |y int| { comptime if false { return unavailable() } else { return y } }\nreturn g(x) }\nreturn f(42) }", false),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("lambda-selection.aru".into(), source.into());
        let headers = passes::resolved_headers(&db, file);
        let owner = headers.declarations.symbols.iter().find(|symbol| symbol.name == "main").expect("main").id;
        let branches = arandu_query::ctfe::item_static_branches(&db, file, owner);
        assert!(branches.diagnostics.is_empty(), "{:?}", branches.diagnostics);
        assert_eq!(branches.decisions.values().copied().collect::<Vec<_>>(), [selected]);
        let diagnostics = errors(&db, file);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, DiagCode::U001FeatureNotSupported);
        assert!(diagnostics[0].message.contains("lambda/closure"));
        assert!(passes::lower_amir(&db, file).amir.funcs.is_empty());
    }
}

#[test]
fn nested_context_parameters_cannot_shadow_globals_into_compile_time_values() {
    for source in [
        "const flag = true\nfunc main(): int { let value = Option.Some(false)\nreturn match value { Option.Some(flag) => { comptime if flag { return 1 } else { return 2 } } Option.None => 0 } }",
        "const flag = true\nfunc main(): int { let f = |flag bool| { comptime if flag { return 1 } else { return 2 } }\nreturn f(false) }",
        "const flag = true\nfunc main(): int { let f = |x int| { let flag = false\ncomptime if flag { return x } else { return 2 } }\nreturn f(42) }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("contexts.aru".into(), source.into());
        let diagnostics = errors(&db, file);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::T043ComptimeRuntimeCapture),
            "{source}: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")),
            "{diagnostics:?}"
        );
    }
}

#[test]
fn runtime_if_keeps_both_branches_and_static_scope_does_not_escape() {
    for source in [
        "func main(): int { if true { return 42 } else { return missing } }",
        "func main(): int { comptime if true { let scoped = 42 } return scoped }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("static.aru".into(), source.into());
        assert!(errors(&db, file)
            .iter()
            .any(|d| d.code == DiagCode::N001UndefinedValue));
    }
}

#[test]
fn unsupported_contexts_and_non_bool_conditions_fail_closed() {
    {
        let source =
            "func main(): int { comptime if 42 { return absent } else { return missing } }";
        let mut db = DatabaseImpl::new();
        let file = db.new_file("static.aru".into(), source.into());
        let diagnostics = errors(&db, file);
        assert!(!diagnostics.is_empty(), "{source}");
        assert!(
            diagnostics
                .iter()
                .all(|d| !d.code.as_str().starts_with("ICE")
                    && !d.message.contains("missing")
                    && !d.message.contains("absent")),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn instance_independent_template_conditions_do_not_require_an_instance() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("contexts.aru".into(), "func selected<T>(x: T): T { comptime if true { return x } else { return unavailable() } }\nfunc main(): int { return selected<int>(42) }".into());
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    assert!(
        passes::lower_amir(&db, file)
            .type_check
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "{:?}",
        passes::lower_amir(&db, file).type_check.diagnostics
    );
}

#[test]
fn imported_helpers_use_header_staging_and_body_edits_cut_off_selection_consumers() {
    use arandu_query::{ArandCompilerDb, SourceFile};
    #[salsa::tracked]
    fn selected(db: &dyn ArandCompilerDb, file: SourceFile) -> usize {
        let owner = passes::local_symbols(db, file)
            .symbols
            .iter()
            .find(|s| s.name == "main")
            .expect("main")
            .id;
        arandu_query::ctfe::item_static_branches(db, file, owner)
            .decisions
            .len()
    }
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let helper = db.new_file(
        "helper.aru".into(),
        "public func choose(): bool { comptime if true { return true } else { return missing } }"
            .into(),
    );
    let source = "from helper import { choose }\nfunc main(): int { comptime if choose() { return 42 } else { return missing } }";
    let file = db.new_file("static.aru".into(), source.into());
    assert_eq!(*selected(&db, file), 1);
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
    log.clear();
    file.set_text(&mut db).to(source.replace("42", "43").into());
    assert_eq!(*selected(&db, file), 1);
    assert_eq!(
        log.count_executions_matching("selected("),
        0,
        "{:?}",
        log.snapshot()
    );
    assert_eq!(
        log.count_executions_matching("ctfe_header_func_amir"),
        0,
        "{:?}",
        log.snapshot()
    );
    helper
        .set_text(&mut db)
        .to("public func choose(): bool { return false }".into());
    assert!(
        errors(&db, file)
            .iter()
            .any(|d| d.code == DiagCode::N001UndefinedValue),
        "{:?}",
        errors(&db, file)
    );
}

#[test]
fn branch_changes_match_clean_and_remove_stale_diagnostics() {
    let mut db = DatabaseImpl::new();
    let source = "func main(): int { comptime if FLAG { return 42 } else { return missing } }";
    let file = db.new_file("static.aru".into(), source.replace("FLAG", "true"));
    assert!(errors(&db, file).is_empty());
    for flag in ["false", "true", "missing", "true"] {
        let text = source.replace("FLAG", flag);
        file.set_text(&mut db).to(text.clone().into());
        let mut clean = DatabaseImpl::new();
        let clean_file = clean.new_file("static.aru".into(), text);
        assert_eq!(errors(&db, file), errors(&clean, clean_file));
    }
}

#[test]
fn target_changes_update_decisions_without_changing_exported_headers() {
    use arandu_middle::DataLayout;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("target.aru".into(), "public func main(): int { comptime if ((~(0 as usize)) >> 31) > (1 as usize) { return 42 } else { return missing } }".into());
    let exports = std::sync::Arc::clone(passes::exported_symbols(&db, file));
    for width in [8, 4, 8] {
        db.set_target_config(DataLayout::ptr_width(width));
        let diagnostics = errors(&db, file);
        assert_eq!(
            diagnostics
                .iter()
                .any(|d| d.code == DiagCode::N001UndefinedValue),
            width == 4,
            "{diagnostics:?}"
        );
        assert_eq!(passes::exported_symbols(&db, file), &exports);
    }
}

#[test]
fn exhausted_condition_helper_frames_do_not_select_or_leak_body_errors() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("limit.aru".into(), "func choose(): bool { return choose() }\nfunc main(): int { comptime if choose() { return unknown_a } else { return unknown_b } }".into());
    let diagnostics = errors(&db, file);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T045ComptimeLimitExceeded),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.code.as_str().starts_with("ICE") && !d.message.contains("unknown_")),
        "{diagnostics:?}"
    );
    file.set_text(&mut db).to("func choose(): bool { return true }\nfunc main(): int { comptime if choose() { return 42 } else { return unknown_b } }".into());
    assert!(errors(&db, file).is_empty(), "{:?}", errors(&db, file));
}

#[test]
fn discarded_branches_still_require_valid_syntax() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static.aru".into(),
        "func main(): int { comptime if true { return 42 } else { let = } }".into(),
    );
    assert!(passes::parse(&db, file).is_err());
}
