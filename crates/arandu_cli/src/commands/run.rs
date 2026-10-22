//! Execution of single files and projects via Cranelift JIT / interpreter pipeline.

use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::args::RunBackend;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::pipeline::{
    attach_stdlib, ensure_host_jit_layout, fail_operational, fail_usage, find_aru_files,
    open_entry_file, optimize_amir_or_exit, pipeline_lower, pipeline_lower_checked,
    print_diagnostics_and_exit, print_genref_report, render_nonfatal_diagnostics,
    render_pipeline_failure,
};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;

#[path = "run_dispatch.rs"]
mod run_dispatch;

#[allow(clippy::too_many_arguments)]
pub fn cmd_project_run(
    start: &Path,
    flags: &ProjectFlags,
    opt: bool,
    _debug: bool,
    genref_report: bool,
    data_layout: DataLayout,
    backend: RunBackend,
    program_args: &[String],
) -> CliResult {
    if flags.release || flags.size {
        return Err(CliFailure::usage(
            "`--release` and `--size` are supported by `build`; `run` remains the interactive Cranelift JIT path",
        ));
    }
    ensure_host_jit_layout(data_layout)?;
    if backend == RunBackend::Llvm && flags.target.is_some() {
        return Err(CliFailure::usage(
            "experimental LLVM execution supports only the host target",
        ));
    }
    let explain_rebuild = arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let (mut db, rebuild_log) = if flags.verbose || explain_rebuild {
        let (db, log) = arandu_query::DatabaseImpl::with_rebuild_log();
        (db, Some(log))
    } else {
        (arandu_query::DatabaseImpl::new(), None)
    };
    db.set_target_config(data_layout);
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };
    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    let artifacts = pipeline_lower(&db, file, &filepath);
    if genref_report {
        print_genref_report(&filepath, &artifacts);
    }
    if !flags.quiet
        && let Some(log) = &rebuild_log
    {
        eprintln!("{}", log.status_line());
    }

    let type_check = &artifacts.type_check;
    let mut amir_owned = if opt {
        Some(artifacts.amir.clone())
    } else {
        None
    };
    if let Some(ref mut amir) = amir_owned {
        optimize_amir_or_exit(amir, type_check, &filepath);
    }
    let amir = match &amir_owned {
        Some(a) => a,
        None => &artifacts.amir,
    };

    if backend == RunBackend::Llvm {
        return execute_llvm_main(
            amir,
            type_check,
            ctx.entry_path,
            program_args,
            "run project",
        );
    }

    use arandu_semantics::CodegenBackend;
    let output = {
        let backend = match arandu_backend_cranelift::CraneliftBackend::try_new() {
            Ok(b) => b,
            Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
        };
        match CodegenBackend::compile(
            backend,
            amir,
            type_check.symbols.as_ref(),
            type_check.type_info.as_ref(),
        ) {
            Ok(out) => out,
            Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
        }
    };

    execute_jit_main(
        &output,
        amir,
        type_check,
        ctx.entry_path,
        program_args,
        "run project",
    )
}

pub(super) fn execute_jit_main(
    output: &impl arandu_semantics::CompiledCode,
    amir: &arandu_semantics::amir::AmirProgram,
    type_check: &arandu_semantics::TypeCheckResult,
    context_path: PathBuf,
    program_args: &[String],
    action: &'static str,
) -> Result<CliSuccess, CliFailure> {
    use arandu_semantics::CompiledCode;

    let mut has_main = false;
    let mut main_is_void = false;

    for f in &amir.funcs {
        if type_check.symbols.get(f.symbol).name.as_str() == "main" {
            has_main = true;
            main_is_void = matches!(
                type_check.type_info.type_interner.resolve(f.return_type),
                arandu_semantics::types::ArType::Void
            );
            break;
        }
    }

    if !has_main {
        return Err(CliFailure::operational(
            action,
            Some(context_path),
            "'main' function not found in compiled program",
        ));
    }

    let mut argv = Vec::with_capacity(program_args.len() + 1);
    argv.push(context_path.to_string_lossy().into_owned());
    argv.extend_from_slice(program_args);
    if arandu_runtime::os_runtime::install_program_args(argv.into_boxed_slice()).is_err() {
        return Err(CliFailure::operational(
            action,
            Some(context_path),
            "program arguments were already installed in this process",
        ));
    }

    unsafe {
        if main_is_void {
            if let Some(main_fn) = CompiledCode::get_fn::<unsafe fn()>(output, "main") {
                main_fn();
                return Ok(CliSuccess::Done);
            }
        } else if let Some(main_fn) = CompiledCode::get_fn::<unsafe fn() -> i32>(output, "main") {
            let code = main_fn();
            return Ok(CliSuccess::ProgramExit(code));
        }
    }

    Err(CliFailure::operational(
        action,
        Some(context_path),
        "compiled module does not export a callable 'main' function",
    ))
}

pub(super) fn execute_llvm_main(
    amir: &arandu_semantics::amir::AmirProgram,
    type_check: &arandu_semantics::TypeCheckResult,
    context_path: PathBuf,
    program_args: &[String],
    action: &'static str,
) -> CliResult {
    let has_main = amir
        .funcs
        .iter()
        .any(|f| type_check.symbols.get(f.symbol).name.as_str() == "main");
    if !has_main {
        return Err(CliFailure::operational(
            action,
            Some(context_path),
            "'main' function not found in compiled program",
        ));
    }

    let ir = arandu_backend_llvm::emit_llvm_ir(
        amir,
        type_check.symbols.as_ref(),
        type_check.type_info.as_ref(),
    )
    .map_err(|diag| CliFailure::diagnostics(std::iter::once(diag), Some(context_path.clone())))?;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let ir_path =
        std::env::temp_dir().join(format!("arandu-llvm-{}-{unique}.ll", std::process::id()));
    if let Err(error) = fs::write(&ir_path, ir) {
        return Err(CliFailure::operational(
            action,
            Some(context_path),
            format!("failed to write temporary LLVM IR: {error}"),
        ));
    }

    let result = run_llvm_ir(&ir_path, program_args, &context_path, action);
    let _ = fs::remove_file(&ir_path);
    result
}

fn run_llvm_ir(
    ir_path: &Path,
    program_args: &[String],
    context_path: &Path,
    action: &'static str,
) -> CliResult {
    // `llvm-as` accepts external declarations, so IR validation alone does not
    // prove that a program is runnable. Prefer the native linker path: it is the
    // only one that resolves Arandu runtime symbols such as `io.println`.
    if tool_available("clang") {
        // Native link is the only path that resolves Arandu runtime symbols
        // (`io.println`, `ar_rt_raw_*`, ToStr helpers). Fail loudly if the
        // packaged static library is missing instead of producing an
        // unresolvable binary.
        let runtime = crate::linker::runtime_library()?;
        let binary_path = ir_path.with_extension("bin");
        let mut command = Command::new("clang");
        command.arg(ir_path).arg(&runtime);
        command.args(crate::linker::host_runtime_link_args());
        let compile = command
            .arg("-o")
            .arg(&binary_path)
            .status()
            .map_err(|error| {
                CliFailure::operational(
                    action,
                    Some(context_path.to_path_buf()),
                    format!("failed to start clang: {error}"),
                )
            })?;
        if !compile.success() {
            return Err(CliFailure::operational(
                action,
                Some(context_path.to_path_buf()),
                format!(
                    "clang failed to compile or link the emitted LLVM IR (runtime {})",
                    runtime.display()
                ),
            ));
        }
        let status = Command::new(&binary_path)
            .args(program_args)
            .status()
            .map_err(|error| {
                CliFailure::operational(
                    action,
                    Some(context_path.to_path_buf()),
                    format!("failed to start LLVM program: {error}"),
                )
            });
        let _ = fs::remove_file(&binary_path);
        return Ok(CliSuccess::ProgramExit(status?.code().unwrap_or(1)));
    }

    Err(CliFailure::operational(
        action,
        Some(context_path.to_path_buf()),
        "LLVM execution requires `clang` on PATH; `lli` cannot link the Arandu runtime",
    ))
}

fn tool_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn cmd_single_file_dispatch(
    command: &str,
    target_path: &Path,
    inv: &crate::args::CliInvocation,
) -> CliResult {
    if inv.project_flags.size && inv.debug {
        return Err(CliFailure::usage(
            "`--size` cannot be combined with `--debug`",
        ));
    }
    if inv.project_flags.size && !matches!(command, "emit-c" | "emit-wasm" | "emit-component") {
        return Err(CliFailure::usage(
            "`--size` requires build, emit-c, emit-wasm or emit-component",
        ));
    }
    if inv.backend != RunBackend::Cranelift && command != "run" {
        return Err(CliFailure::usage(
            "`--backend` is supported only by `run` and `run -b llvm`",
        ));
    }
    if inv.backend == RunBackend::Llvm
        && inv.project_flags.target.is_some()
        && command != "emit-llvm"
    {
        return Err(CliFailure::usage(
            "experimental LLVM execution supports only the host target",
        ));
    }
    if command == "emit-llvm" {
        match inv.project_flags.target.as_deref() {
            Some(triple) if arandu_middle::embedded_profile(triple).is_some() => {
                if inv.data_layout.pointer_width() != 4 {
                    return Err(CliFailure::usage(
                        "bare-metal profiles use a 32-bit data layout",
                    ));
                }
            }
            Some(_) => {
                return Err(CliFailure::usage(
                    "experimental emit-llvm supports the host target or thumbv6m-none-eabi / thumbv7em-none-eabihf / riscv32imc-unknown-none-elf",
                ));
            }
            None if inv.data_layout != DataLayout::host() => {
                return Err(CliFailure::usage(
                    "experimental emit-llvm supports only the host target and layout",
                ));
            }
            None => {}
        }
    }
    let parallel = inv.parallel;
    let debug = inv.debug;
    let opt = inv.opt;
    let backend = inv.backend;
    let genref_report = inv.genref_report;
    let cfg = inv.cfg;
    let ascii = inv.ascii;
    let data_layout = inv.data_layout;
    let project_flags = &inv.project_flags;
    let mut paths = Vec::new();
    if target_path.is_dir() {
        if let Err(err) = find_aru_files(target_path, &mut paths) {
            fail_operational(
                "failed to list directory",
                Some(target_path.to_path_buf()),
                err.to_string(),
            );
        }
        paths.sort();
    } else {
        paths.push(target_path.to_path_buf());
    }

    if paths.is_empty() {
        fail_operational(
            "find Arandu sources",
            Some(target_path.to_path_buf()),
            "no .aru source files found",
        );
    }

    let use_parallel = parallel || paths.len() > 1;
    if use_parallel && command != "check" {
        fail_operational(
            "run command",
            None,
            format!(
                "parallel/multi-file mode is supported only for 'check'; \
                 command '{command}' has ordered user-visible output"
            ),
        );
    }

    let explain = arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let want_status = project_flags.verbose || explain;
    let (mut db, rebuild_log) = if want_status {
        let (db, log) = arandu_query::db::DatabaseImpl::with_rebuild_log();
        (db, Some(log))
    } else {
        (arandu_query::db::DatabaseImpl::new(), None)
    };
    db.set_ctfe_limits(project_flags.ctfe_limits);
    attach_stdlib(&mut db, project_flags.stdlib_path.clone());
    let data_layout = if matches!(command, "emit-wasm" | "emit-component") {
        let identity = if let Some(triple) = &project_flags.target {
            let identity = arandu_middle::db::TargetIdentity::from_triple(triple)
                .unwrap_or_else(|error| fail_usage(error));
            if identity.arch != "wasm32" {
                fail_usage("WebAssembly emission requires a wasm32 target");
            }
            identity
        } else {
            arandu_middle::db::TargetIdentity {
                os: "wasi".into(),
                arch: "wasm32".into(),
                triple: String::new(),
            }
        };
        db.set_target_identity(identity);
        arandu_middle::layout::DataLayout::ptr_width(4)
    } else {
        if let Some(triple) = &project_flags.target {
            let identity = arandu_middle::db::TargetIdentity::from_triple(triple)
                .unwrap_or_else(|error| fail_usage(error));
            db.set_target_identity(identity);
        }
        data_layout
    };
    db.set_target_config(data_layout);
    let mut registry = arandu_base::SourceRegistry::default();

    let mut source_files = Vec::new();
    for p in &paths {
        match fs::read_to_string(p) {
            Ok(source) => {
                let filepath = p.to_string_lossy().into_owned();
                let file_id = registry.register(&filepath, &source);
                let source: std::sync::Arc<str> = std::sync::Arc::from(source);
                let source_file = match crate::pipeline::registered_entry_source(&db, p, &source) {
                    Some(existing) => existing,
                    None => {
                        let file = arandu_query::db::SourceFile::new(
                            &db,
                            file_id,
                            std::sync::Arc::clone(&source),
                            std::sync::Arc::new(p.clone()),
                        );
                        db.register_source_file(filepath.clone(), file);
                        file
                    }
                };
                source_files.push((source_file, filepath, source));
            }
            Err(err) => {
                fail_operational("failed to read", Some(p.clone()), err.to_string());
            }
        }
    }

    if use_parallel {
        let db_mutex = std::sync::Mutex::new(db);
        let outcomes = source_files
            .into_par_iter()
            .map(|(source_file, filepath, _source)| {
                let thread_db = match db_mutex.lock() {
                    Ok(guard) => guard.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let outcome = pipeline_lower_checked(&thread_db, source_file);
                (filepath, outcome)
            })
            .collect::<Vec<_>>();
        let db = match db_mutex.into_inner() {
            Ok(db) => db,
            Err(poisoned) => poisoned.into_inner(),
        };

        // `Vec::into_par_iter` is indexed, so collect preserves the sorted
        // path order even though analysis completes in a different order.
        for (filepath, outcome) in outcomes {
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(diagnostics) => render_pipeline_failure(&db, diagnostics, &filepath),
            };
            render_nonfatal_diagnostics(&db, &outcome.diagnostics, &filepath);
            if genref_report {
                print_genref_report(&filepath, &outcome.artifacts);
            }
            tracing::info!(
                "Compilation verified successfully — no errors found for {}",
                filepath
            );
            if !project_flags.quiet {
                println!("ok {}", filepath);
            }
        }

        if !project_flags.quiet
            && project_flags.verbose
            && let Some(log) = &rebuild_log
        {
            eprintln!("{}", log.status_line());
        }

        if explain && let Some(log) = rebuild_log {
            eprint!("{}", log.format_chain(true));
        }
        return Ok(CliSuccess::Done);
    }

    for (source_file, filepath, source) in source_files {
        run_dispatch::process_file(
            source_file,
            filepath,
            source,
            db.clone(),
            command,
            project_flags,
            explain,
            genref_report,
            debug,
            cfg,
            ascii,
            opt,
            data_layout,
            &backend,
            inv,
        );
    }
    if explain && let Some(log) = rebuild_log {
        eprint!("{}", log.format_chain(true));
    }

    Ok(CliSuccess::Done)
}

pub fn cmd_project_check(
    start: &Path,
    flags: &ProjectFlags,
    _opt: bool,
    _debug: bool,
    parallel: bool,
    genref_report: bool,
    data_layout: DataLayout,
) -> CliResult {
    let explain_rebuild = arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let (mut db, rebuild_log) = if flags.verbose || explain_rebuild {
        let (db, log) = arandu_query::DatabaseImpl::with_rebuild_log();
        (db, Some(log))
    } else {
        (arandu_query::DatabaseImpl::new(), None)
    };
    db.set_target_config(data_layout);
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };
    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    if parallel {
        let package_src = ctx
            .entry_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| ctx.root.clone());
        let mut module_files = vec![(file, filepath.clone())];
        for rel in arandu_query::scan_aru_entries(&package_src) {
            let full_path = package_src.join(&rel);
            let key = full_path.to_string_lossy().into_owned();
            if key == filepath {
                continue;
            }
            if let Some(sf) = db.source_file_by_path(&key) {
                module_files.push((sf, key));
            }
        }
        let db_mutex = std::sync::Mutex::new(db);
        let outcomes = module_files
            .into_par_iter()
            .map(|(sf, path_str)| {
                let thread_db = match db_mutex.lock() {
                    Ok(guard) => guard.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let outcome = pipeline_lower_checked(&thread_db, sf);
                (path_str, outcome)
            })
            .collect::<Vec<_>>();
        let db = match db_mutex.into_inner() {
            Ok(db) => db,
            Err(poisoned) => poisoned.into_inner(),
        };
        for (path_str, outcome) in outcomes {
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(diagnostics) => render_pipeline_failure(&db, diagnostics, &path_str),
            };
            render_nonfatal_diagnostics(&db, &outcome.diagnostics, &path_str);
            if genref_report && path_str == filepath {
                print_genref_report(&filepath, &outcome.artifacts);
            }
        }
    } else {
        let artifacts = pipeline_lower(&db, file, &filepath);
        if genref_report {
            print_genref_report(&filepath, &artifacts);
        }
    }
    if !flags.quiet
        && let Some(log) = &rebuild_log
    {
        eprintln!("{}", log.status_line());
    }
    if explain_rebuild && let Some(log) = &rebuild_log {
        eprint!("{}", log.format_chain(true));
    }
    if !flags.quiet {
        println!("ok {} ({}/{})", filepath, ctx.name, ctx.version);
    }
    Ok(CliSuccess::Done)
}
