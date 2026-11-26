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

#[test]
fn no_copy_rejects_invalid_target_and_arguments() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();

    let bad_target = db.new_file(
        "nocopy-bad-target.aru".into(),
        "@NoCopy\nfunc nope() {}\n".into(),
    );
    let checked = type_check(&db, bad_target);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == arandu_middle::DiagCode::N013InvalidAnnotationTarget),
        "@NoCopy on function must emit N013, got: {:?}",
        checked.diagnostics
    );

    let bad_args = db.new_file(
        "nocopy-bad-args.aru".into(),
        "@NoCopy(\"reason\")\nstruct Token { id: int }\n".into(),
    );
    let checked = type_check(&db, bad_args);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == arandu_middle::DiagCode::N014InvalidAnnotationArguments),
        "@NoCopy with arguments must emit N014, got: {:?}",
        checked.diagnostics
    );
}

#[test]
fn no_copy_struct_and_enum_are_move_only_and_compositional() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "nocopy-semantics.aru".into(),
        r#"
@NoCopy
struct Token { id: int }

@NoCopy
struct Unlocked {}

@NoCopy
enum State {
    Ready,
    Done(int),
}

struct Wrapper {
    tok: Token,
}

struct Plain {
    id: int,
}

func use_token(t: Token): int {
    return t.id
}

func use_wrapper(w: Wrapper): int {
    return w.tok.id
}

func main(): int {
    let t = Token { id: 1 }
    let a = use_token(t)
    let b = use_token(t)
    return a + b
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    assert!(
        checked
            .diagnostics
            .iter()
            .all(|d| d.severity != arandu_diagnostics::Severity::Error),
        "unexpected typeck errors: {:?}",
        checked.diagnostics
    );

    let find_type = |name: &str| {
        let sym = checked
            .symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing symbol {name}"));
        checked
            .type_info
            .type_interner
            .intern(arandu_middle::types::ArType::named(
                sym.id,
                &[],
                &checked.type_info.type_interner,
            ))
    };

    assert!(!checked.type_info.is_copy(find_type("Token")));
    assert!(!checked.type_info.is_copy(find_type("Unlocked")));
    assert!(!checked.type_info.is_copy(find_type("State")));
    assert!(!checked.type_info.is_copy(find_type("Wrapper")));
    assert!(checked.type_info.is_copy(find_type("Plain")));

    // Move checker must reject use-after-move of scalar-only `@NoCopy` struct.
    let diags = file_ide_diagnostics(&db, file);
    assert!(
        diags.iter().any(|d| d.code == "O001"),
        "expected O001 use-after-move for @NoCopy struct, got: {:?}",
        diags.as_slice()
    );
}

#[test]
fn no_copy_repeat_array_is_rejected() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "nocopy-repeat.aru".into(),
        r#"
@NoCopy
struct Token { id: int }

func main(): int {
    let arr = [Token { id: 1 }; 4]
    return arr[0].id
}
"#
        .into(),
    );

    let diags = file_ide_diagnostics(&db, file);
    assert!(
        diags.iter().any(|d| d.code == "T048"),
        "expected T048 error repeating @NoCopy value in [val; 4], got: {:?}",
        diags.as_slice()
    );
}

#[test]
fn no_copy_invalidates_module_signature_hash() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "nocopy-cutoff.aru".into(),
        "public struct Config { timeout: int }\n".into(),
    );
    let sig_before = module_signatures(&db, file).clone();
    let sym = sig_before
        .symbols
        .iter()
        .find(|s| s.name == "Config")
        .expect("Config symbol")
        .id;
    assert!(!sig_before.type_info.non_copy_types.contains(&sym));

    file.set_text(&mut db).to(Arc::from(
        "@NoCopy\npublic struct Config { timeout: int }\n",
    ));
    let sig_after = module_signatures(&db, file).clone();
    let sym_after = sig_after
        .symbols
        .iter()
        .find(|s| s.name == "Config")
        .expect("Config symbol after")
        .id;
    assert!(sig_after.type_info.non_copy_types.contains(&sym_after));
    assert!(
        sig_before != sig_after,
        "adding @NoCopy must invalidate module_signatures"
    );
}
