//! Canonical pipeline execution and diagnostic emission for Arandu CLI.
//!
//! Enforces: CST (`syntax_tree`) → AST (`parse`) → `resolve` → `type_check` → `lower_amir` → backend.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli_error::{CliFailure, CliResult};
use arandu_middle::layout::DataLayout;

pub fn finish(result: CliResult) -> ! {
    let code = match result {
        Ok(success) => success.exit_code(),
        Err(failure) => {
            failure.render();
            failure.exit_code()
        }
    };
    arandu_base::print_perf_summary();
    arandu_base::finalize_self_profile();
    // `process::exit` skips destructors, including the standard output
    // buffers. Flush both streams explicitly so CLI output is not lost on
    // platforms whose stdout/stderr are block-buffered (notably Windows).
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    process::exit(code);
}

pub fn fail_usage(message: impl Into<String>) -> ! {
    finish(Err(CliFailure::usage(message)))
}

/// Guards JIT execution, which runs inside the host process, against a
/// non-host layout. Returns a usage error for `run`/`test`/`bench`.
pub(crate) fn ensure_host_jit_layout(data_layout: DataLayout) -> Result<(), CliFailure> {
    let host_width = std::mem::size_of::<usize>() as u64;
    if data_layout.pointer_width() == host_width {
        return Ok(());
    }
    Err(CliFailure::usage(format!(
        "JIT execution requires the host layout (got pointer width {}, host is {}-bit); \
         use `--layout=by-host` or the `build`/`emit-c` commands for non-host layouts",
        data_layout.pointer_width(),
        host_width * 8,
    )))
}

pub fn fail_operational(
    operation: &'static str,
    context: Option<PathBuf>,
    source: impl Into<String>,
) -> ! {
    finish(Err(CliFailure::operational(operation, context, source)))
}

pub fn print_diagnostics_and_exit(
    diagnostics: impl IntoIterator<Item = arandu_middle::Diagnostic>,
    filepath: &str,
) -> ! {
    let source_path = (!filepath.is_empty()).then(|| PathBuf::from(filepath));
    finish(Err(CliFailure::diagnostics(diagnostics, source_path)))
}

pub fn print_parse_error_and_exit(err: &arandu_parser::ParseError, filepath: &str) -> ! {
    let diag = arandu_middle::Diagnostic::from(err.clone());
    print_diagnostics_and_exit(std::iter::once(diag), filepath);
}

pub fn optimize_amir_or_exit(
    amir: &mut arandu_semantics::amir::AmirProgram,
    type_check: &arandu_semantics::TypeCheckResult,
    filepath: &str,
) {
    if let Err(diag) = arandu_semantics::optimize_amir_checked(
        amir,
        type_check.symbols.as_ref(),
        &type_check.type_info.type_interner,
    ) {
        print_diagnostics_and_exit(std::iter::once(diag), filepath);
    }
}

pub fn optimize_amir_with_level_or_exit(
    amir: &mut arandu_semantics::amir::AmirProgram,
    type_check: &arandu_semantics::TypeCheckResult,
    level: arandu_semantics::OptLevel,
    filepath: &str,
) {
    if let Err(diag) = arandu_semantics::optimize_amir_checked_with_level(
        amir,
        type_check.symbols.as_ref(),
        &type_check.type_info.type_interner,
        level,
    ) {
        print_diagnostics_and_exit(std::iter::once(diag), filepath);
    }
}

pub fn validate_hir_and_monomorphize(
    hir: &mut arandu_semantics::hir::HirProgram,
    type_check: &mut arandu_semantics::TypeCheckResult,
    filepath: &str,
) {
    if let Err(err) = hir.validate_invariants(&hir.pool, &type_check.symbols) {
        let diag = arandu_middle::Diagnostic::ice(
            arandu_middle::DiagCode::ICEL001,
            format!("HIR invariant violation before monomorphization: {err}"),
            arandu_middle::Span::new(0, 0, 0),
        );
        print_diagnostics_and_exit(std::iter::once(diag), filepath);
    }

    if let Err(diags) =
        arandu_semantics::passes::monomorphize::monomorphize_program(type_check, hir)
    {
        print_diagnostics_and_exit(diags, filepath);
    }
}

pub struct CheckedProgram {
    /// Shared with Salsa memo — never deep-clone the AST.
    pub program: std::sync::Arc<arandu_parser::Program>,
    pub type_check: arandu_semantics::TypeCheckResult,
}

pub struct PipelineLowerOutcome {
    pub artifacts: std::sync::Arc<arandu_query::LowerAmirArtifacts>,
    pub diagnostics: Vec<arandu_middle::Diagnostic>,
}

fn dedupe_accumulated_diags(
    diags: &[impl std::ops::Deref<Target = arandu_middle::db::DiagnosticsAccumulator>],
) -> Vec<arandu_middle::Diagnostic> {
    let mut deduped = Vec::with_capacity(diags.len());
    for diagnostic in diags {
        if !deduped.contains(&diagnostic.0) {
            deduped.push(diagnostic.0.clone());
        }
    }
    deduped
}

pub fn render_nonfatal_diagnostics(
    db: &dyn arandu_query::db::ArandCompilerDb,
    diagnostics: &[arandu_middle::Diagnostic],
    filepath: &str,
) {
    for diagnostic in diagnostics {
        let (path_str, text_arc) = if diagnostic.span.file_id != 0 {
            (
                db.file_path(diagnostic.span.file_id)
                    .to_string_lossy()
                    .to_string(),
                db.source_text(diagnostic.span.file_id),
            )
        } else {
            let source = std::fs::read_to_string(filepath).unwrap_or_default();
            (filepath.to_string(), source.into())
        };
        let named_source = miette::NamedSource::new(path_str, text_arc);
        let report = miette::Report::new(diagnostic.clone()).with_source_code(named_source);
        eprintln!("{:?}", report);
    }
}

pub fn render_pipeline_failure(
    db: &dyn arandu_query::db::ArandCompilerDb,
    diagnostics: Vec<arandu_middle::Diagnostic>,
    filepath: &str,
) -> ! {
    let target_path = diagnostics
        .iter()
        .find(|diagnostic| matches!(diagnostic.severity, arandu_middle::Severity::Error))
        .and_then(|diagnostic| {
            (diagnostic.span.file_id != 0).then(|| {
                db.file_path(diagnostic.span.file_id)
                    .to_string_lossy()
                    .to_string()
            })
        })
        .unwrap_or_else(|| filepath.to_string());
    print_diagnostics_and_exit(diagnostics, &target_path)
}

/// Render non-fatal Salsa diagnostics or terminate through the typed diagnostic path.
pub fn handle_accumulated_diags(
    db: &dyn arandu_query::db::ArandCompilerDb,
    diags: &[impl std::ops::Deref<Target = arandu_middle::db::DiagnosticsAccumulator>],
    filepath: &str,
) {
    if diags.is_empty() {
        return;
    }
    let deduped = dedupe_accumulated_diags(diags);
    if deduped
        .iter()
        .any(|d| matches!(d.severity, arandu_middle::Severity::Error))
    {
        render_pipeline_failure(db, deduped, filepath);
    }
    render_nonfatal_diagnostics(db, &deduped, filepath);
}

/// Pure, non-emitting pipeline boundary used by parallel CLI orchestration.
/// Diagnostics are returned to the caller so observable output can be emitted
/// later in stable input order.
pub fn pipeline_lower_checked(
    db: &dyn arandu_query::db::ArandCompilerDb,
    file: arandu_query::db::SourceFile,
) -> Result<PipelineLowerOutcome, Vec<arandu_middle::Diagnostic>> {
    {
        arandu_base::time_pass!("parse");
        let program_res = arandu_query::passes::parse(db, file);
        if let Err(error) = &**program_res {
            return Err(vec![arandu_middle::Diagnostic::from(error.clone())]);
        }
    }

    {
        arandu_base::time_pass!("type_check");
        let _ = arandu_query::passes::type_check(db, file);
    }
    let type_accumulated = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    let mut diagnostics = dedupe_accumulated_diags(&type_accumulated);
    if diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.severity, arandu_middle::Severity::Error))
    {
        return Err(diagnostics);
    }

    let artifacts = {
        arandu_base::time_pass!("lower-amir");
        arandu_query::passes::lower_amir(db, file)
    };
    let lower_accumulated = arandu_query::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    for diagnostic in dedupe_accumulated_diags(&lower_accumulated) {
        if !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
        }
    }
    if diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.severity, arandu_middle::Severity::Error))
    {
        return Err(diagnostics);
    }

    Ok(PipelineLowerOutcome {
        artifacts: std::sync::Arc::clone(&artifacts.value),
        diagnostics,
    })
}

/// Single pipeline entry for check / run / amir / emit-c:
/// parse → type_check (diags once) → lower_amir (lower diags once).
/// Salsa memos each step; no second HIR/mono outside the query.
pub fn pipeline_lower(
    db: &dyn arandu_query::db::ArandCompilerDb,
    file: arandu_query::db::SourceFile,
    filepath: &str,
) -> std::sync::Arc<arandu_query::LowerAmirArtifacts> {
    match pipeline_lower_checked(db, file) {
        Ok(outcome) => {
            render_nonfatal_diagnostics(db, &outcome.diagnostics, filepath);
            outcome.artifacts
        }
        Err(diagnostics) => render_pipeline_failure(db, diagnostics, filepath),
    }
}

/// Parse + type-check for paths that still need a local TypeCheckResult (e.g. `hir`).
pub fn parse_and_check(
    db: &dyn arandu_query::db::ArandCompilerDb,
    file: arandu_query::db::SourceFile,
    filepath: &str,
) -> CheckedProgram {
    let program_res = {
        arandu_base::time_pass!("parse");
        arandu_query::passes::parse(db, file)
    };
    let program = match &**program_res {
        Ok(program) => std::sync::Arc::clone(program),
        Err(err) => print_parse_error_and_exit(err, filepath),
    };

    let type_check = {
        arandu_base::time_pass!("type_check");
        arandu_query::passes::type_check(db, file)
    };

    let diagnostics = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    handle_accumulated_diags(db, &diagnostics, filepath);

    // TypeCheckResult is Arc-heavy (symbols/resolved/type_info) — clone is O(1) for IR.
    CheckedProgram {
        program,
        type_check: (**type_check).clone(),
    }
}

/// Opt-in GenRef observability, intentionally outside Salsa queries. This
/// derives metrics from the immutable AMIR artifact and has no query-visible
/// side effects.
pub fn print_genref_report(filepath: &str, artifacts: &arandu_query::LowerAmirArtifacts) {
    use arandu_middle::amir::{AmirRvalue, AmirStmt};

    let mut total_promotions = 0usize;
    let mut total_checks = 0usize;
    let mut rows = Vec::new();
    for func in &artifacts.amir.funcs {
        let mut promotions = 0usize;
        let mut checks = 0usize;
        for stmt in func.stmts.payloads.iter() {
            let AmirStmt::Assign { rhs, .. } = stmt else {
                continue;
            };
            match rhs {
                AmirRvalue::GenInsert { .. } => promotions += 1,
                AmirRvalue::GenGet { .. }
                | AmirRvalue::GenSet { .. }
                | AmirRvalue::GenUpsert { .. }
                | AmirRvalue::GenRemove { .. } => checks += 1,
                _ => {}
            }
        }
        if promotions != 0 || checks != 0 {
            let name = artifacts.type_check.symbols.get(func.symbol).name.clone();
            rows.push((name, promotions, checks));
            total_promotions += promotions;
            total_checks += checks;
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    eprintln!(
        "[arandu][genref] module={filepath} promotions={total_promotions} checks={total_checks} functions={}",
        rows.len()
    );
    for (name, promotions, checks) in rows {
        eprintln!("[arandu][genref] function={name} promotions={promotions} checks={checks}");
    }
}

/// Attach resolved stdlib root to the DB (install cascade; never cwd-only).
pub fn attach_stdlib(db: &mut arandu_query::DatabaseImpl, explicit: Option<PathBuf>) {
    match arandu_query::resolve_stdlib_root(arandu_query::StdlibResolveOpts {
        explicit,
        ..Default::default()
    }) {
        Ok(root) => {
            db.set_stdlib_root(root.path.clone());
            register_stdlib_sources(db, &root.path);
        }
        Err(e) => {
            // Soft for single-file tools that only need prelude; hard later on import.
            // Doctor / project mode surface the hard error. Log once at debug.
            tracing::debug!("stdlib resolve deferred: {e}");
        }
    }
}

/// Load the immutable installed stdlib at an explicit orchestration boundary.
/// Semantic module resolution never performs filesystem I/O.
pub fn register_stdlib_sources(db: &mut arandu_query::DatabaseImpl, root: &Path) {
    for relative in arandu_query::scan_aru_entries(root) {
        let path = root.join(&relative);
        let key = path.to_string_lossy().into_owned();
        if db.source_file_by_path(&key).is_some() {
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(text) => {
                db.new_stdlib_file(key, text);
            }
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "stdlib module load deferred");
            }
        }
    }
}

pub fn open_entry_file(
    db: &arandu_query::DatabaseImpl,
    registry: &mut arandu_base::SourceRegistry,
    entry: &Path,
) -> (arandu_query::SourceFile, String) {
    let source = match fs::read_to_string(entry) {
        Ok(s) => s,
        Err(err) => {
            fail_operational("failed to read", Some(entry.to_path_buf()), err.to_string());
        }
    };
    let filepath = entry.to_string_lossy().into_owned();
    let file_id = registry.register(&filepath, &source);
    if let Some(existing) = registered_entry_source(db, entry, &source) {
        return (existing, filepath);
    }
    let code = std::sync::Arc::from(source);
    let source_file =
        arandu_query::SourceFile::new(db, file_id, code, std::sync::Arc::new(entry.to_path_buf()));
    db.register_source_file(filepath.clone(), source_file);
    (source_file, filepath)
}

/// Reuse an exact loader input, including through a canonical path alias.
/// Filesystem normalization belongs to this driver, not tracked queries.
pub(crate) fn registered_entry_source(
    db: &arandu_query::DatabaseImpl,
    path: &Path,
    source: &str,
) -> Option<arandu_query::SourceFile> {
    db.source_file_by_path(&path.to_string_lossy())
        .or_else(|| {
            path.canonicalize()
                .ok()
                .and_then(|path| db.source_file_by_path(&path.to_string_lossy()))
        })
        .filter(|file| file.text(db).as_ref() == source)
}

pub fn find_aru_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                find_aru_files(&path, files)?;
            } else if path.extension().and_then(|s| s.to_str()) == Some("aru") {
                files.push(path);
            }
        }
    }
    Ok(())
}

/// True when `path` should use package mode (dir with Arandu.toml, or the toml itself).
pub fn is_project_target(arg: Option<&str>) -> bool {
    let Some(arg) = arg else {
        return true;
    };
    let p = Path::new(arg);
    if p.file_name().and_then(|s| s.to_str()) == Some(arandu_query::MANIFEST_FILENAME) {
        return true;
    }
    if p.is_dir() && p.join(arandu_query::MANIFEST_FILENAME).is_file() {
        return true;
    }
    // Explicit: if a parent walk finds a manifest and the path is not a .aru file,
    // still allow package mode when the user points at the package root.
    matches!(crate::manifest_io::find_manifest(p), Ok(Some(_)))
        && p.extension().and_then(|e| e.to_str()) != Some("aru")
}
