//! Project native object build and linking.

use std::path::Path;

use super::llvm_product::{self, LlvmStop};
use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::linker;
use crate::pipeline::{
    open_entry_file, optimize_amir_or_exit, optimize_amir_with_level_or_exit, pipeline_lower,
    print_genref_report,
};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;

#[path = "build_emit.rs"]
mod build_emit;

pub(crate) fn selects_wasm_backend(target: Option<&str>, kind: project::TargetKind) -> bool {
    target.is_some_and(|triple| triple.starts_with("wasm32"))
        || kind == project::TargetKind::Component
}

fn llvm_clang_identity() -> String {
    match std::process::Command::new("clang")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| "clang-unavailable".to_owned())
        }
        _ => "clang-unavailable".to_owned(),
    }
}

pub fn cmd_project_build(
    start: &Path,
    flags: &ProjectFlags,
    opt: bool,
    debug_info: bool,
    genref_report: bool,
    data_layout: DataLayout,
    backend_choice: crate::args::RunBackend,
) -> CliResult {
    let is_llvm = backend_choice == crate::args::RunBackend::Llvm;
    let plan = llvm_product::prepare(is_llvm, flags)?;
    if flags.size && debug_info {
        return Err(CliFailure::usage(
            "`--size` cannot be combined with `--debug`: size builds strip debug information",
        ));
    }
    let backend = project::BackendChoice::from_release_flag(flags.release);
    let backend_label = if is_llvm {
        llvm_product::status_label(plan.stop)
    } else if flags.size {
        "cranelift-size"
    } else {
        backend.label()
    };
    let explain_rebuild = arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let (mut db, rebuild_log) = if flags.verbose || explain_rebuild {
        let (db, log) = arandu_query::DatabaseImpl::with_rebuild_log();
        (db, Some(log))
    } else {
        (arandu_query::DatabaseImpl::new(), None)
    };
    db.set_target_config(data_layout);
    let ctx = {
        arandu_base::time_pass!("project-load");
        match project::load_project(&mut db, start, flags) {
            Ok(c) => c,
            Err(e) => {
                return Err(CliFailure::operational(
                    "load project",
                    Some(start.into()),
                    e,
                ));
            }
        }
    };
    let profile = if flags.size {
        artifact::NativeProfile::Size
    } else if flags.release || is_llvm {
        artifact::NativeProfile::Release
    } else {
        artifact::NativeProfile::Dev
    };
    let compiler_path = std::env::current_exe().map_err(|error| {
        CliFailure::operational("locate active Arandu compiler", None, error.to_string())
    })?;
    let runtime_path = linker::runtime_library()?;
    let mut build_inputs = ctx.build_inputs.clone();
    if debug_info {
        // Line tables depend on exact byte positions. A trivia-only edit may
        // cut off semantic queries, but it must not reuse stale native DWARF.
        for input in &mut build_inputs {
            if input.semantic_source {
                input.semantic_source = false;
            }
        }
    }
    build_inputs.push(crate::incremental::IncrementalInput {
        key: "toolchain/compiler".to_owned(),
        path: compiler_path.clone(),
        semantic_source: false,
    });
    build_inputs.push(crate::incremental::IncrementalInput {
        key: "toolchain/runtime".to_owned(),
        path: runtime_path,
        semantic_source: false,
    });

    let is_wasm = selects_wasm_backend(flags.target.as_deref(), ctx.target_kind);
    let toolchain_identity = if is_llvm {
        llvm_clang_identity()
    } else {
        String::new()
    };
    if plan.embedded.is_some() {
        if data_layout.pointer_width() != 4 {
            return Err(CliFailure::usage(
                "bare-metal profiles use a 32-bit data layout",
            ));
        }
        db.set_target_config(DataLayout::ptr_width(4));
    } else if !is_wasm && data_layout != DataLayout::host() {
        return Err(CliFailure::usage(
            "native builds require the host data layout; non-host layouts are emit-only",
        ));
    }
    let wasm_triple = flags.target.as_deref().unwrap_or("wasm32-wasip1");
    if is_wasm {
        let identity =
            arandu_middle::db::TargetIdentity::from_triple(wasm_triple).map_err(|error| {
                CliFailure::operational("select compilation target", None, error.to_owned())
            })?;
        db.set_target_identity(identity);
        db.set_target_config(DataLayout::ptr_width(4));
    }

    let session_config = crate::incremental::SessionConfig {
        project_root: &ctx.root,
        ctfe_limits: flags.ctfe_limits,
        package: &ctx.name,
        version: &ctx.version,
        profile,
        backend: if is_wasm {
            if ctx.target_kind == project::TargetKind::Component {
                "wasm-component"
            } else {
                "wasm-core"
            }
        } else if is_llvm {
            llvm_product::session_backend(plan.stop)
        } else {
            "cranelift"
        },
        pointer_width: if is_wasm || plan.embedded.is_some() {
            4
        } else {
            data_layout.pointer_width()
        },
        opt,
        debug_info,
        toolchain: &toolchain_identity,
        manifest_path: &ctx.manifest_path,
        extra_inputs: &build_inputs,
        target_triple: if is_wasm {
            Some(wasm_triple)
        } else {
            plan.embedded.map(|profile| profile.triple())
        },
    };

    let check = {
        arandu_base::time_pass!("incremental-cutoff");
        crate::incremental::check_incremental(&session_config)
    };
    let reusable_input_fingerprints = match check {
        crate::incremental::IncrementalCheck::UpToDate { artifact_path } if !genref_report => {
            let backend_name = if is_wasm {
                if ctx.target_kind == project::TargetKind::Component {
                    "wasm-component"
                } else {
                    "wasm-core"
                }
            } else {
                backend_label
            };
            if !flags.quiet {
                println!(
                    "built {} v{} (backend={}, entry={}, artifact={}, incremental: up-to-date)",
                    ctx.name,
                    ctx.version,
                    backend_name,
                    ctx.entry_rel,
                    artifact_path.display()
                );
            }
            return Ok(CliSuccess::Done);
        }
        crate::incremental::IncrementalCheck::UpToDate { .. } => None,
        crate::incremental::IncrementalCheck::NeedsRebuild {
            reason,
            reusable_input_fingerprints,
        } => {
            if flags.verbose {
                eprintln!("[incremental] rebuild triggered: {reason}");
            }
            reusable_input_fingerprints
        }
    };

    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    let artifacts = pipeline_lower(&db, file, &filepath);
    if genref_report {
        print_genref_report(&filepath, &artifacts);
    }
    if !flags.quiet
        && (flags.verbose || explain_rebuild)
        && let Some(log) = &rebuild_log
    {
        eprintln!("{}", log.status_line());
    }
    if explain_rebuild && let Some(log) = &rebuild_log {
        eprint!("{}", log.format_chain(true));
    }

    let compiler_artifact_layout = if is_wasm {
        artifact::layout_for_target(&ctx.root, profile.directory(), wasm_triple)
    } else if let Some(embedded) = plan.embedded {
        artifact::layout_for_target(&ctx.root, profile.directory(), embedded.triple())
    } else {
        artifact::layout(&ctx.root, profile.directory())
    };
    {
        arandu_base::time_pass!("compiler-artifacts");
        crate::incremental::publish_compiler_artifacts(
            &db,
            file,
            &artifacts,
            &compiler_artifact_layout.incremental,
        )?;
    }

    // Dev "build" = typecheck + lower + relocatable native object emission.
    let type_check = &artifacts.type_check;
    // Debug variable locations refer to the baseline AMIR temp IDs. Keep
    // optimized non-debug builds unchanged; native `--debug` deliberately
    // favors inspectability and stable location ranges.
    let native_debug_info = debug_info && !is_wasm;
    let mut amir_owned =
        if (opt || flags.release || flags.size || is_llvm || plan.stop == LlvmStop::Header)
            && !native_debug_info
        {
            Some(artifacts.amir.clone())
        } else {
            None
        };
    if let Some(ref mut amir) = amir_owned {
        if flags.size {
            optimize_amir_with_level_or_exit(
                amir,
                type_check,
                arandu_semantics::OptLevel::Os,
                &filepath,
            );
        } else if flags.release || is_llvm {
            optimize_amir_with_level_or_exit(
                amir,
                type_check,
                arandu_semantics::OptLevel::O2,
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

    if is_wasm {
        return build_emit::emit_wasm_artifact(
            flags,
            opt,
            &ctx,
            profile,
            wasm_triple,
            &session_config,
            reusable_input_fingerprints,
            filepath,
            type_check,
            amir,
        );
    }
    if is_llvm || plan.stop == LlvmStop::Header {
        return build_emit::emit_llvm_artifact(
            flags,
            &ctx,
            profile,
            plan,
            backend_label,
            &session_config,
            reusable_input_fingerprints,
            filepath,
            type_check,
            amir,
        );
    }
    build_emit::emit_native_artifact(
        flags,
        debug_info,
        data_layout,
        backend,
        backend_label,
        &db,
        &ctx,
        profile,
        &compiler_path,
        &session_config,
        reusable_input_fingerprints,
        filepath,
        type_check,
        amir,
    )
}

#[cfg(test)]
mod tests {
    use super::selects_wasm_backend;
    use crate::project;

    #[test]
    fn wasm_selection_follows_the_triple_not_pointer_width() {
        assert!(!selects_wasm_backend(
            Some("i686-unknown-linux-gnu"),
            project::TargetKind::Bin
        ));
        assert!(selects_wasm_backend(
            Some("wasm32-wasip1"),
            project::TargetKind::Bin
        ));
        assert!(selects_wasm_backend(None, project::TargetKind::Component));
        assert!(!selects_wasm_backend(None, project::TargetKind::Bin));
    }
}
