#![allow(clippy::expect_used, clippy::unwrap_used)]

use arandu_query::dataflow::{item_attribute_validation, ITEM_ATTRIBUTE_VALIDATION_EXEC_COUNT};
use arandu_query::passes::{module_signatures, parse, type_check};
use arandu_query::{file_highlights, file_ide_diagnostics, DatabaseImpl, HlKind, SourceFile};
use salsa::Setter;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

static COUNTER_LOCK: Mutex<()> = Mutex::new(());

fn items(db: &DatabaseImpl, file: SourceFile) -> Vec<arandu_middle::SymbolId> {
    let program = parse(db, file);
    let parsed = program.as_ref();
    let program = parsed.as_ref().expect("parse");
    let signatures = module_signatures(db, file);
    arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref())
}

#[test]
fn removed_alias_reaches_cli_and_ide_diagnostics_with_exact_replacement() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "legacy-annotation.aru".into(),
        "@no_fallback\nfunc critical() {}\n".into(),
    );

    let checked = type_check(&db, file);
    let diagnostic = checked
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == arandu_middle::DiagCode::N012UnknownAnnotation)
        .expect("type_check migration error");
    let replacement = diagnostic.hints[0]
        .replacement
        .as_ref()
        .expect("replacement");
    assert_eq!((replacement.span.start, replacement.span.end), (1, 12));
    assert_eq!(replacement.new_text, "NoFallback");

    let ide = file_ide_diagnostics(&db, file);
    let diagnostic = ide
        .iter()
        .find(|diagnostic| diagnostic.code == "N012")
        .expect("IDE migration error");
    let replacement = diagnostic.hints[0]
        .replacement
        .as_ref()
        .expect("IDE replacement");
    assert_eq!((replacement.start, replacement.end), (1, 12));
    assert_eq!(replacement.new_text, "NoFallback");
}

#[test]
fn annotation_name_has_decorator_semantic_token() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "highlight-annotation.aru".into(),
        "@NoFallback\nfunc critical() {}\n".into(),
    );
    let highlights = file_highlights(&db, file);
    assert!(highlights
        .iter()
        .any(|token| { token.start == 1 && token.end == 11 && token.kind == HlKind::Decorator }));
}

#[test]
fn annotation_validation_early_cuts_off_unchanged_sibling() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "annotation-cutoff.aru".into(),
        "@NoFallback\nfunc alpha() {}\n\nfunc beta(): int { return 1 }\n".into(),
    );
    let initial = items(&db, file);
    assert_eq!(initial.len(), 2);
    let _ = item_attribute_validation(&db, file, initial[0]);
    let _ = item_attribute_validation(&db, file, initial[1]);
    ITEM_ATTRIBUTE_VALIDATION_EXEC_COUNT.store(0, Ordering::SeqCst);

    file.set_text(&mut db).to(Arc::from(
        "@NoFallback\nfunc alpha() {}\n\nfunc beta(): int { return 2 }\n",
    ));
    let changed = items(&db, file);
    let _ = item_attribute_validation(&db, file, changed[0]);
    let _ = item_attribute_validation(&db, file, changed[1]);

    assert!(
        ITEM_ATTRIBUTE_VALIDATION_EXEC_COUNT.load(Ordering::SeqCst) <= 1,
        "only the edited item's annotation validation may execute"
    );
}
