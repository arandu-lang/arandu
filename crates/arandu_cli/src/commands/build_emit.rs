use std::fs;
use std::path::Path;

use super::{CliFailure, CliSuccess, llvm_product};
use crate::artifact;
use crate::cli_error::CliResult;
use crate::linker;
use crate::pipeline::print_diagnostics_and_exit;
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;
use arandu_query::ArandCompilerDb;

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_wasm_artifact(
    flags: &ProjectFlags,
    opt: bool,
    ctx: &project::ProjectContext,
    profile: artifact::NativeProfile,
    wasm_triple: &str,
    session_config: &crate::incremental::SessionConfig<'_>,
    reusable_input_fingerprints: Option<
        std::collections::BTreeMap<String, crate::incremental::fingerprint::FileFingerprint>,
    >,
    filepath: String,
    type_check: &arandu_semantics::TypeCheckResult,
    amir: &arandu_semantics::amir::AmirProgram,
) -> CliResult {
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
            session_config,
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
    Ok(CliSuccess::Done)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_llvm_artifact(
    flags: &ProjectFlags,
    ctx: &project::ProjectContext,
    profile: artifact::NativeProfile,
    plan: llvm_product::LlvmPlan,
    backend_label: &str,
    session_config: &crate::incremental::SessionConfig<'_>,
    reusable_input_fingerprints: Option<
        std::collections::BTreeMap<String, crate::incremental::fingerprint::FileFingerprint>,
    >,
    filepath: String,
    type_check: &arandu_semantics::TypeCheckResult,
    amir: &arandu_semantics::amir::AmirProgram,
) -> CliResult {
    arandu_base::time_pass!("codegen-llvm");
    let ir = if plan.stop == llvm_product::LlvmStop::Header {
        arandu_backend_llvm::header::emit_c_header(
            amir,
            type_check.symbols.as_ref(),
            type_check.type_info.as_ref(),
        )
        .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath))
    } else {
        arandu_backend_llvm::emit_llvm_ir(
            amir,
            type_check.symbols.as_ref(),
            type_check.type_info.as_ref(),
        )
        .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath))
    };

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temp_ll =
        std::env::temp_dir().join(format!("arandu-build-{}-{unique}.ll", std::process::id()));
    let temp_obj =
        std::env::temp_dir().join(format!("arandu-build-{}-{unique}.o", std::process::id()));

    if plan.stop != llvm_product::LlvmStop::Executable {
        let artifact = llvm_product::publish(&plan, flags, &ctx.root, &ctx.name, profile, &ir)?;
        {
            arandu_base::time_pass!("incremental-record");
            crate::incremental::record_session(
                session_config,
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
            session_config,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_native_artifact(
    flags: &ProjectFlags,
    debug_info: bool,
    data_layout: DataLayout,
    backend: project::BackendChoice,
    backend_label: &str,
    db: &arandu_query::DatabaseImpl,
    ctx: &project::ProjectContext,
    profile: artifact::NativeProfile,
    compiler_path: &Path,
    session_config: &crate::incremental::SessionConfig<'_>,
    reusable_input_fingerprints: Option<
        std::collections::BTreeMap<String, crate::incremental::fingerprint::FileFingerprint>,
    >,
    filepath: String,
    type_check: &arandu_semantics::TypeCheckResult,
    amir: &arandu_semantics::amir::AmirProgram,
) -> CliResult {
    let toolchain_fingerprint = {
        arandu_base::time_pass!("toolchain-fingerprint");
        if let Some(fingerprint) = reusable_input_fingerprints
            .as_ref()
            .and_then(|inputs| inputs.get("toolchain/compiler"))
        {
            std::borrow::Cow::Borrowed(fingerprint.content_hash.as_str())
        } else {
            std::borrow::Cow::Owned(crate::incremental::content_digest(compiler_path)?)
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
                session_config,
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
                    session_config,
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
