use super::{execute_jit_main, execute_llvm_main};
use crate::args::RunBackend;
use crate::pipeline::{
    ensure_host_jit_layout, fail_operational, fail_usage, finish, optimize_amir_or_exit,
    optimize_amir_with_level_or_exit, parse_and_check, pipeline_lower, print_diagnostics_and_exit,
    print_genref_report, validate_hir_and_monomorphize,
};
use crate::project::ProjectFlags;
use arandu_middle::layout::DataLayout;
use std::path::{Path, PathBuf};

#[allow(clippy::too_many_arguments)]
pub(super) fn process_file(
    source_file: arandu_query::db::SourceFile,
    filepath: String,
    source: std::sync::Arc<str>,
    db: arandu_query::db::DatabaseImpl,
    command: &str,
    project_flags: &ProjectFlags,
    explain: bool,
    genref_report: bool,
    debug: bool,
    cfg: bool,
    ascii: bool,
    opt: bool,
    data_layout: DataLayout,
    backend: &RunBackend,
    inv: &crate::args::CliInvocation,
) {
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
                match arandu_semantics::lower_to_hir(&mut checked.type_check, &checked.program) {
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

            if *backend == RunBackend::Llvm {
                finish(execute_llvm_main(
                    amir,
                    type_check,
                    PathBuf::from(&filepath),
                    &inv.program_args,
                    "run program",
                ));
            }

            use arandu_semantics::CodegenBackend;
            let output = {
                let backend = {
                    arandu_base::time_pass!("codegen-init");
                    match arandu_backend_cranelift::CraneliftBackend::try_new() {
                        Ok(backend) => backend,
                        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
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
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath));
            print!("{c_src}");
        }
        "emit-llvm" => {
            let artifacts = pipeline_lower(&db, source_file, &filepath);
            if genref_report {
                print_genref_report(&filepath, &artifacts);
            }
            let type_check = &artifacts.type_check;
            let mut amir_owned = if opt {
                Some(artifacts.amir.clone())
            } else {
                None
            };
            if let Some(ref mut amir) = amir_owned {
                optimize_amir_with_level_or_exit(
                    amir,
                    type_check,
                    arandu_semantics::OptLevel::O2,
                    &filepath,
                );
            }
            let amir = amir_owned.as_ref().unwrap_or(&artifacts.amir);
            arandu_base::time_pass!("emit-llvm");
            let ir = arandu_backend_llvm::emit_llvm_ir(
                amir,
                type_check.symbols.as_ref(),
                type_check.type_info.as_ref(),
            )
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath));
            print!("{ir}");
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
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath));

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
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath));

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
}
