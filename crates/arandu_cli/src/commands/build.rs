//! Project native object build and linking.

use std::fs;
use std::path::Path;

use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::linker;
use crate::pipeline::{
    open_entry_file, optimize_amir_or_exit, optimize_amir_with_level_or_exit, pipeline_lower,
    print_diagnostics_and_exit, print_genref_report,
};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;
use arandu_query::ArandCompilerDb;

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
    if backend_choice == crate::args::RunBackend::Llvm && flags.target.is_some() {
        return Err(CliFailure::usage(
            "experimental LLVM build supports only the host target",
        ));
    }
    if flags.size && debug_info {
        return Err(CliFailure::usage(
            "`--size` cannot be combined with `--debug`: size builds strip debug information",
        ));
    }
    let is_llvm = backend_choice == crate::args::RunBackend::Llvm;
    let backend = project::BackendChoice::from_release_flag(flags.release);
    let backend_label = if is_llvm {
        "llvm-release"
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
    if !is_wasm && data_layout != DataLayout::host() {
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
            "llvm"
        } else {
            "cranelift"
        },
        pointer_width: if is_wasm {
            4
        } else {
            data_layout.pointer_width()
        },
        opt,
        debug_info,
        toolchain: &toolchain_identity,
        manifest_path: &ctx.manifest_path,
        extra_inputs: &build_inputs,
        target_triple: if is_wasm { Some(wasm_triple) } else { None },
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
    let mut amir_owned = if (opt || flags.release || flags.size || is_llvm) && !native_debug_info {
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
        let is_component = ctx.target_kind == project::TargetKind::Component
            || wasm_triple.contains("wasi")
            || wasm_triple == "wasm32";
        let wasm_layout = arandu_middle::layout::DataLayout::ptr_width(4);
        let wasm_bytes = if is_component {
            let pkg_name = arandu_backend_wasm::wit_gen::to_wit_ident(&ctx.name);
            let emit = if flags.size {
                arandu_backend_wasm::emit_component_for_size
            } else {
                arandu_backend_wasm::emit_component
            };
            emit(
                amir,
                type_check.symbols.as_ref(),
                &type_check.type_info.type_interner,
                type_check.type_info.as_ref(),
                wasm_layout,
                &pkg_name,
            )
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath))
        } else {
            let emit = if flags.size {
                arandu_backend_wasm::emit_wasm_for_size
            } else {
                arandu_backend_wasm::emit_wasm
            };
            emit(
                amir,
                type_check.symbols.as_ref(),
                &type_check.type_info.type_interner,
                type_check.type_info.as_ref(),
                wasm_layout,
            )
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath))
        };

        let optimized_wasm = crate::wasm_opt::optimize_wasm_if_available(
            wasm_bytes,
            opt && !is_component,
            flags.release && !is_component,
            flags.size && !is_component,
            flags.verbose,
        )?;

        let artifact = artifact::publish_wasm_artifact(
            &ctx.root,
            &ctx.name,
            &ctx.version,
            profile,
            wasm_triple,
            &optimized_wasm,
        )?;

        {
            arandu_base::time_pass!("incremental-record");
            crate::incremental::record_session(
                &session_config,
                &artifact.path,
                Some(artifact.digest),
                reusable_input_fingerprints,
            )?;
        }

        let backend_label = if is_component {
            "wasm-component"
        } else {
            "wasm-core"
        };
        if !flags.quiet {
            println!(
                "built {} v{} (backend={}, entry={}, artifact={})",
                ctx.name,
                ctx.version,
                backend_label,
                ctx.entry_rel,
                artifact.path.display()
            );
        }
        return Ok(CliSuccess::Done);
    }

    if is_llvm {
        arandu_base::time_pass!("codegen-llvm");
        let ir = arandu_backend_llvm::emit_llvm_ir(
            amir,
            type_check.symbols.as_ref(),
            type_check.type_info.as_ref(),
        )
        .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath));

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let temp_ll =
            std::env::temp_dir().join(format!("arandu-build-{}-{unique}.ll", std::process::id()));
        let temp_obj =
            std::env::temp_dir().join(format!("arandu-build-{}-{unique}.o", std::process::id()));

        if let Err(error) = fs::write(&temp_ll, &ir) {
            return Err(CliFailure::operational(
                "build native binary via clang",
                Some(temp_ll),
                format!("failed to write temporary LLVM IR: {error}"),
            ));
        }
        // Keep a copy under the package target for inspection (`emit-llvm` is
        // file-scoped and cannot resolve `self.*` package modules).
        let ir_dump = ctx.root.join("target").join("llvm-ir").join(format!(
            "{}-{}.ll",
            ctx.name,
            std::process::id()
        ));
        if let Some(parent) = ir_dump.parent() {
            let _ = fs::create_dir_all(parent);
            let _ = fs::write(&ir_dump, &ir);
        }

        let mut compile = std::process::Command::new("clang");
        compile.args(["-c", "-O3"]);
        compile.arg(&temp_ll).arg("-o").arg(&temp_obj);
        let compile_status = compile.status().map_err(|error| {
            let _ = fs::remove_file(&temp_ll);
            CliFailure::operational(
                "build native binary via clang",
                Some(temp_ll.clone()),
                format!("failed to start clang: {error}"),
            )
        })?;
        if !compile_status.success() {
            let _ = fs::remove_file(&temp_obj);
            // Keep the IR next to the failure for `llvm-as` / manual inspection.
            let dump = std::env::temp_dir().join(format!(
                "arandu-llvm-failed-{}-{unique}.ll",
                std::process::id()
            ));
            let _ = fs::rename(&temp_ll, &dump);
            return Err(CliFailure::operational(
                "build native binary via clang",
                Some(filepath.into()),
                format!(
                    "clang failed to optimize and assemble the LLVM IR module (IR dumped to {})",
                    dump.display()
                ),
            ));
        }
        let _ = fs::remove_file(&temp_ll);

        let object_bytes = fs::read(&temp_obj).map_err(|error| {
            let _ = fs::remove_file(&temp_obj);
            CliFailure::operational(
                "build native binary via clang",
                Some(temp_obj.clone()),
                error.to_string(),
            )
        })?;
        let _ = fs::remove_file(&temp_obj);

        let artifact = {
            arandu_base::time_pass!("native-artifact");
            artifact::publish_native_artifact(
                &ctx.root,
                &ctx.name,
                &ctx.version,
                profile,
                &object_bytes,
                |object, output| {
                    let mut command = std::process::Command::new("clang");
                    command.arg(object);
                    let runtime = linker::runtime_library()?;
                    command.arg(runtime);
                    command.args(linker::host_runtime_link_args());
                    command.arg("-o").arg(output);
                    let status = command.status().map_err(|error| {
                        CliFailure::operational(
                            "link native artifact",
                            Some(output.to_path_buf()),
                            format!("failed to start clang: {error}"),
                        )
                    })?;
                    if !status.success() {
                        return Err(CliFailure::operational(
                            "link native artifact",
                            Some(output.to_path_buf()),
                            "clang failed to compile or link native executable",
                        ));
                    }
                    Ok("clang-llvm-O3")
                },
            )?
        };

        {
            arandu_base::time_pass!("incremental-record");
            crate::incremental::record_session(
                &session_config,
                &artifact.path,
                Some(artifact.digest),
                reusable_input_fingerprints,
            )?;
        }

        if !flags.quiet {
            println!(
                "built {} v{} (backend={}, entry={}, artifact={})",
                ctx.name,
                ctx.version,
                backend_label,
                ctx.entry_rel,
                artifact.path.display()
            );
        }
        return Ok(CliSuccess::Done);
    }

    let toolchain_fingerprint = {
        arandu_base::time_pass!("toolchain-fingerprint");
        if let Some(fingerprint) = reusable_input_fingerprints
            .as_ref()
            .and_then(|inputs| inputs.get("toolchain/compiler"))
        {
            std::borrow::Cow::Borrowed(fingerprint.content_hash.as_str())
        } else {
            std::borrow::Cow::Owned(crate::incremental::content_digest(&compiler_path)?)
        }
    };

    let (target, optimization) = {
        let Some(target) =
            arandu_backend_cranelift::aot_triple_for_pointer_width(data_layout.pointer_width())
        else {
            return Err(CliFailure::operational(
                "select AOT target",
                Some(format!("pointer-width={}", data_layout.pointer_width()).into()),
                format!(
                    "no Cranelift AOT target for pointer width {} (host is {}-bit); \
                     32-bit Cranelift emission is unsupported — use a layout matching \
                     the host or the C backend",
                    data_layout.pointer_width(),
                    std::mem::size_of::<usize>() * 8,
                ),
            ));
        };
        let optimization = match backend {
            _ if debug_info => arandu_backend_cranelift::AotOptimization::Baseline,
            _ if flags.size => arandu_backend_cranelift::AotOptimization::Size,
            project::BackendChoice::CraneliftDev => {
                arandu_backend_cranelift::AotOptimization::Baseline
            }
            project::BackendChoice::CraneliftRelease => {
                arandu_backend_cranelift::AotOptimization::Speed
            }
        };
        (target, optimization)
    };

    if profile == artifact::NativeProfile::Dev && !debug_info {
        let layout = artifact::layout(&ctx.root, profile.directory());
        let cgu_cache_dir = layout.incremental.join("cgu");
        let result = {
            arandu_base::time_pass!("cgu-codegen");
            crate::cgu::compile_partitioned(
                amir,
                type_check.symbols.as_ref(),
                type_check.type_info.as_ref(),
                &target,
                optimization,
                toolchain_fingerprint.as_ref(),
                &cgu_cache_dir,
            )?
        };
        if flags.verbose {
            eprintln!(
                "[cgu] {} units: {} cached, {} recompiled",
                result.object_files.len(),
                result.cached_count,
                result.recompiled_count
            );
        }
        let elf_layout_file = layout.incremental.join("elf_layout.json");
        let (artifact, linker_label) = {
            arandu_base::time_pass!("native-artifact");
            let existing_artifact = if result.recompiled_units.is_empty() {
                artifact::current_native_artifact(&ctx.root, profile)
            } else {
                artifact::current_native_artifact_candidate(&ctx.root, profile)
            };
            let full_link = || -> Result<_, CliFailure> {
                let published = artifact::publish_partitioned_native_artifact(
                    &ctx.root,
                    &ctx.name,
                    &ctx.version,
                    profile,
                    &result.object_files,
                    |objects, output| {
                        linker::link_objects(objects, output).map(|kind| kind.label())
                    },
                )?;
                crate::linker_elf::record_elf_layout(
                    &published.path,
                    &result.object_files,
                    &elf_layout_file,
                )?;
                Ok((published, backend_label))
            };

            if result.recompiled_units.is_empty()
                && crate::linker_elf::layout_matches_cgus(&elf_layout_file, &result.units)
                && let Some(existing) = existing_artifact
            {
                if flags.verbose {
                    eprintln!(
                        "[incremental-artifact] all CGUs are verified cache hits; reusing {}",
                        existing.path.display()
                    );
                }
                (existing, "incremental-reuse")
            } else if !result.recompiled_units.is_empty()
                && let Some(existing) = existing_artifact
                && elf_layout_file.is_file()
            {
                let staging_dir = layout.bin.join(".staging");
                let _ = fs::create_dir_all(&staging_dir);
                let staging_path = staging_dir.join(format!("patch-{}.tmp", std::process::id()));
                let mut in_place_patched = None;
                if fs::copy(&existing.path, &staging_path).is_ok() {
                    let recompiled_refs: Vec<(&arandu_backend_cranelift::CodegenUnit, &[u8])> =
                        result
                            .recompiled_units
                            .iter()
                            .map(|(unit, bytes)| (unit, bytes.as_slice()))
                            .collect();
                    match crate::linker_elf::try_patch_elf_in_place(
                        &staging_path,
                        &elf_layout_file,
                        &result.units,
                        &recompiled_refs,
                    ) {
                        Ok(Some(new_digest)) => {
                            in_place_patched = Some((staging_path, new_digest));
                        }
                        Ok(None) => {
                            if flags.verbose {
                                eprintln!(
                                    "[in-process-elf] safety/determinism precondition not met; falling back to a full link"
                                );
                            }
                            let _ = fs::remove_file(&staging_path);
                        }
                        Err(error) => {
                            if flags.verbose {
                                eprintln!(
                                    "[in-process-elf] patch failed safely ({error:?}); falling back to a full link"
                                );
                            }
                            let _ = fs::remove_file(&staging_path);
                        }
                    }
                }

                if let Some((exec_path, new_digest)) = in_place_patched {
                    if flags.verbose {
                        eprintln!(
                            "[in-process-elf] patched {} CGU(s) in-place in {}",
                            result.recompiled_count,
                            exec_path.display()
                        );
                    }
                    let published = artifact::record_patched_native_artifact(
                        &ctx.root,
                        &ctx.name,
                        &ctx.version,
                        profile,
                        &result.object_files,
                        &exec_path,
                        &new_digest,
                    )?;
                    (published, crate::linker::LinkerKind::InProcessElf.label())
                } else {
                    full_link()?
                }
            } else {
                full_link()?
            }
        };

        {
            arandu_base::time_pass!("incremental-record");
            crate::incremental::record_session(
                &session_config,
                &artifact.path,
                Some(artifact.digest),
                reusable_input_fingerprints,
            )?;
        }
        if !flags.quiet {
            println!(
                "built {} v{} (backend={}, entry={}, artifact={})",
                ctx.name,
                ctx.version,
                linker_label,
                ctx.entry_rel,
                artifact.path.display()
            );
        }
        return Ok(CliSuccess::Done);
    }

    let backend_impl =
        arandu_backend_cranelift::CraneliftObjectBackend::for_target_with_optimization(
            target,
            optimization,
        );
    let backend_impl = match backend_impl {
        Ok(b) => b,
        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
    };
    let debug_sources = debug_info.then(|| {
        amir.funcs
            .iter()
            .map(|function| function.symbol.file_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter_map(|file_id| {
                db.source_file_by_id(file_id)?;
                Some(arandu_backend_cranelift::DebugSource {
                    file_id,
                    path: db.file_path(file_id),
                    text: db.source_text(file_id),
                })
            })
            .collect::<Vec<_>>()
    });
    let object = {
        arandu_base::time_pass!("codegen-monolithic");
        match &debug_sources {
            Some(sources) => backend_impl.compile_with_debug_sources(
                amir,
                type_check.symbols.as_ref(),
                type_check.type_info.as_ref(),
                sources,
            ),
            None => backend_impl.compile(
                amir,
                type_check.symbols.as_ref(),
                type_check.type_info.as_ref(),
            ),
        }
    };
    match object {
        Ok(object) => {
            let artifact = {
                arandu_base::time_pass!("native-artifact");
                artifact::publish_native_artifact(
                    &ctx.root,
                    &ctx.name,
                    &ctx.version,
                    profile,
                    object.bytes(),
                    |object, output| {
                        let link = if flags.size {
                            linker::link_size
                        } else {
                            linker::link
                        };
                        link(object, output).map(|kind| kind.label())
                    },
                )?
            };
            {
                arandu_base::time_pass!("incremental-record");
                crate::incremental::record_session(
                    &session_config,
                    &artifact.path,
                    Some(artifact.digest),
                    reusable_input_fingerprints,
                )?;
            }
            if !flags.quiet {
                println!(
                    "built {} v{} (backend={}, entry={}, artifact={})",
                    ctx.name,
                    ctx.version,
                    backend_label,
                    ctx.entry_rel,
                    artifact.path.display()
                );
            }
            Ok(CliSuccess::Done)
        }
        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
    }
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
