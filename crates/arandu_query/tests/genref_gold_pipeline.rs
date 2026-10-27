#![allow(clippy::expect_used)]

//! G0 characterization through the canonical query pipeline.

use arandu_middle::DiagCode;
use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::lower_amir;

fn lower(source: &str) -> String {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("genref_gold.aru".into(), source.into());
    let lowered = lower_amir(&db, file);
    assert!(
        !lowered.amir.funcs.is_empty(),
        "surface program did not reach AMIR: {:?}",
        lowered.type_check.diagnostics
    );
    lowered.amir.pretty_print(
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
    )
}

/// A promoted handle stored in memory typed as a reference would be read back
/// as a raw pointer (AGG.1: every backend returned the handle bits instead of
/// the value), so the pipeline rejects the escape instead of compiling it.
#[test]
fn o004_rejects_a_promotable_borrow_stored_in_memory() {
    for source in [
        // Stored through a reference parameter.
        r#"
struct Holder { value: &int }

func store(holder: mut ref Holder): int {
    let value = 42
    set holder.value = &value
    return *holder.value
}

func main(): int {
    let value = 0
    let mut holder = Holder { value: &value }
    return store(mut ref holder)
}
"#,
        // Stored in a local aggregate.
        r#"
struct Holder { value: &int }

func main(): int {
    let start = 5
    let holder = Holder { value: &start }
    return *holder.value
}
"#,
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("genref_o004_store.aru".into(), source.into());
        let diagnostics = file_ide_diagnostics(&db, file);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "O004")
            .expect("O004 IDE diagnostic");
        assert_eq!(
            diagnostic.severity, 0,
            "O004 must be an error: {diagnostic:?}"
        );
        assert!(diagnostic
            .notes
            .iter()
            .any(|note| note.contains("escape path:")));
        assert!(
            diagnostic
                .notes
                .iter()
                .any(|note| note.contains("cannot be safely re-read")),
            "{diagnostic:?}"
        );
        assert!(
            !diagnostic
                .notes
                .iter()
                .any(|note| note.contains("@NoFallback")),
            "the rejection is about representation, not a fallback scope: {diagnostic:?}"
        );
    }
}

#[test]
fn proven_local_borrow_has_no_generational_runtime_operations() {
    let amir = lower(
        r#"
func main(): int {
    let value = 41
    let borrowed = &value
    return *borrowed + 1
}
"#,
    );

    assert!(!amir.contains("gen_insert"), "unexpected fallback:\n{amir}");
    assert!(!amir.contains("gen_get"), "unexpected fallback:\n{amir}");
    assert!(!amir.contains("gen_remove"), "unexpected fallback:\n{amir}");
}

#[test]
fn aggregate_store_escape_characterizes_the_missing_surface_to_amir_path() {
    let source = r#"
struct Holder {
    value: &int
}
struct Pair {
    left: int
    right: int
}

func store(holder: mut ref Holder): int {
    let pair = Pair { left: 42, right: 7 }
    set holder.value = &pair.left
    return *holder.value
}

func main(): int {
    let value = 0
    let mut holder = Holder { value: &value }
    return store(mut ref holder)
}
"#;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("genref_escape_gap.aru".into(), source.into());
    let lowered = lower_amir(&db, file);
    let diagnostics =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file);

    assert!(
        lowered.amir.funcs.is_empty(),
        "G0 characterization changed: aggregate escape unexpectedly reached AMIR"
    );
    assert!(
        !diagnostics.is_empty(),
        "lowering discarded the program without an accumulated diagnostic"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.code == DiagCode::O004GenerationalFallback),
        "expected the aggregate escape to remain inspectable as O004"
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.0.code == DiagCode::O004GenerationalFallback
                && diagnostic.0.severity == arandu_middle::Severity::Error
        }),
        "projected escape must hard-fail instead of creating a stale snapshot"
    );
}
