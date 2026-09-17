//! Behavioral tests for the shared completion engine.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use arandu_ide::{CompletionKind, completions};
use arandu_query::AnalysisHost;

fn completions_at(text: &str, cursor_marker: &str) -> Vec<arandu_ide::CompletionItem> {
    let mut host = AnalysisHost::new();
    let file = host.new_file("completion.aru".into(), text.to_string());
    let snap = host.snapshot();
    let start = text.find(cursor_marker).expect("cursor marker");
    let offset = u32::try_from(start + cursor_marker.len()).expect("offset");
    completions(&snap, file, text, offset)
}

fn labels(items: &[arandu_ide::CompletionItem]) -> Vec<&str> {
    items.iter().map(|item| item.label.as_str()).collect()
}

#[test]
fn struct_fields_complete_after_receiver_dot() {
    let text = concat!(
        "struct Point { x: int, y: int }\n",
        "func main(): int {\n",
        "    let p: Point = Point { x: 1, y: 2 }\n",
        "    return p.\n",
        "}\n",
    );
    let items = completions_at(text, "    return p.");
    let fields: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::Field)
        .map(|item| item.label.as_str())
        .collect();
    assert!(fields.contains(&"x"), "got {fields:?}");
    assert!(fields.contains(&"y"), "got {fields:?}");
}

#[test]
fn enum_variants_complete_after_receiver_dot() {
    let text = concat!(
        "enum Color { Red, Green, Blue }\n",
        "func paint(c: Color): int {\n",
        "    return c.\n",
        "}\n",
    );
    let items = completions_at(text, "    return c.");
    let variants: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::EnumMember)
        .map(|item| item.label.as_str())
        .collect();
    assert!(variants.contains(&"Red"), "got {variants:?}");
    assert!(variants.contains(&"Green"), "got {variants:?}");
    assert!(variants.contains(&"Blue"), "got {variants:?}");
}

#[test]
fn associated_methods_complete_after_receiver_dot() {
    let text = concat!(
        "struct Counter { value: int }\n",
        "public func Counter.get(self: ref Counter): int { return self.value }\n",
        "func main(): int {\n",
        "    let c: Counter = Counter { value: 0 }\n",
        "    return c.\n",
        "}\n",
    );
    let items = completions_at(text, "    return c.");
    let methods: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::Method)
        .map(|item| item.label.as_str())
        .collect();
    assert!(methods.contains(&"get"), "got {methods:?}");
}

#[test]
fn locals_declared_after_the_cursor_are_not_offered() {
    let text = "func main(): int {\n    let value = 1\n    return 0\n}\n";
    let before_declaration = u32::try_from(text.find("let value").expect("decl")).expect("offset");
    let mut host = AnalysisHost::new();
    let file = host.new_file("scope.aru".into(), text.to_string());
    let snap = host.snapshot();
    let items = completions(&snap, file, text, before_declaration);
    assert!(
        !labels(&items).contains(&"value"),
        "undeclared local leaked into completions: {:?}",
        labels(&items)
    );
}

#[test]
fn type_position_offers_types_not_functions() {
    let text = concat!(
        "struct Widget { id: int }\n",
        "func build(): int { return 0 }\n",
        "func main(): int {\n",
        "    let w: \n",
        "    return 0\n",
        "}\n",
    );
    let items = completions_at(text, "    let w: ");
    let names = labels(&items);
    assert!(names.contains(&"Widget"), "expected user type: {names:?}");
    assert!(names.contains(&"int"), "expected primitive type: {names:?}");
    assert!(
        !names.contains(&"build"),
        "functions must not be offered in type position: {names:?}"
    );
}

#[test]
fn keywords_and_locals_are_ranked_before_types() {
    let text = concat!(
        "struct Thing { id: int }\n",
        "func main(): int {\n",
        "    let local = 1\n",
        "    return local\n",
        "}\n",
    );
    let items = completions_at(text, "    return ");
    let sort_of = |label: &str| {
        items
            .iter()
            .find(|item| item.label == label)
            .and_then(|item| item.sort_text.clone())
            .expect("item")
    };
    assert!(
        sort_of("local") < sort_of("Thing"),
        "locals should outrank types"
    );
    assert!(
        sort_of("local") < sort_of("func"),
        "locals should outrank keywords"
    );
}
