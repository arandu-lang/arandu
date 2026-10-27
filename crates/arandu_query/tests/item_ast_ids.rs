#![allow(clippy::unwrap_used, clippy::expect_used)]
//! RFC 0042 phase 1: `AstId` names a top-level item independently of where it
//! sits, and `item_positions` is the only map from that identity to bytes.
//! Phase 2: `item_ast` is the item's AST with positions relative to it.

use arandu_base::AstId;
use arandu_query::passes::{item_ast, item_ast_ids, item_positions, parse, type_check};
use arandu_query::{DatabaseImpl, SourceFile, StableHash};
use salsa::Setter;
use std::collections::BTreeMap;
use std::sync::Arc;

fn program(growth: usize) -> String {
    let mut alpha = String::from(
        "/// Grows.\n@Effects(none)\nfunc alpha(value: int): int {\n    let mut acc = value\n",
    );
    for step in 1..=growth {
        alpha.push_str(&format!("    acc = acc + {step}\n"));
    }
    alpha.push_str("    return acc\n}\n");
    format!(
        "{alpha}\nstruct Point {{\n    x: int\n}}\n\nimpl Point {{\n    func get(self: ref): int {{\n        return self.x\n    }}\n\n    func twice(self: ref): int {{\n        return self.x * 2\n    }}\n}}\n\nextern \"C\" {{\n    func ar_rt_wake(id: int): void\n}}\n\n/// Entry.\nfunc main(): int {{\n    return alpha(1)\n}}\n"
    )
}

fn ids_by_name(db: &DatabaseImpl, file: SourceFile) -> BTreeMap<String, AstId> {
    let symbols = type_check(db, file).symbols.clone();
    item_ast_ids(db, file)
        .iter()
        .map(|(symbol, id)| (symbols.get(*symbol).name.to_string(), *id))
        .collect()
}

#[test]
fn every_definition_maps_to_its_enclosing_item() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("items.aru".into(), program(0));
    let file_id = *file.file_id(&db);
    let ids = ids_by_name(&db, file);
    let expected = [
        ("alpha", 0),
        ("Point", 1),
        ("Point.get", 2),
        ("Point.twice", 2),
        ("ar_rt_wake", 3),
        ("main", 4),
    ];
    for (name, index) in expected {
        assert_eq!(
            ids.get(name),
            Some(&AstId::new(file_id, index)),
            "`{name}` in {ids:?}"
        );
    }
    assert_eq!(item_positions(&db, file).len(), 5);
}

#[test]
fn item_positions_cover_attributes() {
    let mut db = DatabaseImpl::new();
    let source = program(0);
    let file = db.new_file("items.aru".into(), source.clone());
    let positions = item_positions(&db, file);
    let parsed = parse(&db, file);
    let parsed = parsed.as_ref();
    let program = parsed.as_ref().expect("parse");
    for decl in &program.decls {
        let span = arandu_semantics::item_source_span(program.pool.decl(*decl));
        let id = positions.ast_id_containing(span).expect("enclosing item");
        let item = positions.item_span(id).expect("item span");
        assert!(item.start <= span.start && span.end <= item.end);
    }
    let first = positions
        .item_span(positions.ids().next().expect("item"))
        .expect("span");
    // A doc comment that opens the file is root trivia, outside the first
    // item; the item still covers its attributes.
    let text = &source[first.start as usize..first.end as usize];
    assert!(text.trim_start().starts_with("@Effects(none)"), "{text:?}");
    assert!(!text.contains("/// Grows."), "{text:?}");
}

#[test]
fn growing_a_body_moves_positions_but_keeps_ids() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("items.aru".into(), program(0));
    let before_ids = ids_by_name(&db, file);
    let before = Arc::clone(item_positions(&db, file));

    file.set_text(&mut db).to(Arc::from(program(3)));
    let after = item_positions(&db, file);
    assert_eq!(before_ids, ids_by_name(&db, file));
    assert_ne!(*before, **after);

    let ids: Vec<_> = after.ids().collect();
    assert_eq!(ids, before.ids().collect::<Vec<_>>());
    let shift = after.item_span(ids[1]).unwrap().start - before.item_span(ids[1]).unwrap().start;
    assert!(shift > 0);
    for id in &ids[1..] {
        let (old, new) = (
            before.item_span(*id).unwrap(),
            after.item_span(*id).unwrap(),
        );
        assert_eq!((new.start - old.start, new.end - old.end), (shift, shift));
        // A relative position resolves against the item's current start.
        let relative = arandu_base::Span::new(old.file_id, 0, old.end - old.start);
        assert_eq!(after.to_absolute(*id, relative), Some(new));
    }
}

fn item_ast_hashes(db: &DatabaseImpl, file: SourceFile) -> Vec<blake3::Hash> {
    item_positions(db, file)
        .ids()
        .map(|id| item_ast(db, file, id).stable_hash())
        .collect()
}

#[test]
fn a_moved_item_keeps_its_local_ast() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("items.aru".into(), program(0));
    let before = item_ast_hashes(&db, file);

    file.set_text(&mut db).to(Arc::from(program(3)));
    let after = item_ast_hashes(&db, file);
    assert_ne!(before[0], after[0], "the edited item changes");
    assert_eq!(before[1..], after[1..], "the moved items do not");
}

#[test]
fn local_positions_resolve_to_the_file_ast() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("items.aru".into(), program(3));
    let positions = item_positions(&db, file);
    let parsed = parse(&db, file);
    let parsed = parsed.as_ref();
    let whole = parsed.as_ref().expect("parse");
    let mut file_spans = whole.pool.decl_spans.clone();
    file_spans.extend(whole.pool.expr_spans.iter().copied());
    file_spans.sort();

    for id in positions.ids() {
        let local = item_ast(&db, file, id);
        let local = local.value.as_ref().as_ref().expect("item parse");
        let len = {
            let item = positions.item_span(id).expect("item");
            item.end - item.start
        };
        for span in local.pool.decl_spans.iter().chain(&local.pool.expr_spans) {
            assert!(span.end <= len, "{span:?} outside item {id:?}");
            let absolute = positions.to_absolute(id, *span).expect("absolute");
            assert!(
                file_spans.binary_search(&absolute).is_ok(),
                "{absolute:?} of {id:?}"
            );
        }
    }
}
