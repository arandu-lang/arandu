#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{DiagCode, SymbolId, SymbolKind};
use arandu_query::{
    passes::{header_signatures, resolve, resolved_headers},
    ArandCompilerDb, DatabaseImpl, SourceFile, StableHash,
};
use salsa::Setter;

#[salsa::tracked]
fn header_consumer(db: &dyn ArandCompilerDb, file: SourceFile) -> usize {
    header_signatures(db, file).type_info.decl_types.len()
}

fn parameter(db: &DatabaseImpl, file: SourceFile, name: &str) -> SymbolId {
    resolved_headers(db, file)
        .declarations
        .symbols
        .iter()
        .find(|symbol| symbol.kind == SymbolKind::Param && symbol.name == name)
        .expect("header parameter")
        .id
}

#[test]
fn headers_do_not_resolve_bodies_and_completion_preserves_parameter_ids() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "headers.aru".into(),
        "func first(x: int): int { return missing }\nfunc second(y: int): int { return y }".into(),
    );
    let headers = resolved_headers(&db, file);
    assert!(headers.declarations.diagnostics.is_empty());
    assert!(headers
        .declarations
        .symbols
        .iter()
        .all(|s| s.kind != SymbolKind::Local));
    assert!(header_signatures(&db, file).diagnostics.is_empty());
    assert_eq!(log.count_executions_matching("resolve("), 0);
    let x = parameter(&db, file, "x");
    let y = parameter(&db, file, "y");
    let complete = resolve(&db, file);
    assert!(
        log.count_executions_matching("resolve(") > 0,
        "{:?}",
        log.snapshot()
    );
    assert!(complete
        .diagnostics
        .iter()
        .any(|d| d.code == DiagCode::N001UndefinedValue));
    assert_eq!(complete.symbols.get(x).name, "x");
    assert_eq!(complete.symbols.get(y).name, "y");
    file.set_text(&mut db).to("func first(x: int): int { let a = 1; let b = 2; return a + b }\nfunc second(y: int): int { return y }".into());
    assert_eq!(parameter(&db, file, "x"), x);
    assert_eq!(parameter(&db, file, "y"), y);
    let complete = resolve(&db, file);
    assert_eq!(complete.symbols.get(y).name, "y");
    assert!(complete
        .diagnostics
        .iter()
        .all(|d| d.code != DiagCode::N001UndefinedValue));
}

#[test]
fn same_length_body_edit_cuts_off_header_consumers_and_matches_clean() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let source = "func one(x: int): int { return x + 1 }\nfunc two(y: int): int { return y }";
    let file = db.new_file("cutoff.aru".into(), source.into());
    let first = *header_consumer(&db, file);
    let before = resolved_headers(&db, file).stable_hash();
    log.clear();
    let edited = source.replace("x + 1", "x + 2");
    file.set_text(&mut db).to(edited.clone().into());
    assert_eq!(*header_consumer(&db, file), first);
    assert_eq!(resolved_headers(&db, file).stable_hash(), before);
    assert_eq!(log.count_executions_matching("header_consumer"), 0);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("cutoff.aru".into(), edited);
    assert_eq!(
        header_signatures(&db, file).type_info.decl_types.len(),
        header_signatures(&clean, clean_file)
            .type_info
            .decl_types
            .len()
    );
    assert_eq!(
        resolve(&db, file).stable_hash(),
        resolve(&clean, clean_file).stable_hash()
    );
}

#[test]
fn imported_headers_ignore_invalid_helper_bodies_and_keep_usage_for_completion() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    db.new_file(
        "helper.aru".into(),
        "public func answer(): int { return missing }".into(),
    );
    let file = db.new_file(
        "main.aru".into(),
        "from helper import { answer }\nfunc main(): int { return answer() }".into(),
    );
    let headers = header_signatures(&db, file);
    assert!(headers.diagnostics.is_empty(), "{:?}", headers.diagnostics);
    assert_eq!(log.count_executions_matching("resolve("), 0);
    assert_eq!(log.count_executions_matching("declaration_signatures"), 0);
    let completed = resolve(&db, file);
    assert!(
        completed.diagnostics.is_empty(),
        "{:?}",
        completed.diagnostics
    );
}

#[test]
fn header_errors_and_missing_import_recovery_remain_visible() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "invalid.aru".into(),
        "import absent\nfunc main(x: MissingType): int { return absent.foo() }".into(),
    );
    let headers = header_signatures(&db, file);
    assert!(headers
        .diagnostics
        .iter()
        .any(|d| d.code == DiagCode::M001UnresolvedImport));
    assert!(headers
        .diagnostics
        .iter()
        .any(|d| d.code == DiagCode::N002UndefinedType));
    let completed = resolve(&db, file);
    assert!(!completed
        .diagnostics
        .iter()
        .any(|d| d.code == DiagCode::N001UndefinedValue));
}

#[test]
fn cyclic_header_signatures_recover_without_requesting_bodies() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let a = db.new_file(
        "a.aru".into(),
        "from b import { other }\npublic func entry(): int { return missing_a }".into(),
    );
    db.new_file(
        "b.aru".into(),
        "from a import { entry }\npublic func other(): int { return missing_b }".into(),
    );
    let signatures = header_signatures(&db, a);
    assert!(signatures
        .diagnostics
        .iter()
        .any(|d| d.message.contains("cyclic")));
    assert!(!signatures
        .diagnostics
        .iter()
        .any(|d| d.code == DiagCode::N001UndefinedValue));
    assert_eq!(log.count_executions_matching("resolve("), 0);
}
