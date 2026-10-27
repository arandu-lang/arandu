#![allow(clippy::unwrap_used, clippy::expect_used)]
//! RFC 0042 phase 0: guards for a body edit that moves the later items of its
//! file. Positions must stay exact wherever they reach the user (diagnostics,
//! goto, debug info), the incremental result must equal a cold build, and the
//! number of re-lowered siblings is pinned so the migration can lower it.

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
    // `alpha` plus the four items it moves. RFC 0042 targets 1: only the
    // edited item should be lowered again.
    assert_eq!(
        log.count_executions_matching("instance_hir"),
        5,
        "{}",
        log.format_chain(false)
    );
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
