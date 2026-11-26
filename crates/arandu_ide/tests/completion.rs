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
    let fields: Vec<(&str, Option<&str>)> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::Field)
        .map(|item| (item.label.as_str(), item.detail.as_deref()))
        .collect();
    assert!(fields.contains(&("x", Some("int"))), "got {fields:?}");
    assert!(fields.contains(&("y", Some("int"))), "got {fields:?}");
}

#[test]
fn private_fields_complete_only_in_owner_methods() {
    let external = concat!(
        "struct Secret { private value: int, public visible: int }\n",
        "func leak(s: Secret): int { return s. }\n",
    );
    let external_items = completions_at(external, "return s.");
    let external_fields: Vec<&str> = external_items
        .iter()
        .filter(|item| item.kind == CompletionKind::Field)
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        external_fields.contains(&"visible"),
        "got {external_fields:?}"
    );
    assert!(
        !external_fields.contains(&"value"),
        "got {external_fields:?}"
    );

    let internal = concat!(
        "struct Secret { private value: int, public visible: int }\n",
        "public func Secret.read(self: ref Secret): int { return self. }\n",
    );
    let internal_items = completions_at(internal, "return self.");
    let internal_fields: Vec<&str> = internal_items
        .iter()
        .filter(|item| item.kind == CompletionKind::Field)
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        internal_fields.contains(&"value"),
        "got {internal_fields:?}"
    );
}

#[test]
fn enum_variants_complete_after_receiver_dot() {
    let text = concat!(
        "enum Color { Red, Green(int), Blue }\n",
        "func paint(c: Color): int {\n",
        "    return c.\n",
        "}\n",
    );
    let items = completions_at(text, "    return c.");
    let variants: Vec<(&str, Option<&str>)> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::EnumMember)
        .map(|item| (item.label.as_str(), item.detail.as_deref()))
        .collect();
    assert!(variants.contains(&("Red", None)), "got {variants:?}");
    assert!(
        variants.contains(&("Green", Some("int"))),
        "got {variants:?}"
    );
    assert!(variants.contains(&("Blue", None)), "got {variants:?}");
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
fn interface_methods_show_name_and_parameters_in_detail() {
    let text = concat!(
        "interface Drawable {\n",
        "    func draw(scale: int): int\n",
        "}\n",
        "func main(): int { return Drawable. }\n",
    );
    let items = completions_at(text, "return Drawable.");
    let draw = items
        .iter()
        .find(|item| item.kind == CompletionKind::Method && item.label == "draw")
        .expect("draw completion");
    assert_eq!(draw.detail.as_deref(), Some("func draw(int): int"));
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

#[test]
fn str_methods_complete_after_receiver_dot() {
    let text = concat!(
        "import std.core.str as s\n",
        "func main(): int {\n",
        "    let text = \"hello\"\n",
        "    return text.\n",
        "}\n",
    );
    let mut host = AnalysisHost::new();
    let _str_file = host.new_file(
        "stdlib/core/str.aru".into(),
        include_str!("../../../stdlib/core/str.aru").to_string(),
    );
    let file = host.new_file("main.aru".into(), text.to_string());
    let snap = host.snapshot();
    let start = text.find("    return text.").expect("cursor marker");
    let offset = u32::try_from(start + "    return text.".len()).expect("offset");
    let items = completions(&snap, file, text, offset);
    let methods: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::Method)
        .map(|item| item.label.as_str())
        .collect();
    assert!(methods.contains(&"len"), "got {methods:?}");
    assert!(methods.contains(&"isEmpty"), "got {methods:?}");
    assert!(methods.contains(&"startsWith"), "got {methods:?}");
}

#[test]
fn primitive_methods_complete_without_import() {
    let text = concat!(
        "func main(): int {\n",
        "    let text = \"hello\"\n",
        "    return text.\n",
        "}\n",
    );
    let mut host = AnalysisHost::new();
    let _str_file = host.new_file(
        "stdlib/core/str.aru".into(),
        include_str!("../../../stdlib/core/str.aru").to_string(),
    );
    let file = host.new_file("main.aru".into(), text.to_string());
    let snap = host.snapshot();
    let start = text.find("    return text.").expect("cursor marker");
    let offset = u32::try_from(start + "    return text.".len()).expect("offset");
    let items = completions(&snap, file, text, offset);
    let methods: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::Method)
        .map(|item| item.label.as_str())
        .collect();
    assert!(methods.contains(&"len"), "got {methods:?}");
    assert!(methods.contains(&"isEmpty"), "got {methods:?}");
    assert!(methods.contains(&"startsWith"), "got {methods:?}");
}

#[test]
fn int_methods_complete_without_import() {
    let text = concat!(
        "func main(): int {\n",
        "    let x = 42\n",
        "    return x.\n",
        "}\n",
    );
    let mut host = AnalysisHost::new();
    let _num_file = host.new_file(
        "stdlib/core/num.aru".into(),
        include_str!("../../../stdlib/core/num.aru").to_string(),
    );
    let file = host.new_file("main.aru".into(), text.to_string());
    let snap = host.snapshot();
    let start = text.find("    return x.").expect("cursor marker");
    let offset = u32::try_from(start + "    return x.".len()).expect("offset");
    let items = completions(&snap, file, text, offset);
    let methods: Vec<&str> = items
        .iter()
        .filter(|item| item.kind == CompletionKind::Method)
        .map(|item| item.label.as_str())
        .collect();
    assert!(methods.contains(&"abs"), "got {methods:?}");
    assert!(methods.contains(&"checkedAdd"), "got {methods:?}");
}

#[test]
fn unsigned_primitive_bit_methods_complete_from_the_stdlib() {
    let mut host = AnalysisHost::new();
    for (path, text) in [
        (
            "stdlib/core/intrinsics.aru",
            include_str!("../../../stdlib/core/intrinsics.aru"),
        ),
        (
            "stdlib/core/option.aru",
            include_str!("../../../stdlib/core/option.aru"),
        ),
        (
            "stdlib/core/num.aru",
            include_str!("../../../stdlib/core/num.aru"),
        ),
    ] {
        host.new_file(path.into(), text.into());
    }
    for ty in ["u32", "u64"] {
        let text = format!("func main(): int {{ let value: {ty} = 0; value. }}");
        let file = host.new_file(format!("bits_{ty}.aru"), text.clone());
        let offset = u32::try_from(text.find("value. ").unwrap() + "value.".len()).unwrap();
        let snapshot = host.snapshot();
        let items = completions(&snapshot, file, &text, offset);
        for method in ["countOnes", "leadingZeros", "trailingZeros", "reverseBits"] {
            assert!(
                labels(&items).contains(&method),
                "{ty} missing {method}: {:?}",
                labels(&items)
            );
        }
        drop(snapshot);
    }
}

#[test]
fn decimal_methods_complete_from_the_stdlib() {
    let mut host = AnalysisHost::new();
    for (path, text) in [
        (
            "stdlib/core/intrinsics.aru",
            include_str!("../../../stdlib/core/intrinsics.aru"),
        ),
        (
            "stdlib/core/slice.aru",
            include_str!("../../../stdlib/core/slice.aru"),
        ),
        (
            "stdlib/core/decimal.aru",
            include_str!("../../../stdlib/core/decimal.aru"),
        ),
    ] {
        host.new_file(path.into(), text.into());
    }
    let text = "import std.core.decimal as decimal\nfunc main(): int { let d = decimal.Decimal.zero(); d. }";
    let file = host.new_file("decimal_completion.aru".into(), text.to_string());
    let offset = u32::try_from(text.find("d. ").unwrap() + "d.".len()).unwrap();
    let snapshot = host.snapshot();
    let items = completions(&snapshot, file, text, offset);
    let item_labels = labels(&items);
    for method in ["round", "add", "sub", "mul", "div"] {
        assert!(
            item_labels.contains(&method),
            "Decimal missing {method}: {:?}",
            item_labels
        );
    }
}

#[test]
fn completions_surface_inherent_methods_on_primitive_types() {
    let mut host = AnalysisHost::default();
    let text = "public func int.double(self: int): int { return self * 2 }\nfunc main(): int {\n    let x: int = 10\n    return x.\n}\n";
    let file = host.new_file("primitive_completion.aru".into(), text.to_string());
    let offset = u32::try_from(text.find("return x.").unwrap() + "return x.".len()).unwrap();
    let snapshot = host.snapshot();
    let items = completions(&snapshot, file, text, offset);
    let item_labels = labels(&items);
    assert!(
        item_labels.contains(&"double"),
        "primitive int missing 'double': {:?}",
        item_labels
    );
}

#[test]
fn guard_keyword_is_suggested() {
    let text = "func main(): void {\n    gu\n}\n";
    let items = completions_at(text, "    gu");
    let item_labels = labels(&items);
    assert!(
        item_labels.contains(&"guard"),
        "missing 'guard': {item_labels:?}"
    );
}

#[test]
fn call_arguments_are_suggested_with_labels() {
    let text = concat!(
        "func connect(host: str, port: int): bool {\n",
        "    return true\n",
        "}\n",
        "func main(): void {\n",
        "    connect(\n",
        "}\n",
    );
    let items = completions_at(text, "connect(");
    let item_labels = labels(&items);
    assert!(
        item_labels.contains(&"host:"),
        "missing 'host:': {item_labels:?}"
    );
    assert!(
        item_labels.contains(&"port:"),
        "missing 'port:': {item_labels:?}"
    );
}

#[test]
fn auto_import_suggests_std_collections_hashmap_with_text_edit() {
    let text = "module app.demo\n\nfunc main(): void {\n    let m: HashM\n}\n";
    let items = completions_at(text, "HashM");
    let hm = items
        .iter()
        .find(|item| item.label == "HashMap")
        .expect("expected auto-import suggestion for HashMap");
    assert_eq!(hm.additional_text_edits.len(), 1);
    assert!(
        hm.additional_text_edits[0]
            .new_text
            .contains("from std.collections import { HashMap }"),
        "unexpected edit: {:?}",
        hm.additional_text_edits[0]
    );
}

#[test]
fn auto_import_skips_already_imported_symbols() {
    let text = "module app.demo\n\nimport std.fs\n\nfunc main(): void {\n    fs\n}\n";
    let items = completions_at(text, "    fs");
    let fs_items: Vec<_> = items.iter().filter(|item| item.label == "fs").collect();
    assert!(
        fs_items
            .iter()
            .all(|item| item.additional_text_edits.is_empty()),
        "already-imported `fs` must not carry auto-import edits: {fs_items:?}"
    );
}

#[test]
fn auto_import_suggests_workspace_symbols_from_other_files() {
    let mut host = AnalysisHost::default();
    let _user_file = host.new_file(
        "models/user.aru".into(),
        "module models.user\n\npublic struct Account {\n    public id: int\n}\n\npublic func makeAccount(): Account {\n    return Account { id: 1 }\n}\n"
            .to_string(),
    );
    let main_text = "module app.main\n\nfunc main(): void {\n    let a: Acco\n}\n";
    let main_file = host.new_file("app/main.aru".into(), main_text.to_string());
    let offset = u32::try_from(main_text.find("Acco").unwrap() + "Acco".len()).unwrap();
    let snap = host.snapshot();

    let items = completions(&snap, main_file, main_text, offset);
    let acc = items
        .iter()
        .find(|item| item.label == "Account")
        .expect("expected workspace auto-import for Account");
    assert_eq!(acc.additional_text_edits.len(), 1);
    assert!(
        acc.additional_text_edits[0]
            .new_text
            .contains("from models.user import { Account }"),
        "unexpected edit: {:?}",
        acc.additional_text_edits[0]
    );
}

#[test]
fn auto_import_respects_package_internal_visibility() {
    use arandu_middle::{ModuleId, PackageId, TargetId};
    use arandu_query::vfs::{ModuleBinding, PackageModuleMap};
    use std::sync::Arc;

    let mut host = AnalysisHost::default();
    let same_pkg_file = host.new_file(
        "src/auth.aru".into(),
        "module self.auth\n\ninternal func verifyToken(): bool { return true }\n".to_string(),
    );
    let dep_file = host.new_file(
        "deps/web/router.aru".into(),
        "module web.router\n\npublic struct Router {}\ninternal func secretInternal(): void {}\n"
            .to_string(),
    );
    let main_text = "module self.main\n\nfunc main(): void {\n    verify\n}\n";
    let main_file = host.new_file("src/main.aru".into(), main_text.to_string());

    let pkg0 = PackageId::try_from_usize(0).unwrap();
    let pkg1 = PackageId::try_from_usize(1).unwrap();
    let tgt0 = TargetId::try_from_usize(0).unwrap();
    let tgt1 = TargetId::try_from_usize(1).unwrap();
    let mut bindings = vec![
        (
            "self/main.aru".to_string(),
            ModuleBinding {
                package: pkg0,
                target: tgt0,
                module: ModuleId::try_from_usize(0).unwrap(),
                file: main_file,
            },
        ),
        (
            "self/auth.aru".to_string(),
            ModuleBinding {
                package: pkg0,
                target: tgt0,
                module: ModuleId::try_from_usize(1).unwrap(),
                file: same_pkg_file,
            },
        ),
        (
            "web/router.aru".to_string(),
            ModuleBinding {
                package: pkg1,
                target: tgt1,
                module: ModuleId::try_from_usize(2).unwrap(),
                file: dep_file,
            },
        ),
    ];
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    let map = PackageModuleMap::new(host.db(), pkg0, tgt0, Arc::new(bindings));
    host.db_mut().set_package_module_map(map);
    host.bump_revision();

    let snap = host.snapshot();
    // Same-package `internal` function IS suggested.
    let offset = u32::try_from(main_text.find("verify").unwrap() + "verify".len()).unwrap();
    let items = completions(&snap, main_file, main_text, offset);
    let vt = items
        .iter()
        .find(|item| item.label == "verifyToken")
        .expect("expected same-package internal auto-import for verifyToken");
    assert!(
        vt.additional_text_edits[0]
            .new_text
            .contains("from self.auth import { verifyToken }"),
        "unexpected edit: {:?}",
        vt.additional_text_edits[0]
    );

    // Dependency `public` type IS suggested, but dependency `internal` function is NOT.
    let router_candidates =
        arandu_ide::auto_import_candidates_for_name(&snap, main_file, main_text, "Router");
    assert_eq!(router_candidates.len(), 1);
    assert_eq!(
        router_candidates[0].import_statement,
        "from web.router import { Router }"
    );
    let secret_candidates =
        arandu_ide::auto_import_candidates_for_name(&snap, main_file, main_text, "secretInternal");
    assert!(
        secret_candidates.is_empty(),
        "dependency internal symbol must never be suggested: {secret_candidates:?}"
    );
}
