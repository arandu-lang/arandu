//! Owning closures, `own (A) -> R [+ Send]` (RFC 0032, L3): the closure
//! types come from the ambient `std.alloc.closure`, captures are owned, and
//! only the owner or a `mut ref` may call.
#![allow(clippy::expect_used)]
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::type_check;
use std::path::{Path, PathBuf};

fn register_stdlib(db: &mut DatabaseImpl, directory: &Path, root: &Path) {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .expect("stdlib directory")
        .map(|entry| entry.expect("stdlib entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            register_stdlib(db, &path, root);
        } else if path.extension().is_some_and(|extension| extension == "aru") {
            let relative = path
                .strip_prefix(root)
                .expect("beneath the stdlib root")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path).expect("stdlib source");
            db.new_stdlib_file(format!("stdlib/{relative}"), source);
        }
    }
}

fn database(source: &str) -> (DatabaseImpl, arandu_query::SourceFile) {
    let mut db = DatabaseImpl::default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .expect("workspace stdlib");
    register_stdlib(&mut db, &root, &root);
    let file = db.new_file("own.aru".to_string(), source.to_string());
    (db, file)
}

/// Every error of the file, ownership checks included.
fn all_errors(source: &str) -> Vec<String> {
    let (db, file) = database(source);
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

fn errors(source: &str) -> Vec<DiagCode> {
    let (db, file) = database(source);
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn owning_closures_need_no_import() {
    let codes = errors(
        r#"
            func counter(start: int): own () -> int {
                let mut n = start
                return own () => {
                    n += 1
                    return n
                }
            }
            func triple(x: int): int { return x * 3 }
            func keep(f: own (int) -> int, g: own (int) -> int + Send): int {
                return f(1) + g(2)
            }
            func main(): int {
                let next = counter(1)
                let by = 4
                return next() + keep(triple, (x) => x + by)
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn each_rejected_owning_closure_reports_one_diagnostic() {
    let cases: &[(&str, &str, DiagCode)] = &[
        (
            "a bound other than Send",
            "struct Tag {}\nfunc f(g: own () -> int + Tag): int { return 0 }",
            DiagCode::T011GenericConstraintNotSatisfied,
        ),
        (
            "a call through a shared reference",
            "func f(g: ref own () -> int): int { return g() }",
            DiagCode::T026CannotAssignImmutable,
        ),
        (
            "a call on a temporary",
            "func make(): own () -> int { return () => 1 }\nfunc f(): int { return make()() }",
            DiagCode::T026CannotAssignImmutable,
        ),
        (
            "a borrowed view captured",
            "func f(name: str): own () -> int { return () => name.len() as int }",
            DiagCode::O010EscapeOfBorrowedValue,
        ),
        (
            "a reference captured",
            "func f(value: ref int): own () -> int { return () => *value }",
            DiagCode::O010EscapeOfBorrowedValue,
        ),
        (
            "a capture that is not Send",
            "struct Guard { id: int }\n@Destructor\nfunc Guard.destroy(self: own Guard): void {}\nfunc f(guard: Guard): own () -> int + Send { return () => guard.id }",
            DiagCode::T025InterfaceNotSatisfied,
        ),
        (
            "a function of another signature",
            "func g(x: bool): int { return 0 }\nfunc f(): own (int) -> int { return g }",
            DiagCode::T002IncompatibleAssignment,
        ),
    ];
    for (name, source, expected) in cases {
        let codes = errors(&format!("{source}\nfunc main(): int {{ return 0 }}\n"));
        assert_eq!(codes, vec![*expected], "{name}");
    }
}

#[test]
fn an_owner_or_an_exclusive_reference_may_call() {
    let codes = errors(
        r#"
            func viaOwner(f: own (int) -> int): int { return f(1) }
            func viaExclusive(f: mut ref own (int) -> int): int { return f(1) }
            func main(): int {
                let mut f: own (int) -> int = (x) => x
                return viaExclusive(mut ref f) + viaOwner(f)
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn an_owning_closure_moves_and_is_not_copy() {
    let codes = all_errors(
        r#"
            func viaOwner(f: own (int) -> int): int { return f(1) }
            func main(): int {
                let f: own (int) -> int = (x) => x
                let a = viaOwner(f)
                return a + viaOwner(f)
            }
        "#,
    );
    assert_eq!(codes, vec!["O001".to_string()]);
}
