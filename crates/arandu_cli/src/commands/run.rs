//! Execution of single files and projects via Cranelift JIT / interpreter pipeline.

use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::pipeline::{
    attach_stdlib, ensure_host_jit_layout, fail_operational, fail_usage, find_aru_files, finish,
    open_entry_file, optimize_amir_or_exit, optimize_amir_with_level_or_exit, parse_and_check,
    pipeline_lower, pipeline_lower_checked, print_diagnostics_and_exit, print_genref_report,
    render_nonfatal_diagnostics, render_pipeline_failure, validate_hir_and_monomorphize,
};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;

pub fn cmd_project_run(
    start: &Path,
    flags: &ProjectFlags,
    opt: bool,
    _debug: bool,
    genref_report: bool,
    data_layout: DataLayout,
    program_args: &[String],
) -> CliResult {
    if flags.release || flags.size {
        return Err(CliFailure::usage(
            "`--release` and `--size` are supported by `build`; `run` remains the interactive Cranelift JIT path",
        ));
    }
    ensure_host_jit_layout(data_layout)?;
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

fn execute_jit_main(
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
    let parallel = inv.parallel;
    let debug = inv.debug;
    let opt = inv.opt;
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
                let code = std::sync::Arc::from(source.clone());
                let source_file = arandu_query::db::SourceFile::new(
                    &db,
                    file_id,
                    code,
                    std::sync::Arc::new(p.clone()),
                );
                db.register_source_file(filepath.clone(), source_file);
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

    let process_file = |source_file: arandu_query::db::SourceFile,
                        filepath: String,
                        source: String,
                        db: arandu_query::db::DatabaseImpl| {
        match command {
            "lex" => match arandu_lexer::lex_to_string(&source) {
                Ok(output) => println!("{output}"),
                Err(err) => {
                    fail_operational(
                        "lex source",
                        Some(PathBuf::from(&filepath)),
                        err.to_string(),
                    );
                }
            },
            "parse" => match arandu_parser::parse_to_string(&source) {
                Ok(output) => println!("{output}"),
                Err(err) => {
                    fail_operational(
                        "parse source",
                        Some(PathBuf::from(&filepath)),
                        err.to_string(),
                    );
                }
            },
            "check" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if !project_flags.quiet
                    && (project_flags.verbose || explain)
                    && let Some(log) = db.rebuild_log()
                {
                    eprintln!("{}", log.status_line());
                }
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                tracing::info!(
                    "Compilation verified successfully — no errors found for {}",
                    filepath
                );
                if !project_flags.quiet {
                    println!("ok {}", filepath);
                }
            }
            "hir" => {
                let mut checked = parse_and_check(&db, source_file, &filepath);
                let mut hir = {
                    arandu_base::time_pass!("lower-hir");
                    match arandu_semantics::lower_to_hir(&mut checked.type_check, &checked.program)
                    {
                        Ok(hir) => hir,
                        Err(diags) => print_diagnostics_and_exit(diags, &filepath),
                    }
                };
                validate_hir_and_monomorphize(&mut hir, &mut checked.type_check, &filepath);

                if debug {
                    println!("{hir:#?}");
                } else {
                    let ctx = arandu_semantics::hir::HirPrettyCtx {
                        pool: &hir.pool,
                        symbols: &checked.type_check.symbols,
                        show_spans: false,
                        type_interner: Some(&checked.type_check.type_info.type_interner),
                    };
                    println!("--- HIR for {} ---", filepath);
                    print!("{}", hir.pretty_print(&ctx));
                }
            }
            "amir" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                let symbols = artifacts.type_check.symbols.as_ref();
                let interner = &artifacts.type_check.type_info.type_interner;
                let mut amir_owned = if opt {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    optimize_amir_or_exit(amir, &artifacts.type_check, &filepath);
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                if cfg {
                    if ascii {
                        print!("{}", amir.render_cfg_ascii(symbols, interner));
                    } else {
                        print!("{}", amir.render_cfg_dot(symbols, interner));
                    }
                } else if debug {
                    println!("{amir:#?}");
                } else {
                    println!("--- AMIR for {} ---", filepath);
                    print!("{}", amir.pretty_print(symbols, interner));
                }
            }
            "run" => {
                if let Err(failure) = ensure_host_jit_layout(data_layout) {
                    finish(Err(failure));
                }
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                tracing::info!("AMIR lowering completed (Salsa: single pipeline)");

                if !project_flags.quiet
                    && let Some(log) = db.rebuild_log()
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
                    arandu_base::time_pass!("optimize-amir");
                    optimize_amir_or_exit(amir, type_check, &filepath);
                    tracing::info!("Optimisation passes applied");
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                use arandu_semantics::CodegenBackend;
                let output = {
                    let backend = {
                        arandu_base::time_pass!("codegen-init");
                        match arandu_backend_cranelift::CraneliftBackend::try_new() {
                            Ok(backend) => backend,
                            Err(diag) => {
                                print_diagnostics_and_exit(std::iter::once(diag), &filepath)
                            }
                        }
                    };
                    arandu_base::time_pass!("codegen-translate");
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
                tracing::info!("Machine code generated (Cranelift JIT backend)");

                let result = execute_jit_main(
                    &output,
                    amir,
                    type_check,
                    PathBuf::from(&filepath),
                    &inv.program_args,
                    "run program",
                );
                finish(result);
            }
            "emit-c" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                let type_check = &artifacts.type_check;
                let mut amir_owned = if opt || project_flags.size {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    if project_flags.size {
                        optimize_amir_with_level_or_exit(
                            amir,
                            type_check,
                            arandu_semantics::OptLevel::Os,
                            &filepath,
                        );
                    } else {
                        optimize_amir_or_exit(amir, type_check, &filepath);
                    }
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                arandu_base::time_pass!("emit-c");
                let c_src = arandu_backend_c::emit_c(
                    amir,
                    type_check.symbols.as_ref(),
                    type_check.type_info.as_ref(),
                    &type_check.type_info.type_interner,
                    data_layout,
                )
                .unwrap_or_else(|diag| {
                    print_diagnostics_and_exit(std::iter::once(diag), &filepath)
                });
                print!("{c_src}");
            }
            "emit-wasm" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                let type_check = &artifacts.type_check;
                let mut amir_owned = if opt || project_flags.size {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    if project_flags.size {
                        optimize_amir_with_level_or_exit(
                            amir,
                            type_check,
                            arandu_semantics::OptLevel::Os,
                            &filepath,
                        );
                    } else {
                        optimize_amir_or_exit(amir, type_check, &filepath);
                    }
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                // Wasm32 requires ptr_width=4; warn if a different layout was
                // requested via --layout.
                let wasm_layout = if data_layout.pointer_width() == 4 {
                    data_layout
                } else {
                    tracing::warn!(
                        "emit-wasm requires ptr_width=4 (wasm32); \
                         ignoring requested layout ({})",
                        data_layout.pointer_width()
                    );
                    arandu_middle::layout::DataLayout::ptr_width(4)
                };

                arandu_base::time_pass!("emit-wasm");
                let emit = if project_flags.size {
                    arandu_backend_wasm::emit_wasm_for_size
                } else {
                    arandu_backend_wasm::emit_wasm
                };
                let wasm_bytes = emit(
                    amir,
                    type_check.symbols.as_ref(),
                    &type_check.type_info.type_interner,
                    type_check.type_info.as_ref(),
                    wasm_layout,
                )
                .unwrap_or_else(|diag| {
                    print_diagnostics_and_exit(std::iter::once(diag), &filepath)
                });

                // Write bytes to stdout; callers can redirect to a .wasm file.
                use std::io::Write;
                std::io::stdout()
                    .write_all(&wasm_bytes)
                    .unwrap_or_else(|e| {
                        fail_operational("write wasm output", None, e.to_string());
                    });
            }
            "emit-component" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                let type_check = &artifacts.type_check;
                let mut amir_owned = if opt || project_flags.size {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    if project_flags.size {
                        optimize_amir_with_level_or_exit(
                            amir,
                            type_check,
                            arandu_semantics::OptLevel::Os,
                            &filepath,
                        );
                    } else {
                        optimize_amir_or_exit(amir, type_check, &filepath);
                    }
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                // Wasm32 requires ptr_width=4; warn if a different layout was
                // requested via --layout.
                let wasm_layout = if data_layout.pointer_width() == 4 {
                    data_layout
                } else {
                    tracing::warn!(
                        "emit-component requires ptr_width=4 (wasm32); \
                         ignoring requested layout ({})",
                        data_layout.pointer_width()
                    );
                    arandu_middle::layout::DataLayout::ptr_width(4)
                };

                // WIT package name derived from the source file stem (kebab-case).
                let pkg_name = Path::new(&filepath)
                    .file_stem()
                    .map(|s| arandu_backend_wasm::wit_gen::to_wit_ident(&s.to_string_lossy()))
                    .unwrap_or_else(|| "module".to_owned());

                arandu_base::time_pass!("emit-component");
                let emit = if project_flags.size {
                    arandu_backend_wasm::emit_component_for_size
                } else {
                    arandu_backend_wasm::emit_component
                };
                let component_bytes = emit(
                    amir,
                    type_check.symbols.as_ref(),
                    &type_check.type_info.type_interner,
                    type_check.type_info.as_ref(),
                    wasm_layout,
                    &pkg_name,
                )
                .unwrap_or_else(|diag| {
                    print_diagnostics_and_exit(std::iter::once(diag), &filepath)
                });

                // Write bytes to stdout; callers can redirect to a .wasm file.
                use std::io::Write;
                std::io::stdout()
                    .write_all(&component_bytes)
                    .unwrap_or_else(|e| {
                        fail_operational("write component output", None, e.to_string());
                    });
            }
            "graph" => {
                use arandu_query::db::ArandCompilerDb;
                let dep_graph = arandu_query::passes::module_dependency_graph(&db, source_file);
                let mut dot_graph = petgraph::Graph::<String, ()>::new();
                let mut node_map = std::collections::HashMap::new();
                for node in dep_graph.node_indices() {
                    let file_id = dep_graph[node];
                    let path = db.file_path(file_id);
                    let path_str = path.to_string_lossy().into_owned();
                    let new_node = dot_graph.add_node(path_str);
                    node_map.insert(node, new_node);
                }
                for edge in dep_graph.edge_indices() {
                    let Some((source, target)) = dep_graph.edge_endpoints(edge) else {
                        continue;
                    };
                    if let (Some(&s), Some(&t)) = (node_map.get(&source), node_map.get(&target)) {
                        dot_graph.add_edge(s, t, ());
                    }
                }
                println!("{:?}", petgraph::dot::Dot::with_config(&dot_graph, &[]));
            }
            _ => {
                fail_usage("error: unknown command");
            }
        }
    };

    for (source_file, filepath, source) in source_files {
        process_file(source_file, filepath, source, db.clone());
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
