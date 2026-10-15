//! Bounded incremental oracle for the generated static tree. Span-changing
//! edits are checked against clean analysis, not incorrectly required to cut off.

use arandu_middle::Severity;
use arandu_query::{passes, DatabaseImpl};

use super::Failure;
use crate::smith::synth::synthesize_nested_comptime_if;

pub(super) fn check_cache(seed: u64) -> Result<(), Failure> {
    let generated = synthesize_nested_comptime_if(seed, 3);
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("smith-static.aru".into(), generated.source.clone());
    let initial = passes::type_check(&db, file);
    if initial
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Err(Failure::new(
            "static-if-incremental",
            format!(
                "seed {seed}: valid generated tree rejected: {:?}",
                initial.diagnostics
            ),
            false,
        ));
    }
    log.clear();
    db.update_file_text(
        file,
        format!("{}\n// outside the owner\n", generated.source),
    );
    let _ = passes::type_check(&db, file);
    if log.count_executions_matching("item_static_branches") != 0 {
        return Err(Failure::new(
            "static-if-cutoff",
            format!("seed {seed}: an edit outside the owner reran static selection"),
            false,
        ));
    }
    log.clear();
    let edited = generated
        .source
        .replace("missing", "unknown")
        .replace("nonexistent", "unavailable")
        .replace("absent", "unseen");
    db.update_file_text(file, edited.clone());
    let incremental = passes::type_check(&db, file);
    if log.count_executions_matching("ctfe_eval_root") != 0 {
        return Err(Failure::new(
            "static-if-cutoff",
            format!(
                "seed {seed}: unchanged conditions reran evaluation after a discarded-code edit"
            ),
            false,
        ));
    }
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("smith-static.aru".into(), edited);
    let reference = passes::type_check(&clean, clean_file);
    if incremental.diagnostics != reference.diagnostics
        || incremental.resolved.comptime_branches != reference.resolved.comptime_branches
    {
        return Err(Failure::new(
            "static-if-incremental",
            format!("seed {seed}: incremental static decisions differ from clean"),
            false,
        ));
    }
    Ok(())
}
