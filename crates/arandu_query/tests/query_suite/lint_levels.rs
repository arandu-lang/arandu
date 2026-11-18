//! `@Suppress`, `@Deny` and `@Forbid` set the level of a warning in a
//! function body; `@NoSuspend` is `@Effects(NoSuspend)`.
#![allow(clippy::expect_used)]
use arandu_query::db::DatabaseImpl;
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

/// `(code, is an error)` of every diagnostic, in order.
fn diagnostics(body: &str) -> Vec<(String, bool)> {
    let mut db = DatabaseImpl::default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .expect("workspace stdlib");
    register_stdlib(&mut db, &root, &root);
    let source = format!(
        "from std.core.result import {{ Result }}\nfunc risky(): Result<int, int> {{ return Result.Ok(1) }}\n{body}\nfunc main(): int {{ return 0 }}\n"
    );
    let file = db.new_file("lints.aru".to_string(), source);
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .map(|diagnostic| (diagnostic.code.clone(), diagnostic.severity == 0))
        .collect()
}

#[test]
fn a_warning_keeps_its_level_without_annotations() {
    assert_eq!(
        diagnostics("func plain() {\n    let r = risky()\n}"),
        [("W006".to_string(), false)]
    );
}

#[test]
fn suppress_removes_the_warning() {
    assert!(diagnostics("@Suppress(\"W006\")\nfunc quiet() {\n    let r = risky()\n}").is_empty());
}

#[test]
fn suppress_accepts_multiple_arguments() {
    assert!(diagnostics(
        "@Suppress(\"W006\", \"unused_variable\")\nfunc quiet() {\n    let r = risky()\n}"
    )
    .is_empty());
}

#[test]
fn deny_accepts_multiple_arguments() {
    assert_eq!(
        diagnostics("@Deny(\"W006\", \"shadowing\")\nfunc strict() {\n    let r = risky()\n}"),
        [("W006".to_string(), true)]
    );
}

#[test]
fn deny_and_forbid_make_it_an_error() {
    for annotation in ["Deny", "Forbid"] {
        assert_eq!(
            diagnostics(&format!(
                "@{annotation}(\"W006\")\nfunc strict() {{\n    let r = risky()\n}}"
            )),
            [("W006".to_string(), true)],
            "{annotation}"
        );
    }
}

#[test]
fn a_lint_is_named_by_name_code_or_all() {
    for lint in ["unhandled_result", "W006", "warnings"] {
        assert!(
            diagnostics(&format!(
                "@Suppress(\"{lint}\")\nfunc quiet() {{\n    let r = risky()\n}}"
            ))
            .is_empty(),
            "{lint}"
        );
    }
}

#[test]
fn a_forbidden_lint_cannot_be_suppressed() {
    let found = diagnostics(
        "@Forbid(\"W006\")\n@Suppress(\"W006\")\nfunc both() {\n    let r = risky()\n}",
    );
    assert!(found.contains(&("W006".to_string(), true)), "{found:?}");
    assert!(found.contains(&("N014".to_string(), true)), "{found:?}");
}

#[test]
fn a_level_names_a_warning() {
    assert_eq!(
        diagnostics("@Suppress(\"T002\")\nfunc bad() { }"),
        [("N014".to_string(), true)]
    );
}

#[test]
fn no_suspend_rejects_an_await() {
    let found = diagnostics(
        "async func compute(): int { return 42 }\n@NoSuspend\nasync func execute(): int {\n    let v = await compute()\n    return v\n}",
    );
    assert!(found.contains(&("T039".to_string(), true)), "{found:?}");
    assert!(diagnostics("@NoSuspend\nfunc calm(): int { return 1 }").is_empty());
}
