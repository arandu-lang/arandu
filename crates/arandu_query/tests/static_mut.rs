//! `static mut` is checked by the front end but has no lowered storage yet:
//! it fails closed instead of dropping writes or linking a missing symbol.
#![allow(clippy::expect_used)]

use arandu_middle::{DiagCode, Severity};
use arandu_query::{passes, DatabaseImpl};

fn codes(diagnostics: &[arandu_middle::Diagnostic]) -> Vec<DiagCode> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn access_outside_unsafe_is_o017() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static_mut.aru".into(),
        "static mut COUNTER: int = 0\nfunc bump(): void {\n    COUNTER = COUNTER + 1\n}\nfunc main(): int { return 0 }\n"
            .into(),
    );
    let checked = passes::type_check(&db, file);
    assert!(
        codes(&checked.diagnostics).contains(&DiagCode::O017StaticMutRequiresUnsafe),
        "{:?}",
        checked.diagnostics
    );
}

#[test]
fn lowering_rejects_reads_and_writes_until_storage_exists() {
    for body in [
        "    unsafe {\n        COUNTER = 1\n    }\n    return 0",
        "    return unsafe { COUNTER }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file(
            "static_mut.aru".into(),
            format!("static mut COUNTER: int = 0\nfunc main(): int {{\n{body}\n}}\n"),
        );
        assert!(codes(&passes::type_check(&db, file).diagnostics).is_empty());
        let program = arandu_query::runtime::runtime_program(&db, file);
        assert!(
            codes(&program.diagnostics).contains(&DiagCode::U001FeatureNotSupported),
            "{body}: {:?}",
            program.diagnostics
        );
    }
}
