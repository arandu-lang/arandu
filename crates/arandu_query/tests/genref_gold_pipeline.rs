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

/// RFC 0043 Dual-Locus: a borrow stored in memory typed as a reference promotes
/// the borrowed local's Storage Locus (`lambda_store`) to `TaskArena` while
/// keeping its Reference Locus (`lambda_ref`) as a native pointer (`DirectPtr`),
/// emitting an inspectable `O004` Note unless `@NoFallback` rejects it.
#[test]
fn o004_promotes_borrow_stored_in_memory_via_dual_locus_and_rejects_under_no_fallback() {
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
            diagnostic.severity, 2,
            "O004 must be an inspectable note under Dual-Locus promotion: {diagnostic:?}"
        );
        assert!(diagnostic
            .notes
            .iter()
            .any(|note| note.contains("escape path:")));
        assert!(
            diagnostic
                .notes
                .iter()
                .any(|note| note.contains("TaskArena") && note.contains("RFC 0043")),
            "{diagnostic:?}"
        );
        let amir = lower(source);
        assert!(
            amir.contains("arena_alloc("),
            "expected Dual-Locus TaskArena allocation in AMIR:\n{amir}"
        );
    }

    // With @NoFallback, Dual-Locus promotion is rejected as an Error (severity 0).
    let no_fallback_source = r#"
struct Holder { value: &int }

@NoFallback
func main(): int {
    let start = 5
    let holder = Holder { value: &start }
    return *holder.value
}
"#;
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "genref_o004_nofallback.aru".into(),
        no_fallback_source.into(),
    );
    let diagnostics = file_ide_diagnostics(&db, file);
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "O004")
        .expect("O004 IDE diagnostic under @NoFallback");
    assert_eq!(
        diagnostic.severity, 0,
        "O004 must be an error under @NoFallback: {diagnostic:?}"
    );
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
fn aggregate_store_escape_lowers_to_dual_locus_task_arena_amir() {
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
        !lowered.amir.funcs.is_empty(),
        "Dual-Locus aggregate escape must reach AMIR"
    );
    let amir = lowered.amir.pretty_print(
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
    );
    assert!(
        amir.contains("arena_alloc("),
        "expected Dual-Locus TaskArena allocation in AMIR:\n{amir}"
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.0.code == DiagCode::O004GenerationalFallback
                && diagnostic.0.severity == arandu_middle::Severity::Note
        }),
        "projected escape must emit an inspectable O004 Note"
    );
}
