//! Project native object build and linking.

use std::fs;
use std::path::Path;

use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::linker;
use crate::pipeline::{
    open_entry_file, optimize_amir_or_exit, optimize_amir_with_level_or_exit, pipeline_lower,
    print_diagnostics_and_exit,
};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;

pub fn cmd_project_build(
    start: &Path,
    flags: &ProjectFlags,
    opt: bool,
    _debug: bool,
    data_layout: DataLayout,
) -> CliResult {
    let backend = project::BackendChoice::from_release_flag(flags.release);
    let (mut db, rebuild_log) = arandu_query::DatabaseImpl::with_rebuild_log();
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
    let profile = if flags.release {
        artifact::NativeProfile::Release
    } else {
        artifact::NativeProfile::Dev
    };
    let compiler_path = std::env::current_exe().map_err(|error| {
        CliFailure::operational("locate active Arandu compiler", None, error.to_string())
    })?;
    let runtime_path = linker::runtime_library()?;
    let mut build_inputs = ctx.build_inputs.clone();
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

    let is_wasm = flags
        .target
        .as_deref()
        .is_some_and(|t| t.starts_with("wasm32"))
        || ctx.target_kind == project::TargetKind::Component
        || data_layout.pointer_width() == 4;
    let wasm_triple = flags.target.as_deref().unwrap_or("wasm32-wasip1");

    let session_config = crate::incremental::SessionConfig {
        project_root: &ctx.root,
        package: &ctx.name,
        version: &ctx.version,
        profile,
        pointer_width: if is_wasm {
            4
        } else {
            data_layout.pointer_width()
        },
        opt,
        manifest_path: &ctx.manifest_path,
        extra_inputs: &build_inputs,
        target_triple: if is_wasm { Some(wasm_triple) } else { None },
    };

    let check = {
        arandu_base::time_pass!("incremental-cutoff");
        crate::incremental::check_incremental(&session_config)
    };
    let reusable_input_fingerprints = match check {
        crate::incremental::IncrementalCheck::UpToDate { artifact_path } => {
            let backend_name = if is_wasm {
                if ctx.target_kind == project::TargetKind::Component {
                    "wasm-component"
                } else {
                    "wasm-core"
                }
            } else {
                backend.label()
            };
            println!(
                "built {} v{} (backend={}, entry={}, artifact={}, incremental: up-to-date)",
                ctx.name,
                ctx.version,
                backend_name,
                ctx.entry_rel,
                artifact_path.display()
            );
            return Ok(CliSuccess::Done);
        }
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
    eprintln!("{}", rebuild_log.status_line());

    // Dev "build" = typecheck + lower + relocatable native object emission.
    let type_check = &artifacts.type_check;
    let mut amir_owned = if opt || flags.release {
        Some(artifacts.amir.clone())
    } else {
        None
    };
    if let Some(ref mut amir) = amir_owned {
        if flags.release {
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
            arandu_backend_wasm::emit_component(
                amir,
                type_check.symbols.as_ref(),
                &type_check.type_info.type_interner,
                type_check.type_info.as_ref(),
                wasm_layout,
                &pkg_name,
            )
            .unwrap_or_else(|diag| print_diagnostics_and_exit(std::iter::once(diag), &filepath))
        } else {
            arandu_backend_wasm::emit_wasm(
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
            opt,
            flags.release,
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
        println!(
            "built {} v{} (backend={}, entry={}, artifact={})",
            ctx.name,
            ctx.version,
            backend_label,
            ctx.entry_rel,
            artifact.path.display()
        );
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
            project::BackendChoice::CraneliftDev => {
                arandu_backend_cranelift::AotOptimization::Baseline
            }
            project::BackendChoice::CraneliftRelease => {
                arandu_backend_cranelift::AotOptimization::Speed
            }
        };
        (target, optimization)
    };

    if profile == artifact::NativeProfile::Dev {
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
                Ok((published, backend.label()))
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
        println!(
            "built {} v{} (backend={}, entry={}, artifact={})",
            ctx.name,
            ctx.version,
            linker_label,
            ctx.entry_rel,
            artifact.path.display()
        );
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
    let object = {
        arandu_base::time_pass!("codegen-monolithic");
        backend_impl.compile(
            amir,
            type_check.symbols.as_ref(),
            type_check.type_info.as_ref(),
        )
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
                    |object, output| linker::link(object, output).map(|kind| kind.label()),
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
            println!(
                "built {} v{} (backend={}, entry={}, artifact={})",
                ctx.name,
                ctx.version,
                backend.label(),
                ctx.entry_rel,
                artifact.path.display()
            );
            Ok(CliSuccess::Done)
        }
        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
    }
}
