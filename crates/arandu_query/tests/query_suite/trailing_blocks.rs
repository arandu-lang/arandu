//! A block after a call is its last argument, `() => { ... }`.
use arandu_query::db::DatabaseImpl;

fn diagnostics(source: &str) -> Vec<(String, Vec<String>)> {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("blocks.aru".to_string(), source.to_string());
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| (diagnostic.code.clone(), diagnostic.notes.clone()))
        .collect()
}

#[test]
fn a_trailing_block_is_the_last_argument() {
    let source = "func twice(f: () -> int): int { return f() + f() }\n\
        func main(): int {\n    let n = twice { 21 }\n    return n - 42\n}\n";
    assert!(diagnostics(source).is_empty());
}

#[test]
fn a_block_the_callee_cannot_take_is_explained() {
    let source = "func f(a: int): int { return a }\n\
        func main(): int {\n    return f(1) { 2 }\n}\n";
    let found = diagnostics(source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "T012");
    assert!(
        found[0].1.iter().any(|note| note.contains("last argument")),
        "{found:?}"
    );
}
