//! Non-escaping callbacks (RFC 0032, L1): what the type checker accepts and
//! which diagnostic each rejected form gets.
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

const PRELUDE: &str = r#"
struct Handle { raw: ptr[u8] }
func apply(x: int, f: (int) -> int): int { return f(x) }
func double(x: int): int { return x * 2 }
func isPositive(x: int): bool { return x > 0 }
func applyT<T>(f: (T) -> T, x: T): T { return f(x) }
func mapOne<T, R>(x: T, f: (T) -> R): R { return f(x) }
func ident<T>(x: T): T { return x }
func loose<T, U>(x: T): T { return x }
func negate(b: bool): bool { return !b }
func keep<T>(a: T, b: T): T { return a }
struct Pair<T> { first: T, second: T }
func Pair.combine(self: ref Pair<T>, f: (T, T) -> T): T { return f(self.first, self.second) }
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
            "named function with another signature",
            "    return apply(1, isPositive)",
            DiagCode::T003IncompatibleCallArg,
        ),
        (
            "not callable",
            "    return apply(1, 5)",
            DiagCode::T003IncompatibleCallArg,
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
    ];
    for (name, body, code) in cases {
        assert_eq!(errors(body), vec![*code], "{name}");
    }
}

#[test]
fn borrowed_captures_are_accepted() {
    for body in [
        "    let raw: ptr[u8] = nil\n    let n = Handle { raw: raw }\n    return apply(1, (x) => { let m = n.raw\n        return x })",
        "    let mut n = 0\n    let r = apply(1, (x) => { n = n + 1\n        return x })\n    return r + n",
    ] {
        let accepted = errors(body);
        assert!(accepted.is_empty(), "{body}: {accepted:?}");
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

#[test]
fn named_functions_and_owning_lambdas_are_callbacks() {
    for body in [
        "    return apply(1, double)",
        "    let g: (int) -> int = double\n    return g(1)",
        "    let k: int = 2\n    return apply(1, own (x: int) => x + k)",
        "    let k: int = 2\n    return apply(1, move (x) => x + k)",
    ] {
        assert!(errors(body).is_empty(), "{body}: {:?}", errors(body));
    }
}

#[test]
fn a_capture_of_generic_type_is_rejected() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "generic.aru".into(),
        "func twice(f: () -> void): void { f() }\nfunc forward(f: (int) -> void): void {\n    twice(() => f(1))\n}\nfunc main(): int { return 0 }\n".into(),
    );
    let codes = type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, vec![DiagCode::U001FeatureNotSupported]);
}

#[test]
fn generic_callees_take_callbacks_with_explicit_or_inferred_arguments() {
    for body in [
        "    return mapOne<int, int>(4, (x) => x * 10)",
        "    let b: bool = mapOne<int, bool>(4, (x) => x > 3)\n    return 0",
        "    return applyT(ident, 9)",
        "    return applyT<int>(double, 9)",
        "    let p = Pair<int> { first: 2, second: 3 }\n    return p.combine((a, b) => a * b)",
        "    let p = Pair<int> { first: 2, second: 3 }\n    return p.combine(keep)",
    ] {
        assert!(errors(body).is_empty(), "{body}: {:?}", errors(body));
    }
}

#[test]
fn a_callback_fixes_the_parameters_it_mentions() {
    let cases: &[(&str, &str, DiagCode)] = &[
        (
            "generic function with a parameter outside the callback",
            "    return applyT(loose, 1)",
            DiagCode::T001CannotInferType,
        ),
        (
            "plain function binds T before the value argument",
            "    let b = applyT(negate, 5)\n    return 0",
            DiagCode::T003IncompatibleCallArg,
        ),
        (
            "explicit arguments name only the declared parameters",
            "    return mapOne<int, int, int>(4, (x) => x)",
            DiagCode::T012WrongArgCount,
        ),
    ];
    for (name, body, code) in cases {
        assert_eq!(errors(body), vec![*code], "{name}");
    }
}

#[test]
fn an_edit_in_another_item_keeps_closure_identities() {
    use salsa::Setter;
    fn source(alpha_body: &str) -> String {
        format!(
            "func apply(x: int, f: (int) -> int): int {{ return f(x) }}\nfunc double(x: int): int {{ return x * 2 }}\nfunc alpha(): int {{\n{alpha_body}\n}}\nfunc beta(): int {{\n    let k: int = 3\n    return apply(1, (x) => x + k) + apply(2, double)\n}}\n"
        )
    }
    fn identities(db: &DatabaseImpl, file: arandu_query::SourceFile) -> Vec<String> {
        let checked = type_check(db, file);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let mut identities = checked
            .type_info
            .closures
            .values()
            .map(|recipe| {
                format!(
                    "{} {:?} {:?} {}",
                    recipe.struct_name,
                    recipe.struct_symbol,
                    recipe.method_symbol,
                    recipe.host_name
                )
            })
            .collect::<Vec<_>>();
        identities.sort();
        identities
    }
    let mut db = DatabaseImpl::default();
    let file = db.new_file("identity.aru".into(), source("    return 1"));
    let before = identities(&db, file);
    assert_eq!(before.len(), 2, "{before:?}");
    file.set_text(&mut db).to(std::sync::Arc::from(source(
        "    let unrelated = 40 + 2\n    return unrelated - 41",
    )));
    assert_eq!(identities(&db, file), before);
}
