//! A generic struct literal infers its parameters structurally from its field
//! values (TYP.NEXT S1 regression).
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

const PRELUDE: &str = r#"
struct Fresh { tag: int }
struct One<T> { value: T }
struct Pair<T> { first: T, second: T }
struct Holder<T> { inner: One<T>, count: int }
func makeFresh(tag: int): Fresh { return Fresh { tag: tag } }
"#;

fn errors(body: &str) -> Vec<DiagCode> {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "literals.aru".into(),
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
fn nominal_nested_and_called_values_infer_the_parameters() {
    for body in [
        "    let p = Pair { first: Fresh { tag: 1 }, second: Fresh { tag: 0 } }\n    return p.first.tag",
        "    let h = Holder { inner: One { value: Fresh { tag: 5 } }, count: 2 }\n    return h.inner.value.tag",
        "    let o = One { value: makeFresh(9) }\n    return o.value.tag",
        "    let one = One { value: 7 }\n    let held = Holder { inner: one, count: 1 }\n    return held.inner.value",
    ] {
        assert!(errors(body).is_empty(), "{body}: {:?}", errors(body));
    }
}

#[test]
fn conflicting_field_values_report_once() {
    assert_eq!(
        errors("    let p = Pair { first: 1, second: true }\n    return 0"),
        vec![DiagCode::T002IncompatibleAssignment]
    );
}
