//! Typed holes `todo` (RFC 0036, H1): checked against their goal, reported
//! as T102 with candidate fills, shadowed by any declaration named `todo`.
#![allow(clippy::expect_used)]
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::type_check;
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

fn database(source: &str) -> (DatabaseImpl, arandu_query::SourceFile) {
    database_for(source, None)
}

fn database_for(source: &str, triple: Option<&str>) -> (DatabaseImpl, arandu_query::SourceFile) {
    let mut db = DatabaseImpl::default();
    if let Some(triple) = triple {
        db.set_target(
            arandu_middle::DataLayout::ptr_width(4),
            arandu_middle::db::TargetIdentity::from_triple(triple).expect("canonical triple"),
        );
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .expect("workspace stdlib");
    register_stdlib(&mut db, &root, &root);
    let file = db.new_file("holes.aru".to_string(), source.to_string());
    (db, file)
}

fn holes(source: &str) -> Vec<arandu_diagnostics::Diagnostic> {
    let (db, file) = database(source);
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .cloned()
        .collect()
}

fn fills(diagnostic: &arandu_diagnostics::Diagnostic) -> Vec<String> {
    diagnostic
        .hints
        .iter()
        .filter_map(|hint| hint.replacement.as_ref())
        .map(|replacement| replacement.new_text.clone())
        .collect()
}

#[test]
fn a_hole_takes_the_expected_type_and_lists_fills() {
    let source = r#"
        func area(width: int, height: int, name: str): int {
            let base = width
            return todo("multiply by the height")
        }
    "#;
    let found = holes(source);
    assert_eq!(found.len(), 1, "{found:?}");
    let hole = &found[0];
    assert_eq!(hole.code, DiagCode::T102TypedHole);
    assert_eq!(hole.message, "typed hole: expected 'int'");
    assert!(hole
        .notes
        .iter()
        .any(|note| note == "todo: multiply by the height"));
    assert_eq!(fills(hole), vec!["base", "height", "width"]);
    let start = source.find("todo(").expect("hole");
    assert_eq!(hole.span.start as usize, start);
}

#[test]
fn a_hole_in_synthesis_position_learns_its_goal_from_use() {
    let found = holes(
        r#"
        func takes(flag: bool): int { return 0 }
        func main(): int {
            let pending = todo
            return takes(pending)
        }
        "#,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, DiagCode::T102TypedHole);
    assert_eq!(found[0].message, "typed hole: expected 'bool'");
}

#[test]
fn an_unknown_goal_is_shown_and_not_reported_twice() {
    let found = holes(
        r#"
        func main(): int {
            let pending = todo()
            return 0
        }
        "#,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, DiagCode::T102TypedHole);
    assert_eq!(found[0].message, "typed hole: expected '_'");
}

#[test]
fn a_hole_does_not_cascade() {
    let found = holes(
        r#"
        struct Point { x: int, y: int }
        func main(): int {
            let p: Point = todo
            return p.x + p.y
        }
        "#,
    );
    let codes: Vec<DiagCode> = found.iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![DiagCode::T102TypedHole]);
}

#[test]
fn a_declaration_named_todo_shadows_the_hole() {
    for source in [
        "func todo(): int { return 1 }\nfunc main(): int { return todo() }\n",
        "func main(): int {\n    let todo = 3\n    return todo\n}\n",
        "func main(todo: int): int { return todo }\n",
    ] {
        let found = holes(source);
        assert!(found.is_empty(), "{source}: {found:?}");
    }
}

#[test]
fn fills_respect_scope_order_and_moves() {
    let found = holes(
        r#"
        import std.alloc.string as strings
        func keep(name: strings.String): strings.String {
            if true {
                let inner = strings.String.from("a")
                return todo
            }
            let later = strings.String.from("b")
            return name
        }
        "#,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    let hole = &found[0];
    assert_eq!(fills(hole), vec!["inner", "name"]);
    assert!(hole
        .hints
        .iter()
        .any(|hint| hint.message == "fill with `inner` (moves `inner`)"));
}

#[test]
fn too_many_reasons_are_an_argument_error() {
    let found = holes("func main(): int { return todo(\"a\", \"b\") }\n");
    let codes: Vec<DiagCode> = found.iter().map(|d| d.code).collect();
    assert_eq!(
        codes,
        vec![DiagCode::T012WrongArgCount, DiagCode::T102TypedHole]
    );
}

#[test]
fn holes_are_reported_in_source_order() {
    let source = "func a(): int { return todo }\nfunc b(): bool { return todo }\n";
    let found = holes(source);
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        vec!["typed hole: expected 'int'", "typed hole: expected 'bool'"]
    );
}
