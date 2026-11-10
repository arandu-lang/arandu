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
fn unsafe_extern_and_exported_functions_are_not_values() {
    let cases: &[(&str, &str, DiagCode)] = &[
        (
            "@Unsafe",
            "@Unsafe\nfunc raw(): int { return 0 }\nfunc main(): int {\n    let f = raw\n    return 0\n}\n",
            DiagCode::O013ExternRequiresUnsafe,
        ),
        (
            "@Export",
            "@Export(\"api_value\")\npublic func api(): int { return 1 }\nfunc main(): int {\n    let f = api\n    return 0\n}\n",
            DiagCode::U001FeatureNotSupported,
        ),
    ];
    for (name, source, code) in cases {
        assert_eq!(errors(source), vec![*code], "{name}");
    }
}
