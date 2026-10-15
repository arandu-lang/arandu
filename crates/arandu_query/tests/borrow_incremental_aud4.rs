#![allow(clippy::expect_used, clippy::unwrap_used)]
//! AUD.4 — borrowed-return summaries across Salsa and IDE diagnostics.

use std::sync::Arc;

use arandu_middle::DiagCode;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{borrow_interfaces, lower_amir, module_signatures, type_check};
use arandu_query::{file_ide_diagnostics, RebuildEvent, SourceFile};
use salsa::Setter;

fn codes(db: &DatabaseImpl, file: SourceFile) -> Vec<String> {
    file_ide_diagnostics(db, file)
        .iter()
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

fn execute_count(events: &[RebuildEvent], query: &str) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, RebuildEvent::Execute { key } if key.contains(query)))
        .count()
}

fn library_source(extra_body: &str, second_parameter: &str) -> String {
    format!(
        r#"module loans

public func borrowValue(value: ref int, marker: {second_parameter}): ref int {{
    {extra_body}
    return value
}}
"#
    )
}

const CALLER_SOURCE: &str = r#"module caller
import loans

func main(): int {
    let value = 42
    let marker = 0
    let borrowed = loans.borrowValue(value, marker)
    return *borrowed
}
"#;

#[test]
fn dependency_body_edit_preserves_summary_and_cuts_off_caller_body() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let dependency = db.new_file("loans.aru".into(), library_source("", "int"));
    let caller = db.new_file("caller.aru".into(), CALLER_SOURCE.into());

    let initial = type_check(&db, caller);
    assert!(initial.diagnostics.is_empty(), "{:?}", initial.diagnostics);
    let dependency_signatures = module_signatures(&db, dependency);
    assert_eq!(
        dependency_signatures
            .type_info
            .return_borrow_summaries
            .len(),
        1
    );

    log.clear();
    dependency.set_text(&mut db).to(Arc::from(library_source(
        "let bodyOnly = marker + 1",
        "int",
    )));

    let after = type_check(&db, caller);
    assert!(after.diagnostics.is_empty(), "{:?}", after.diagnostics);
    assert_eq!(after.type_info.return_borrow_summaries.len(), 1);
    let events = log.snapshot();
    assert_eq!(
        execute_count(&events, "item_typing"),
        1,
        "only the edited dependency body may re-execute when its public summary is unchanged:\n{}",
        log.format_chain(true)
    );
    assert_eq!(execute_count(&events, "item_body_typeck"), 0);
    assert_eq!(execute_count(&events, "lower_amir"), 0);
}

#[test]
fn summary_change_invalidates_caller_and_removes_imported_dependency() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let dependency = db.new_file("loans.aru".into(), library_source("", "int"));
    let caller = db.new_file("caller.aru".into(), CALLER_SOURCE.into());

    let initial = type_check(&db, caller);
    assert!(initial.diagnostics.is_empty(), "{:?}", initial.diagnostics);
    assert_eq!(initial.type_info.return_borrow_summaries.len(), 1);

    log.clear();
    dependency.set_text(&mut db).to(Arc::from(
        r#"module loans

public func borrowValue(value: ref int, marker: ref int): ref int {
    return marker
}
"#,
    ));

    let changed_dependency = borrow_interfaces(&db, dependency);
    let changed_summary = changed_dependency
        .entries
        .iter()
        .map(|(_, summary)| summary)
        .next()
        .expect("flow-derived imported summary");
    assert_eq!(
        changed_summary.dependencies[0].sources[0].parameter_index, 1,
        "the body, not signature ambiguity, chooses the exported origin"
    );
    let changed_caller = type_check(&db, caller);
    assert_eq!(changed_caller.type_info.return_borrow_summaries.len(), 1);
    assert!(
        execute_count(&log.snapshot(), "item_typing") >= 2,
        "dependency and caller typing must re-execute after the imported declaration changes:\n{}",
        log.format_chain(true)
    );
    assert_eq!(execute_count(&log.snapshot(), "item_body_typeck"), 1);
    assert_eq!(execute_count(&log.snapshot(), "lower_amir"), 0);
}

fn mutation_source(with_conflict: bool) -> String {
    let mutation = if with_conflict { "value = 43" } else { "" };
    format!(
        r#"func borrowValue(value: ref int): ref int {{
    return value
}}

func main(): int {{
    let mut value = 42
    let borrowed = borrowValue(value)
    {mutation}
    return *borrowed
}}
"#
    )
}

#[test]
fn ide_o003_is_born_and_removed_after_cached_edits() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("mutation.aru".into(), mutation_source(false));
    let checked = type_check(&db, file);
    assert_eq!(
        checked.type_info.return_borrow_summaries.len(),
        1,
        "the borrowed parameter return must publish exactly one contract"
    );
    let lowered = lower_amir(&db, file);
    assert!(
        lowered.amir.funcs.iter().any(|func| checked
            .type_info
            .return_borrow_summaries
            .contains_key(&func.symbol)),
        "lowered function identities must match the exported borrow contracts: summaries={:?}, funcs={:?}",
        checked.type_info.return_borrow_summaries,
        lowered.amir.funcs.iter().map(|func| func.symbol).collect::<Vec<_>>()
    );
    let baseline = codes(&db, file);
    assert!(!baseline.iter().any(|code| code == "O003"));
    assert!(
        !baseline.iter().any(|code| code == "O004" || code == "O010"),
        "a declared parameter passthrough is not a local escape: {baseline:?}"
    );

    file.set_text(&mut db).to(Arc::from(mutation_source(true)));
    let conflicting = file_ide_diagnostics(&db, file);
    let diagnostic = conflicting
        .iter()
        .find(|diagnostic| diagnostic.code == "O003")
        .expect("mutation under returned borrow must surface O003");
    assert!(!diagnostic.labels.is_empty());
    assert!(!diagnostic.notes.is_empty());

    file.set_text(&mut db).to(Arc::from(mutation_source(false)));
    assert!(!codes(&db, file)
        .iter()
        .any(|code| code == "O002" || code == "O003"));
}

fn local_escape_source(escape: bool) -> String {
    if escape {
        r#"func leak(): ref int {
    let value = 42
    return ref value
}
"#
        .into()
    } else {
        r#"func main(): int {
    let value = 42
    let borrowed = ref value
    return *borrowed
}
"#
        .into()
    }
}

#[test]
fn ide_o010_is_born_and_removed_after_cached_edits() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("escape.aru".into(), local_escape_source(false));
    assert!(!codes(&db, file).iter().any(|code| code == "O010"));

    file.set_text(&mut db)
        .to(Arc::from(local_escape_source(true)));
    let escaped = file_ide_diagnostics(&db, file);
    let diagnostic = escaped
        .iter()
        .find(|diagnostic| diagnostic.code == "O010")
        .expect("returning a local borrow must surface O010");
    assert!(!diagnostic.labels.is_empty());
    assert!(!diagnostic.notes.is_empty());

    file.set_text(&mut db)
        .to(Arc::from(local_escape_source(false)));
    assert!(!codes(&db, file).iter().any(|code| code == "O010"));
}

fn move_source(with_live_borrow: bool) -> String {
    let borrow = if with_live_borrow {
        "let borrowed = ref owner"
    } else {
        ""
    };
    let use_borrow = if with_live_borrow {
        "return result + observe(borrowed)"
    } else {
        "return result"
    };
    format!(
        r#"struct Resource {{ handle: ptr[u8] }}

func consume(value: Resource): int {{
    return 42
}}

func observe(value: ref Resource): int {{
    return 1
}}

func main(): int {{
    let owner = Resource {{ handle: nil }}
    {borrow}
    let result = consume(owner)
    {use_borrow}
}}
"#
    )
}

#[test]
fn ide_o002_is_born_and_removed_after_cached_edits() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("move.aru".into(), move_source(false));
    assert!(!codes(&db, file).iter().any(|code| code == "O002"));

    file.set_text(&mut db).to(Arc::from(move_source(true)));
    let moved = file_ide_diagnostics(&db, file);
    let diagnostic = moved
        .iter()
        .find(|diagnostic| diagnostic.code == "O002")
        .expect("moving an owner under a live borrow must surface O002");
    assert!(!diagnostic.labels.is_empty());
    assert!(!diagnostic.notes.is_empty());

    file.set_text(&mut db).to(Arc::from(move_source(false)));
    assert!(!codes(&db, file).iter().any(|code| code == "O002"));
}

fn destroy_source(with_escape: bool) -> String {
    let result = if with_escape { "ref Resource" } else { "int" };
    let body = if with_escape {
        "return ref resource"
    } else {
        "return resource.value"
    };
    format!(
        r#"struct Resource {{ value: int }}

@Destructor
func Resource.close(own self): void {{}}

func inspect(): {result} {{
    let resource = Resource {{ value: 42 }}
    {body}
}}
"#
    )
}

#[test]
fn ide_o006_is_born_and_removed_after_cached_edits() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("destroy.aru".into(), destroy_source(false));
    assert!(!codes(&db, file).iter().any(|code| code == "O006"));

    file.set_text(&mut db).to(Arc::from(destroy_source(true)));
    let destroyed = file_ide_diagnostics(&db, file);
    let diagnostic = destroyed
        .iter()
        .find(|diagnostic| diagnostic.code == "O006")
        .expect("destroying an owner while returning its borrow must surface O006");
    assert!(!diagnostic.labels.is_empty());
    assert!(!diagnostic.notes.is_empty());

    file.set_text(&mut db).to(Arc::from(destroy_source(false)));
    assert!(!codes(&db, file).iter().any(|code| code == "O006"));
}

#[test]
fn diagnostic_code_enum_still_owns_aud4_contracts() {
    assert_eq!(DiagCode::O002MoveWhileBorrowed.as_str(), "O002");
    assert_eq!(DiagCode::O003MutableBorrowConflict.as_str(), "O003");
    assert_eq!(DiagCode::O006DestroyWhileBorrowed.as_str(), "O006");
    assert_eq!(DiagCode::O010EscapeOfBorrowedValue.as_str(), "O010");
}

fn slice_view_source(with_conflict: bool) -> String {
    let mutation = if with_conflict { "owner.value = 2" } else { "" };
    format!(
        r#"module std.core.slice_test

struct Owner {{ value: int }}

extern "arandu-intrinsic" {{
    func makeRawView(owner: ref Owner): []int
}}

func makeView(owner: ref Owner): []int {{
    unsafe {{
        return makeRawView(owner)
    }}
}}

func observe(values: []int): int {{
    return 0
}}

func main(): int {{
    let mut owner = Owner {{ value: 1 }}
    let view = makeView(owner)
    {mutation}
    return observe(view)
}}
"#
    )
}

#[test]
fn ide_slice_view_conflict_is_born_and_removed_incrementally() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("slice_view.aru".into(), slice_view_source(false));
    assert!(!codes(&db, file).iter().any(|code| code == "O003"));

    file.set_text(&mut db)
        .to(Arc::from(slice_view_source(true)));
    let conflicting = file_ide_diagnostics(&db, file);
    let diagnostic = conflicting
        .iter()
        .find(|diagnostic| diagnostic.code == "O002" || diagnostic.code == "O003")
        .unwrap_or_else(|| {
            let observed: Vec<_> = conflicting
                .iter()
                .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.message.as_str()))
                .collect();
            panic!(
                "mutating an owner under a live []T view must surface an ownership conflict: {observed:?}"
            )
        });
    assert!(!diagnostic.labels.is_empty());
    assert!(!diagnostic.notes.is_empty());

    file.set_text(&mut db)
        .to(Arc::from(slice_view_source(false)));
    assert!(!codes(&db, file)
        .iter()
        .any(|code| code == "O002" || code == "O003"));
}
