//! Calls through function values (RFC 0032, L2): accepted at typeck, with the
//! safety obligations checked where a function becomes a value.
#![allow(clippy::unwrap_used)]
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::type_check;

fn errors(source: &str) -> Vec<DiagCode> {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("ind.aru".to_string(), source.to_string());
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn a_function_value_is_called_indirectly() {
    let codes = errors(
        r#"
            func add(a: int, b: int): int { return a + b }
            func main(): int {
                let f = add
                return f(1, 2)
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_function_value_in_an_inferred_struct_field_is_called() {
    let codes = errors(
        r#"
            struct Job<T> { callback: T }
            func count(value: int): int { return value + 1 }
            func main(): int {
                let job = Job { callback: count }
                return (job.callback)(41)
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_function_value_keeps_its_parameter_types() {
    let codes = errors(
        r#"
            func add(a: int, b: int): int { return a + b }
            func main(): int {
                let f = add
                return f(1, true)
            }
        "#,
    );
    assert_eq!(codes, vec![DiagCode::T003IncompatibleCallArg]);
}

#[test]
fn unsafe_extern_functions_are_not_values() {
    let source =
        "@Unsafe\nfunc raw(): int { return 0 }\nfunc main(): int {\n    let f = raw\n    return 0\n}\n";
    assert_eq!(errors(source), vec![DiagCode::O013ExternRequiresUnsafe]);
}

#[test]
fn exported_functions_with_aggregates_are_values() {
    let source = "struct Pair { a: int, b: int }\n@Export(\"api_pair\")\npublic func api(p: Pair): Pair { return Pair { a: p.a + 1, b: p.b + 2 } }\nfunc main(): int {\n    let f = api\n    let res = f(Pair { a: 10, b: 20 })\n    return res.a + res.b\n}\n";
    assert!(errors(source).is_empty(), "{:?}", errors(source));
}

/// With scalars and thin pointers alone, the C entry point of an `@Export`
/// function is also its internal one, so it is a function value.
#[test]
fn exported_functions_with_scalar_signatures_are_values() {
    let source = "@Export(\"api_value\")\npublic func api(x: int, p: ptr[u8]): f64 { return 1.0 }\nfunc main(): int {\n    let f = api\n    return 0\n}\n";
    assert!(errors(source).is_empty(), "{:?}", errors(source));
}

const FORMS: &str = r#"
func identity<T>(x: T): T { return x }
func double(x: int): int { return x * 2 }
func negate(b: bool): bool { return !b }
func apply(x: int, f: (int) -> int): int { return f(x) }
"#;

fn body_errors(body: &str) -> Vec<DiagCode> {
    errors(&format!("{FORMS}\nfunc main(): int {{\n{body}\n}}\n"))
}

#[test]
fn generic_functions_lambdas_and_values_are_function_values() {
    for body in [
        "    let f: (int) -> int = identity\n    return f(1)",
        "    let f: (int) -> int = (x) => x + 1\n    return f(1)",
        "    let f = double\n    return apply(1, f)",
        "    return apply(1, identity)",
    ] {
        assert!(
            body_errors(body).is_empty(),
            "{body}: {:?}",
            body_errors(body)
        );
    }
}

#[test]
fn each_rejected_function_value_reports_one_diagnostic() {
    let cases: &[(&str, &str, DiagCode)] = &[
        (
            "generic value without a type to fix it",
            "    let f = identity\n    return 0",
            DiagCode::T001CannotInferType,
        ),
        (
            "capturing lambda as a value",
            "    let k = 2\n    let f: (int) -> int = (x) => x + k\n    return f(1)",
            DiagCode::T002IncompatibleAssignment,
        ),
        (
            "function value of another signature as a callback",
            "    let f = negate\n    return apply(1, f)",
            DiagCode::T003IncompatibleCallArg,
        ),
    ];
    for (name, body, code) in cases {
        assert_eq!(body_errors(body), vec![*code], "{name}");
    }
}

#[test]
fn exported_aggregate_functions_lower_via_runtime_program_same_and_cross_module() {
    use arandu_query::runtime::runtime_program;

    let mut db = DatabaseImpl::default();
    let api_file = db.new_file(
        "api_mod.aru".to_string(),
        r#"
module api_mod
@Repr("C")
public struct Pair {
    public a: int
    public b: int
}
@Export("api_pair_fn")
public func api(p: Pair): Pair {
    return Pair { a: p.a + 1, b: p.b + 2 }
}
"#
        .to_string(),
    );
    let main_file = db.new_file(
        "main.aru".to_string(),
        r#"
import api_mod as m
@Repr("C")
struct LocalPair {
    x: int
    y: int
}
@Export("local_pair_fn")
public func localApi(p: LocalPair): LocalPair {
    return LocalPair { x: p.x + 10, y: p.y + 20 }
}
func callCross(f: (m.Pair) -> m.Pair): int {
    let out = f(m.Pair { a: 3, b: 4 })
    return out.a + out.b
}
func main(): int {
    let f1 = m.api
    let f2 = m.api
    let g = localApi
    let lp = g(LocalPair { x: 1, y: 2 })
    return callCross(f1) + callCross(f2) + lp.x + lp.y
}
"#
        .to_string(),
    );

    let _ = type_check(&db, api_file);
    let program = runtime_program(&db, main_file);
    let errs: Vec<_> = program
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "unexpected diagnostics: {errs:?}");

    let thunk_count = program
        .artifacts
        .amir
        .funcs
        .iter()
        .filter(|f| {
            program
                .artifacts
                .type_check
                .symbols
                .try_get(f.symbol)
                .is_some_and(|s| s.name.starts_with("__ar_export_thunk_"))
        })
        .count();
    assert_eq!(
        thunk_count, 2,
        "expected exactly one thunk per distinct @Export aggregate function (one cross-module, one local)"
    );
}

#[test]
fn bare_func_pointer_parameter_can_escape_and_rejects_capturing_lambdas() {
    let accepted = errors(
        r#"
            func addOne(x: int): int { return x + 1 }
            func register(cb: func(int) -> int): func(int) -> int { return cb }
            func main(): int {
                let stored = register(addOne)
                let nonCap = register((x) => x + 2)
                return stored(10) + nonCap(20)
            }
        "#,
    );
    assert!(
        accepted.is_empty(),
        "bare func(A) -> R must store/return without T100: {accepted:?}"
    );

    let rejected_capture = errors(
        r#"
            func register(cb: func(int) -> int): func(int) -> int { return cb }
            func main(): int {
                let k = 5
                let _ = register((x) => x + k)
                return 0
            }
        "#,
    );
    assert!(
        !rejected_capture.is_empty(),
        "capturing lambda passed to `func(int) -> int` must be rejected"
    );
}
