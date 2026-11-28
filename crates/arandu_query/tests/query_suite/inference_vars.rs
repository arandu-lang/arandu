//! Inference variables (TYP.NEXT S1): a generic parameter nothing fixes at
//! its call is a variable that later uses of the value bind, as in rustc.
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
    let file = db.new_file("infer.aru".to_string(), source.to_string());
    (db, file)
}

fn errors(source: &str) -> Vec<DiagCode> {
    let (db, file) = database(source);
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

fn diagnostics(source: &str) -> Vec<(DiagCode, String, u32)> {
    let (db, file) = database(source);
    type_check(&db, file)
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| {
            (
                diagnostic.code,
                diagnostic.message.clone(),
                diagnostic.span.start,
            )
        })
        .collect()
}

#[test]
fn a_later_use_binds_the_element_type() {
    let codes = errors(
        r#"
            import std.alloc.vec as vec
            func main(): int {
                let mut v = vec.Vec.new()
                v.push(1)
                v.push(2)
                return v.len() as int
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_variable_takes_the_width_its_literal_meets_later() {
    let codes = errors(
        r#"
            import std.alloc.vec as vec
            func total(items: ref vec.Vec<i64>): i64 { return 0 }
            func main(): int {
                let mut v = vec.Vec.new()
                v.push(1)
                let sum = total(ref v)
                return sum as int
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn an_assignment_binds_a_generic_result() {
    let codes = errors(
        r#"
            func pick<T>(): Option<T> { return Option.None }
            func main(): int {
                let mut x = pick()
                x = Option.Some(3)
                return 0
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_map_learns_key_and_value_from_put() {
    let codes = errors(
        r#"
            import std.alloc.hash_map as maps
            import std.core.hash as hash
            struct Key { id: int }
            public func Key.eq(self: ref Key, other: ref Key): bool { return self.id == other.id }
            public func Key.hash<H: hash.Hasher>(self: ref Key, state: mut ref H): void {}
            func main(): int {
                let mut m = maps.HashMap.new()
                m.put(Key { id: 1 }, true)
                return 0
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn an_unused_variable_is_one_error_at_its_call() {
    let source = r#"
            import std.alloc.vec as vec
            func main(): int {
                let v = vec.Vec.new()
                return 0
            }
        "#;
    let found = diagnostics(source);
    assert_eq!(found.len(), 1, "{found:?}");
    let (code, message, start) = &found[0];
    assert_eq!(*code, DiagCode::T001CannotInferType);
    assert!(message.contains("`T`"), "{message}");
    assert_eq!(*start as usize, source.find("vec.Vec.new()").expect("call"));
}

#[test]
fn unresolved_variables_are_reported_in_creation_order() {
    let source = r#"
            import std.alloc.vec as vec
            func main(): int {
                let a = vec.Vec.new()
                let b = vec.Vec.new()
                return 0
            }
        "#;
    let found = diagnostics(source);
    let starts: Vec<u32> = found.iter().map(|(_, _, start)| *start).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(starts, sorted);
}

#[test]
fn a_conflicting_use_is_a_mismatch_not_an_inference_error() {
    let codes = errors(
        r#"
            import std.alloc.vec as vec
            func main(): int {
                let mut v = vec.Vec.new()
                v.push(1)
                v.push(true)
                return 0
            }
        "#,
    );
    assert!(!codes.is_empty());
    assert!(!codes.contains(&DiagCode::T001CannotInferType), "{codes:?}");
}

#[test]
fn bounds_are_checked_once_the_variable_is_bound() {
    let codes = errors(
        r#"
            import std.alloc.hash_map as maps
            struct Key { id: int }
            func main(): int {
                let mut m = maps.HashMap.new()
                m.put(Key { id: 1 }, 2)
                return 0
            }
        "#,
    );
    assert!(
        codes.contains(&DiagCode::T025InterfaceNotSatisfied),
        "{codes:?}"
    );
    assert!(!codes.contains(&DiagCode::T001CannotInferType), "{codes:?}");
}

#[test]
fn an_operator_settles_a_numeric_variable() {
    let codes = errors(
        r#"
            func identity<T>(value: T): T { return value }
            func nothing<T>(): Option<T> { return Option.None }
            func main(): int {
                let mut value = nothing()
                value = Option.Some(identity(9))
                match value {
                    Option.Some(nine) => { return nine - 9 }
                    Option.None => { return 1 }
                }
            }
        "#,
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn inference_diagnostics_are_deterministic() {
    let source = r#"
            import std.alloc.vec as vec
            func main(): int {
                let a = vec.Vec.new()
                let mut b = vec.Vec.new()
                b.push(true)
                let c = vec.Vec.new()
                return 0
            }
        "#;
    let first = diagnostics(source);
    for _ in 0..4 {
        assert_eq!(diagnostics(source), first);
    }
    assert_eq!(first.len(), 2, "{first:?}");
}
