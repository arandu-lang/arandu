#![allow(clippy::unwrap_used, clippy::expect_used)]
//! RFC 0042 phase 0: guards for a body edit that moves the later items of its
//! file. Positions must stay exact wherever they reach the user (diagnostics,
//! goto, debug info), the incremental result must equal a cold build, and only
//! the edited item is lowered again.

use arandu_middle::db::DiagnosticsAccumulator;
use arandu_query::passes::{lower_amir, symbol_span, type_check};
use arandu_query::{file_ide_diagnostics, DatabaseImpl, SourceFile, StableHash};
use salsa::Setter;
use std::sync::Arc;

/// `alpha` comes first, so growing its body moves every later item.
fn program(growth: usize) -> String {
    let mut alpha = String::from("func alpha(value: int): int {\n    let mut acc = value\n");
    for step in 1..=growth {
        alpha.push_str(&format!("    acc = acc + {step}\n"));
    }
    alpha.push_str("    return acc\n}\n");
    format!(
        "{alpha}\nfunc beta(value: int): int {{\n    return alpha(value) + 1\n}}\n\nfunc gamma(value: int): int {{\n    return beta(value) * 2\n}}\n\nfunc delta(): int {{\n    return 7\n}}\n\nfunc main(): int {{\n    return alpha(1) + beta(2) + gamma(3) + delta()\n}}\n"
    )
}

fn text_at(db: &DatabaseImpl, file: SourceFile, span: arandu_middle::Span) -> String {
    file.text(db)
        .get(span.start as usize..span.end as usize)
        .unwrap_or_default()
        .to_owned()
}

fn function_named(db: &DatabaseImpl, file: SourceFile, name: &str) -> arandu_middle::SymbolId {
    let artifacts = lower_amir(db, file);
    let symbols = &artifacts.type_check.symbols;
    artifacts
        .amir
        .funcs
        .iter()
        .map(|function| function.symbol)
        .find(|symbol| symbols.get(*symbol).name == name)
        .expect("function")
}

#[test]
fn growing_a_body_relowers_the_siblings_it_moves() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("guards.aru".into(), program(0));
    let _ = lower_amir(&db, file);
    log.clear();

    file.set_text(&mut db).to(Arc::from(program(3)));
    let _ = lower_amir(&db, file);
    // Only the edited item is lowered again; the four items it moves keep
    // their item-local HIR and units (RFC 0042 step C).
    for query in ["instance_hir", "runtime_raw_unit", "runtime_unit"] {
        assert_eq!(
            log.count_executions_matching(query),
            1,
            "{query}: {}",
            log.format_chain(false)
        );
    }
}

/// Editor diagnostics place the flow checks of a moved item; the checks and
/// the dataflow facts run again only for the edited item.
#[test]
fn growing_a_body_rechecks_only_its_own_flow() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("guards.aru".into(), program(0));
    let _ = file_ide_diagnostics(&db, file);
    log.clear();

    file.set_text(&mut db).to(Arc::from(program(3)));
    let _ = file_ide_diagnostics(&db, file);
    for query in [
        "item_flow_diagnostics",
        "liveness_facts",
        "func_borrow_summaries",
    ] {
        assert_eq!(
            log.count_executions_matching(query),
            1,
            "{query}: {}",
            log.format_chain(false)
        );
    }
}

#[test]
fn a_moved_sibling_lowers_to_the_bytes_of_a_cold_build() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("guards.aru".into(), program(0));
    let _ = lower_amir(&db, file);
    file.set_text(&mut db).to(Arc::from(program(3)));
    let incremental = lower_amir(&db, file).value.stable_hash();

    let mut cold = DatabaseImpl::new();
    let cold_file = cold.new_file("guards.aru".into(), program(3));
    assert_eq!(
        incremental,
        lower_amir(&cold, cold_file).value.stable_hash()
    );
}

#[test]
fn a_moved_sibling_keeps_exact_diagnostic_positions() {
    let broken = |growth: usize| program(growth).replace("    return 7\n", "    return true\n");
    let mut db = DatabaseImpl::new();
    let file = db.new_file("guards.aru".into(), broken(0));
    for growth in [0, 3] {
        if growth > 0 {
            file.set_text(&mut db).to(Arc::from(broken(growth)));
        }
        let _ = type_check(&db, file);
        let typed = type_check::accumulated::<DiagnosticsAccumulator>(&db, file);
        assert!(
            typed.iter().any(|diagnostic| {
                text_at(&db, file, diagnostic.0.span) == "return true"
                    && diagnostic
                        .0
                        .labels
                        .iter()
                        .any(|label| text_at(&db, file, label.span) == "int")
            }),
            "growth {growth}: {:?}",
            typed.iter().map(|d| &d.0).collect::<Vec<_>>()
        );
        let ide = file_ide_diagnostics(&db, file);
        assert!(
            ide.iter().any(|diagnostic| {
                text_at(
                    &db,
                    file,
                    arandu_middle::Span::new(diagnostic.file_id, diagnostic.start, diagnostic.end),
                ) == "return true"
            }),
            "growth {growth}: {:?}",
            ide.iter().collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_moved_sibling_keeps_its_declaration_and_goto_positions() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("guards.aru".into(), program(0));
    let _ = lower_amir(&db, file);
    file.set_text(&mut db).to(Arc::from(program(3)));

    for name in ["beta", "gamma", "delta"] {
        let symbol = function_named(&db, file, name);
        // Debug info reads the composed symbol span as the declaration line.
        let composed = lower_amir(&db, file).type_check.symbols.get(symbol).span;
        assert_eq!(text_at(&db, file, composed), name, "composed `{name}`");
        // Goto resolves the definition through `symbol_span`.
        assert_eq!(
            text_at(&db, file, symbol_span(&db, symbol)),
            name,
            "goto `{name}`"
        );
    }
}

/// Debug line rows and variable locations read the temp and local spans of
/// each function. A cold build places units the same way, so the check is
/// absolute: every position of a moved sibling lies in its own text.
#[test]
fn a_moved_sibling_keeps_its_debug_positions_in_its_own_text() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("guards.aru".into(), program(0));
    let _ = lower_amir(&db, file);
    file.set_text(&mut db).to(Arc::from(program(3)));

    let text = file.text(&db).to_string();
    let artifacts = lower_amir(&db, file);
    for name in ["beta", "gamma", "delta"] {
        let symbol = function_named(&db, file, name);
        let function = artifacts
            .amir
            .funcs
            .iter()
            .find(|function| function.symbol == symbol)
            .expect("function");
        let start = text.find(&format!("func {name}(")).expect("item") as u32;
        let end = start + text[start as usize..].find("\n}\n").expect("item end") as u32 + 2;
        let spans = function
            .temps
            .iter()
            .map(|temp| temp.span)
            .chain(function.locals.iter().map(|local| local.span));
        let mut checked = 0;
        for span in spans.filter(|span| *span != arandu_middle::Span::new(0, 0, 0)) {
            checked += 1;
            assert!(
                span.file_id == *file.file_id(&db) && start <= span.start && span.end <= end,
                "`{name}`: {span:?} is outside {start}..{end}"
            );
        }
        assert!(checked > 0, "`{name}` has positioned temps");
    }
}

/// An ownership diagnostic comes from the unit of a moved sibling, whose
/// positions are relative to its item until the composition and the IDE place
/// them back in the file.
#[test]
fn a_moved_sibling_keeps_exact_ownership_diagnostic_positions() {
    let conflicting = |growth: usize| {
        format!(
            "{}\nfunc borrowValue(value: ref int): ref int {{\n    return value\n}}\n\nfunc conflict(): int {{\n    let mut value = 42\n    let borrowed = borrowValue(value)\n    value = 43\n    return *borrowed\n}}\n",
            program(growth)
        )
    };
    let mut db = DatabaseImpl::new();
    let file = db.new_file("guards.aru".into(), conflicting(0));
    for growth in [0, 3] {
        if growth > 0 {
            file.set_text(&mut db).to(Arc::from(conflicting(growth)));
        }
        let ide = file_ide_diagnostics(&db, file);
        let diagnostic = ide
            .iter()
            .find(|diagnostic| diagnostic.code == "O003")
            .unwrap_or_else(|| panic!("growth {growth}: {:?}", ide.iter().collect::<Vec<_>>()));
        let at = |start: u32, end: u32| {
            text_at(
                &db,
                file,
                arandu_middle::Span::new(diagnostic.file_id, start, end),
            )
        };
        assert_eq!(
            at(diagnostic.start, diagnostic.end),
            "mut value",
            "growth {growth}"
        );
        assert_eq!(
            diagnostic
                .labels
                .iter()
                .map(|label| at(label.start, label.end))
                .collect::<Vec<_>>(),
            ["let borrowed = borrowValue(value)"],
            "growth {growth}"
        );
        let runtime = arandu_query::runtime::runtime_program(&db, file);
        let compiled = runtime
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code == arandu_middle::DiagCode::O003MutableBorrowConflict
            })
            .unwrap_or_else(|| panic!("growth {growth}: {:?}", runtime.diagnostics));
        assert_eq!(
            text_at(&db, file, compiled.span),
            "mut value",
            "growth {growth}"
        );
        assert_eq!(
            compiled
                .labels
                .iter()
                .map(|label| text_at(&db, file, label.span))
                .collect::<Vec<_>>(),
            ["let borrowed = borrowValue(value)"],
            "growth {growth}"
        );
    }
}

/// A caller lowered before its generic callee names the callee header without
/// positions. Debug info reads each instance's symbol span, so the composed
/// instance takes it from the unit that defines it.
#[test]
fn an_instance_is_positioned_by_its_own_unit_even_after_its_caller() {
    let source = "func main(): int {\n    return identity(5)\n}\n\nfunc identity<T>(x: T): T {\n    return x\n}\n";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("order.aru".into(), source.into());
    let artifacts = lower_amir(&db, file);
    let symbols = &artifacts.type_check.symbols;
    let instance = artifacts
        .amir
        .funcs
        .iter()
        .map(|function| symbols.get(function.symbol))
        .find(|symbol| symbol.name.contains("identity"))
        .expect("identity instance");
    assert!(
        text_at(&db, file, instance.span).starts_with("func identity<T>"),
        "{instance:?}"
    );
}
