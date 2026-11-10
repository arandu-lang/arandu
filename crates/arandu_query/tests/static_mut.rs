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
fn reads_and_writes_lower_to_static_storage() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static_mut.aru".into(),
        "static mut COUNTER: int = 0\nfunc main(): int {\n    unsafe {\n        COUNTER = COUNTER + 1\n    }\n    return unsafe { COUNTER }\n}\n"
            .into(),
    );
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        codes(&program.diagnostics).is_empty(),
        "{:?}",
        program.diagnostics
    );
    let statics = &program.artifacts.amir.statics;
    assert_eq!(statics.len(), 1);
    assert_eq!(statics[0].name.as_str(), "ar_static_static_mut_COUNTER");
}

#[test]
fn an_initial_value_without_a_data_image_is_t101() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static_mut.aru".into(),
        "static mut NAME: str = \"x\"\nfunc main(): int {\n    return unsafe { NAME.len } as int\n}\n".into(),
    );
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        codes(&program.diagnostics).contains(&DiagCode::T101StaticWithoutDataImage),
        "{:?}",
        program.diagnostics
    );
}
