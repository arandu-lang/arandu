//! Non-escaping callbacks (RFC 0032, L1): what the type checker accepts and
//! which diagnostic each rejected form gets.
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

const PRELUDE: &str = r#"
struct Handle { raw: ptr[u8] }
func apply(x: int, f: (int) -> int): int { return f(x) }
func double(x: int): int { return x * 2 }
"#;

fn errors(body: &str) -> Vec<DiagCode> {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "closures.aru".into(),
        format!("{PRELUDE}\nfunc main(): int {{\n{body}\n}}\n"),
    );
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn callback_arguments_with_copy_captures_are_accepted() {
    let accepted = errors(
        "    let k: int = 1\n    let a = apply(1, (x: int) => x + k)\n    let b = apply(a, (x) => { if x > 0 { return x }\n        return k })\n    return b",
    );
    assert!(accepted.is_empty(), "{accepted:?}");
}

#[test]
fn each_rejected_closure_reports_one_diagnostic() {
    let cases: &[(&str, &str, DiagCode)] = &[
        (
            "escaping",
            "    let g = (x: int) => x + 1\n    return 0",
            DiagCode::U001FeatureNotSupported,
        ),
        (
            "named function",
            "    return apply(1, double)",
            DiagCode::U001FeatureNotSupported,
        ),
        (
            "not callable",
            "    return apply(1, 5)",
            DiagCode::T003IncompatibleCallArg,
        ),
        (
            "shared borrow",
            "    let raw: ptr[u8] = nil\n    let n = Handle { raw: raw }\n    return apply(1, (x) => { let m = n\n        return x })",
            DiagCode::U001FeatureNotSupported,
        ),
        (
            "mutable borrow",
            "    let mut n = 0\n    return apply(1, (x) => { n = n + 1\n        return x })",
            DiagCode::U001FeatureNotSupported,
        ),
        (
            "arity",
            "    return apply(1, (x, y) => x)",
            DiagCode::T003IncompatibleCallArg,
        ),
        (
            "annotation",
            "    return apply(1, (x: bool) => 1)",
            DiagCode::T003IncompatibleCallArg,
        ),
        (
            "result",
            "    return apply(1, (x) => true)",
            DiagCode::T004IncompatibleReturnType,
        ),
        (
            "indirect call",
            "    let g: (int) -> int = double\n    return g(1)",
            DiagCode::T033IndirectCallNotSupported,
        ),
    ];
    for (name, body, code) in cases {
        assert_eq!(errors(body), vec![*code], "{name}");
    }
}

#[test]
fn moved_captures_are_accepted_in_a_move_closure() {
    let accepted = errors(
        "    let raw: ptr[u8] = nil\n    let n = Handle { raw: raw }\n    return apply(1, move |x| { let m = n\n        return x })",
    );
    assert!(accepted.is_empty(), "{accepted:?}");
}

#[test]
fn a_borrowed_callback_cannot_escape_the_call() {
    for (name, source) in [
        (
            "field",
            "struct Box { g: (int) -> int }\nfunc keep(f: (int) -> int): int { let b = Box { g: f }\n return 0 }",
        ),
        (
            "return",
            "func leak(f: (int) -> int): func(int) int { return f }",
        ),
        (
            "typed local",
            "func assign(f: (int) -> int): int { let g: (int) -> int = f\n return 0 }",
        ),
    ] {
        let mut db = DatabaseImpl::default();
        let file = db.new_file(
            "escape.aru".into(),
            format!("{source}\nfunc main(): int {{ return 0 }}\n"),
        );
        let codes = type_check(&db, file)
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(codes, vec![DiagCode::T100BorrowedCallbackEscapes], "{name}");
    }
}
