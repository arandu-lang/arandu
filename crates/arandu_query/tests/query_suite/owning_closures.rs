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
    database_for(source, None)
}

fn database_for(source: &str, triple: Option<&str>) -> (DatabaseImpl, arandu_query::SourceFile) {
    let mut db = DatabaseImpl::default();
    if let Some(triple) = triple {
        db.set_target(
            arandu_middle::DataLayout::ptr_width(4),
            arandu_middle::db::TargetIdentity::from_triple(triple).expect("canonical triple"),
        );
    }
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
            "struct Guard { raw: ptr[int], id: int }\n@Destructor\nfunc Guard.destroy(self: own Guard): void {}\nfunc f(guard: Guard): own () -> int + Send { return () => guard.id }",
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

#[test]
fn closures_lend_to_callbacks_and_take_function_values() {
    let codes = errors(
        r#"
            func apply(x: int, f: (int) -> int): int { return f(x) }
            func double(x: int): int { return x * 2 }
            func main(): int {
                let k = 3
                let c: own (int) -> int = (x) => x + k
                let thin: (int) -> int = double
                let owned: own (int) -> int = thin
                return apply(1, c) + owned(1)
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_temporary_closure_is_called_or_lent_to_a_callback() {
    let codes = errors(
        r#"
            func apply(x: int, f: (int) -> int): int { return f(x) }
            func make(): own (int) -> int { return (x) => x }
            func main(): int { return apply(1, make()) + make()(2) }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn an_owning_closure_never_crosses_a_foreign_boundary() {
    let codes = errors(
        "extern \"C\" {\n    func keep(f: own () -> int): int\n}\nfunc main(): int { return 0 }\n",
    );
    assert_eq!(codes, vec![DiagCode::T049InvalidFfiExport]);
}

#[test]
fn bare_metal_rejects_only_closures_that_allocate() {
    let source = r#"
        func triple(x: int): int { return x * 3 }
        func plain(): own (int) -> int { return triple }
        func capturing(k: int): own (int) -> int { return (x) => x + k }
        func main(): int { return 0 }
    "#;
    let (db, file) = database_for(source, Some("thumbv7em-none-eabihf"));
    let codes: Vec<_> = type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, vec![DiagCode::T099RequiresOperatingSystem]);
}

#[test]
fn a_once_closure_is_consumed_by_its_call() {
    let twice = all_errors(
        r#"
            import std.alloc.string as strings
            func make(text: strings.String): own once () -> strings.String {
                return () => text
            }
            func main(): int {
                let f = make(strings.String.from("a"))
                let first = f()
                let second = f()
                return 0
            }
        "#,
    );
    assert!(twice.iter().any(|code| code == "O001"), "{twice:?}");

    let through_reference = errors(
        r#"
            func call(f: mut ref own once () -> int): int { return f() }
            func main(): int { return 0 }
        "#,
    );
    assert_eq!(through_reference, vec![DiagCode::T026CannotAssignImmutable]);
}

#[test]
fn closures_become_once_but_never_the_reverse() {
    let accepted = errors(
        r#"
            func run(f: own once () -> int): int { return f() }
            func sendable(f: own () -> int + Send): own once () -> int { return f }
            func main(): int {
                let counter: own () -> int = () => 1
                return run(counter)
            }
        "#,
    );
    assert!(accepted.is_empty(), "{accepted:?}");
    let rejected = errors(
        r#"
            func again(f: own once () -> int): own () -> int { return f }
            func main(): int { return 0 }
        "#,
    );
    assert!(!rejected.is_empty());
}

#[test]
fn a_lambda_with_annotated_parameters_needs_no_expected_type() {
    let accepted = errors(
        r#"
            func main(): int {
                let double = (x: int) => x * 2
                let k = 1
                let add = (x: int) => x + k
                return double(add(1)) - 4
            }
        "#,
    );
    assert!(accepted.is_empty(), "{accepted:?}");
    let untyped = errors("func main(): int {\n    let f = (x) => x\n    return 0\n}\n");
    assert_eq!(untyped, vec![DiagCode::T001CannotInferType]);
}

/// The editor reports what the build reports for a closure body: its
/// synthesized method is checked with the function that creates it.
#[test]
fn a_consumed_capture_is_reported_in_the_editor() {
    for body in [
        "func make(text: strings.String): own () -> strings.String { return () => text }\n",
        "func apply(f: () -> strings.String): strings.String { return f() }\nfunc cb(text: strings.String): strings.String { return apply(() => text) }\n",
    ] {
        let source = format!(
            "import std.alloc.string as strings\n{body}func main(): int {{ return 0 }}\n"
        );
        assert_eq!(all_errors(&source), ["O016"], "{body}");
    }
}

#[test]
fn owning_closure_rejects_sync_bound_and_multi_bound_combinations() {
    let sync_only = errors(
        r#"
            func take(f: own (int) -> int + Sync): int { return f(1) }
            func main(): int { return 0 }
        "#,
    );
    assert_eq!(sync_only, vec![DiagCode::T011GenericConstraintNotSatisfied]);

    let multi_bound = errors(
        r#"
            func take(f: own (int) -> int + Send + Sync): int { return f(1) }
            func main(): int { return 0 }
        "#,
    );
    assert_eq!(
        multi_bound,
        vec![DiagCode::T011GenericConstraintNotSatisfied]
    );
}

/// An owning closure that only assigns to its copy of a captured `Copy`
/// local is warned (W009); one that reads its own state is not.
#[test]
fn a_captured_copy_that_is_only_assigned_warns() {
    let warnings = |source: &str| -> Vec<String> {
        let (db, file) = database(source);
        arandu_query::file_ide_diagnostics(&db, file)
            .iter()
            .filter(|diagnostic| diagnostic.code.starts_with('W'))
            .map(|diagnostic| diagnostic.code.clone())
            .collect()
    };
    let lost = "func later(job: own () -> void) {\n    let mut j = job\n    j()\n}\nfunc main(): int {\n    let mut count = 0\n    later {\n        count += 1\n    }\n    return count\n}\n";
    assert_eq!(warnings(lost), ["W009"]);
    let stateful = "func counter(): own () -> int {\n    let mut n = 0\n    return () => {\n        n += 1\n        n\n    }\n}\nfunc main(): int {\n    let mut next = counter()\n    return next() - 1\n}\n";
    assert!(warnings(stateful).is_empty(), "{:?}", warnings(stateful));
}
