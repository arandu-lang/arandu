//! `p1 | p2`: every alternative binds the same names with the same types.
use arandu_query::db::DatabaseImpl;

fn errors(source: &str) -> Vec<String> {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("or.aru".to_string(), source.to_string());
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

const SHAPE: &str =
    "enum Shape {\n    Circle(int),\n    Square(int),\n    Flag(bool),\n    Empty,\n}\n";

#[test]
fn alternatives_share_their_bindings() {
    let source = format!(
        "{SHAPE}func size(s: Shape): int {{\n    return match s {{\n        Shape.Circle(n) | Shape.Square(n) if n > 1 => n,\n        Shape.Circle(n) | Shape.Square(n) => n + 1,\n        _ => 0,\n    }}\n}}\nfunc main(): int {{ return size(Shape.Empty) }}\n"
    );
    assert_eq!(errors(&source), Vec::<String>::new());
}

#[test]
fn a_name_missing_from_an_alternative_is_n022() {
    let source = format!(
        "{SHAPE}func size(s: Shape): int {{\n    return match s {{\n        Shape.Circle(n) | Shape.Empty => 1,\n        _ => 0,\n    }}\n}}\nfunc main(): int {{ return size(Shape.Empty) }}\n"
    );
    assert_eq!(errors(&source), ["N022"]);
    let extra = format!(
        "{SHAPE}func size(s: Shape): int {{\n    return match s {{\n        Shape.Empty | Shape.Circle(n) => 1,\n        _ => 0,\n    }}\n}}\nfunc main(): int {{ return size(Shape.Empty) }}\n"
    );
    assert_eq!(errors(&extra), ["N022"]);
}

#[test]
fn a_name_has_one_type_in_every_alternative() {
    let source = format!(
        "{SHAPE}func size(s: Shape): int {{\n    return match s {{\n        Shape.Circle(n) | Shape.Flag(n) => 1,\n        _ => 0,\n    }}\n}}\nfunc main(): int {{ return size(Shape.Empty) }}\n"
    );
    assert_eq!(errors(&source), ["T002"]);
}
