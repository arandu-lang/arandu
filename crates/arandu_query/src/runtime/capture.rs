use crate::db::HashEq;
use crate::passes::{parse, PreparedHir};
use crate::{ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::types::FunctionInstance;
use arandu_middle::SymbolId;

/// A synthesized `ThreadJob.run` has no AST declaration. Its body is the
/// closure, rewritten so each capture is `self.field`. Ordinary functions
/// never enter this query path.
pub(super) fn thread_capture_function_hir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<PreparedHir> {
    let typed = crate::passes::type_check(db, file);
    let parsed = parse(db, file);
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"FunctionHir/v1-thread-capture");
    fingerprint.update(&symbol.file_id.to_le_bytes());
    fingerprint.update(&symbol.local_id.0.to_le_bytes());
    fingerprint.update(typed.stable_hash().as_bytes());
    let mut checked = (**typed).clone();
    let mut diagnostics = Vec::new();
    let hir = match parsed.as_ref() {
        Ok(program) => {
            match arandu_semantics::lower_thread_capture_hir(&mut checked, program, symbol) {
                Ok(hir) => Some(hir),
                Err(errors) => {
                    diagnostics = errors;
                    None
                }
            }
        }
        Err(_) => None,
    };
    HashEq::new(PreparedHir {
        hir,
        type_check: checked,
        diagnostics,
        source_fingerprint: fingerprint.finalize(),
    })
}

pub(super) fn import_synthetic_captures(
    db: &dyn ArandCompilerDb,
    key: &FunctionInstance,
    checked: &mut arandu_semantics::TypeCheckResult,
) {
    let mut files = Vec::new();
    for argument in &key.arguments {
        let _ = argument.for_each_symbol(|symbol| {
            if symbol.is_synthetic() && checked.symbols.try_get(symbol).is_none() {
                files.push(symbol.file_id);
            }
        });
    }
    files.sort_unstable();
    files.dedup();
    for file_id in files {
        let Some(source) = db.source_file_by_id(file_id) else {
            continue;
        };
        let typed = crate::passes::type_check(db, source);
        arandu_typeck::type_checker::info::import_thread_captures(checked, typed);
    }
}
