#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P3 — per-item IDE diagnostic memos early-cutoff on sibling edits.

use arandu_query::dataflow::ITEM_IDE_DIAGS_EXEC_COUNT;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{module_signatures, parse, type_check};
use arandu_query::{file_ide_diagnostics, ide_diags_fingerprint, item_ide_diagnostics, SourceFile};
use salsa::Setter;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

static COUNTER_LOCK: Mutex<()> = Mutex::new(());

fn body_items(db: &DatabaseImpl, file: SourceFile) -> Vec<arandu_middle::SymbolId> {
    let program = parse(db, file);
    let Ok(program) = &**program else {
        return vec![];
    };
    let sigs = module_signatures(db, file);
    arandu_semantics::body_item_symbols(program, sigs.resolved.as_ref())
}

fn two_funcs(beta_ret: i32) -> String {
    format!(
        r#"
func alpha(): int {{
    return 1
}}

func beta(): int {{
    return {beta_ret}
}}
"#
    )
}

#[test]
fn item_ide_diags_beta_edit_skips_alpha() {
    let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file("p3.aru".into(), two_funcs(2));

    let _ = type_check(&db, file);
    let items = body_items(&db, file);
    assert!(items.len() >= 2, "need 2 funcs, got {}", items.len());
    let alpha = items[0];
    let beta = items[1];

    let d_alpha0 = item_ide_diagnostics(&db, file, alpha);
    let _ = item_ide_diagnostics(&db, file, beta);
    let fp_alpha0 = ide_diags_fingerprint(d_alpha0);

    ITEM_IDE_DIAGS_EXEC_COUNT.store(0, Ordering::SeqCst);

    file.set_text(&mut db).to(Arc::from(two_funcs(99)));

    let items2 = body_items(&db, file);
    assert!(items2.len() >= 2);
    let alpha2 = items2[0];
    let beta2 = items2[1];

    let d_alpha1 = item_ide_diagnostics(&db, file, alpha2);
    let _ = item_ide_diagnostics(&db, file, beta2);
    let _ = file_ide_diagnostics(&db, file);

    let execs = ITEM_IDE_DIAGS_EXEC_COUNT.load(Ordering::SeqCst);
    assert!(
        execs <= 1,
        "expected ≤1 item_ide_diagnostics execute after beta-only edit, got {execs}"
    );

    let fp_alpha1 = ide_diags_fingerprint(d_alpha1);
    // Alpha diags content should be stable (empty↔empty) even if SymbolId renumbered.
    assert_eq!(
        fp_alpha0, fp_alpha1,
        "alpha IDE diags fingerprint should be stable across beta body edit"
    );
}

#[test]
fn file_ide_diagnostics_fingerprint_stable_on_noop() {
    let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file("fp3.aru".into(), "func main(): int { return 1 }\n".into());
    let d1 = file_ide_diagnostics(&db, file);
    let fp1 = ide_diags_fingerprint(d1);
    let d2 = file_ide_diagnostics(&db, file);
    let fp2 = ide_diags_fingerprint(d2);
    assert_eq!(fp1, fp2);
}

#[test]
fn parse_failure_reports_syntax_diagnostics_in_ide() {
    let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = DatabaseImpl::new();
    // A file whose parse fails must NOT yield an empty Problems panel.
    let file = db.new_file(
        "broken.aru".into(),
        "func main(): int { let 1 = 2; return 0 }\n".into(),
    );
    let diagnostics = file_ide_diagnostics(&db, file);
    assert!(
        !diagnostics.is_empty(),
        "parse failure must surface syntax diagnostics, got {:?}",
        **diagnostics
    );
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic
            .primary_label
            .as_deref()
            .is_some_and(|label| label.starts_with("unexpected "))),
        "syntax diagnostics should preserve their contextual primary label"
    );

    // Repeating a request without an edit preserves the diagnostic fingerprint.
    let fp1 = ide_diags_fingerprint(diagnostics);
    let d2 = file_ide_diagnostics(&db, file);
    assert_eq!(fp1, ide_diags_fingerprint(d2));

    // Repairing the file removes the syntax diagnostic.
    file.set_text(&mut db)
        .to(Arc::from("func main(): int { let value = 2; return 0 }\n"));
    let repaired = file_ide_diagnostics(&db, file);
    assert!(
        repaired
            .iter()
            .all(|d| !d.code.starts_with('P') && !d.code.starts_with("LX")),
        "repaired file should have no syntax diagnostics, got {:?}",
        repaired.iter().map(|d| d.code.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn recovering_diagnostics_reuse_parse_and_update_when_first_error_is_unchanged() {
    use arandu_query::passes::parse_diagnostics;
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file(
        "recover.aru".into(),
        "func main() { let 1 = 2; let 3 = 4; }".into(),
    );
    let first = parse(&db, file).as_ref().unwrap_err().clone();
    assert_eq!(parse_diagnostics(&db, file).len(), 2);
    // The query key includes the name and inputs. Both results consume one
    // parse execution; diagnostics do not run their own CST-to-AST lower.
    assert_eq!(log.count_executions_matching("parse("), 1);
    file.set_text(&mut db)
        .to(Arc::from("func main() { let 1 = 2; let valid = 4; }"));
    let after = parse(&db, file).as_ref().unwrap_err();
    assert_eq!(first.span, after.span);
    assert_eq!(first.message, after.message);
    assert_eq!(parse_diagnostics(&db, file).len(), 1);
}

#[test]
fn file_ide_compose_matches_item_union() {
    let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file("union.aru".into(), two_funcs(1));
    let file_diags = file_ide_diagnostics(&db, file);
    let items = body_items(&db, file);
    let mut union = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &id in &items {
        for d in item_ide_diagnostics(&db, file, id).iter() {
            let k = (d.start, d.end, d.code.clone(), d.message.clone());
            if seen.insert(k) {
                union.push(d.clone());
            }
        }
    }
    union.sort_by(|a, b| {
        (a.start, a.end, &a.code, &a.message).cmp(&(b.start, b.end, &b.code, &b.message))
    });
    let mut file_sorted = file_diags.iter().cloned().collect::<Vec<_>>();
    file_sorted.sort_by(|a, b| {
        (a.start, a.end, &a.code, &a.message).cmp(&(b.start, b.end, &b.code, &b.message))
    });
    // File may include signature diags; item union is subset.
    for d in &union {
        assert!(
            file_sorted.iter().any(|f| {
                f.start == d.start && f.end == d.end && f.code == d.code && f.message == d.message
            }),
            "file_ide_diagnostics missing item diag {:?}",
            d
        );
    }
}

#[test]
fn ide_diagnostic_preserves_labels_hints_and_replacements() {
    let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "rich.aru".into(),
        "func main() { let value: int = 1; set value = 2; }\n".into(),
    );
    let diagnostics = file_ide_diagnostics(&db, file);
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "T026")
        .expect("immutable assignment diagnostic");

    assert_eq!(diagnostic.labels.len(), 2);
    assert!(diagnostic.notes.is_empty());
    assert_eq!(diagnostic.hints.len(), 1);
    let replacement = diagnostic.hints[0]
        .replacement
        .as_ref()
        .expect("structured mutability replacement");
    assert_eq!(replacement.new_text, "mut value");

    let fingerprint = ide_diags_fingerprint(diagnostics);
    let mut changed_label = diagnostics.iter().cloned().collect::<Vec<_>>();
    changed_label
        .iter_mut()
        .find(|diagnostic| diagnostic.code == "T026")
        .expect("mutable cloned diagnostic")
        .primary_label = Some("new primary context".into());
    assert_ne!(fingerprint, ide_diags_fingerprint(&changed_label));

    let mut changed = diagnostics.iter().cloned().collect::<Vec<_>>();
    changed
        .iter_mut()
        .find(|diagnostic| diagnostic.code == "T026")
        .expect("mutable cloned diagnostic")
        .hints[0]
        .message
        .push('!');
    assert_ne!(fingerprint, ide_diags_fingerprint(&changed));
}

#[test]
fn file_ide_diags_reports_closure_body_move_errors() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "closure_move.aru".into(),
        r#"
struct Token { raw: ptr[int] }
func apply(f: (int) -> int): int {
    return f(1)
}
func main(): int {
    let tok = Token { raw: nil }
    return apply((x) => {
        let a = tok
        let b = tok
        return x
    })
}
"#
        .into(),
    );
    let diagnostics = file_ide_diagnostics(&db, file);
    assert!(
        diagnostics.iter().any(|d| d.code == "O001"),
        "expected O001 move error inside synthesized closure body, got: {:?}",
        diagnostics.as_slice()
    );
}
