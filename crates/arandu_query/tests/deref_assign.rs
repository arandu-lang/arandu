//! `*place = value`: writing through a reference needs `mut ref` (or a raw
//! pointer in `unsafe`) and a reference or pointer to dereference.
use arandu_middle::{DiagCode, Severity};
use arandu_query::{passes, DatabaseImpl};

fn errors(source: &str) -> Vec<DiagCode> {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "deref.aru".into(),
        format!("{source}\nfunc main(): int {{ return 0 }}\n"),
    );
    passes::type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn writes_need_a_mutable_reference() {
    assert!(errors("func f(r: mut ref int): void {\n    *r = 1\n}").is_empty());
    assert_eq!(
        errors("func f(r: ref int): void {\n    *r = 1\n}"),
        [DiagCode::T026CannotAssignImmutable]
    );
    assert_eq!(
        errors("func f(): void {\n    let mut x = 1\n    *x = 2\n}"),
        [DiagCode::T005OperatorNotApplicable]
    );
}

#[test]
fn raw_pointers_need_unsafe() {
    assert_eq!(
        errors("func f(p: ptr[int]): void {\n    *p = 1\n}"),
        [DiagCode::O012AllocRequiresUnsafe]
    );
    assert!(
        errors("func f(p: ptr[int]): void {\n    unsafe {\n        *p = 1\n    }\n}").is_empty()
    );
}
