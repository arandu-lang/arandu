//! The cooperative executor must retain the coroutine result type in its handle.
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

fn check_join(statement: &str, rejection: Option<DiagCode>) {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/std/runtime/executor.aru".into(),
        include_str!("../../../stdlib/std/runtime/executor.aru").into(),
    );
    let file = db.new_file(
        "handle.aru".into(),
        format!(
            r#"
import std.runtime.executor as rt
async func answer(): int {{ return 42 }}
func main(): int {{
    let ex = rt.SyncExecutor.new()
    let handle = ex.spawn(answer())
    {statement}
    ex.cancel(handle)
    return 0
}}
"#
        ),
    );
    let result = type_check(&db, file);
    if let Some(code) = rejection {
        assert!(
            result.diagnostics.iter().any(|d| d.code == code),
            "{statement}: {:?}",
            result.diagnostics
        );
    } else {
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{statement}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn join_rejects_explicit_wrong_result_type() {
    check_join(
        "let result = ex.join<bool>(handle)",
        Some(DiagCode::T003IncompatibleCallArg),
    );
}

#[test]
fn join_rejects_wrong_expected_result_type() {
    check_join(
        "let result: bool = ex.join(handle)",
        Some(DiagCode::T002IncompatibleAssignment),
    );
}

#[test]
fn join_infers_result_from_handle_without_expected_type() {
    check_join(
        "let result = ex.join(handle)\nlet value: int = result",
        None,
    );
}
