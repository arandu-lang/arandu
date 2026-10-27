//! Project native object build and linking.

use std::path::{Path, PathBuf};

use super::llvm_product::{self, LlvmStop};
use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::linker;
use crate::pipeline::{
    open_entry_file, pipeline_lower, print_genref_report, product_amir_or_exit, product_opt_level,
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
    BuildSession::open(
        start,
        BuildRequest {
            flags: flags.clone(),
            opt,
            debug_info,
            genref_report,
            data_layout,
            backend_choice,
        },
    )?
    .build()
}

/// Everything one `arandu build` invocation asks for.
#[derive(Debug, Clone)]
pub(crate) struct BuildRequest {
    pub flags: ProjectFlags,
    pub opt: bool,
    pub debug_info: bool,
    pub genref_report: bool,
    pub data_layout: DataLayout,
    pub backend_choice: crate::args::RunBackend,
}

/// One project database kept alive across builds of a single request (HOT.2).
///
/// Opening configures the target inputs and loads the project once; target
/// setters always invalidate their dependents, so they never run per build.
/// Each [`Self::build`] first refreshes the sources whose text changed on
/// disk, then reuses every memo the edit did not invalidate. Program errors
/// still terminate the process, as in the batch command.
pub(crate) struct BuildSession {
    start: PathBuf,
    request: BuildRequest,
    db: arandu_query::DatabaseImpl,
    rebuild_log: Option<std::sync::Arc<arandu_query::RebuildLog>>,
    explain_rebuild: bool,
    ctx: project::ProjectContext,
    plan: llvm_product::LlvmPlan,
    is_llvm: bool,
    is_wasm: bool,
    backend: project::BackendChoice,
    backend_label: &'static str,
    profile: artifact::NativeProfile,
    wasm_triple: String,
    toolchain_identity: String,
    /// Package source files and the manifest bytes seen when opening. A
    /// different file set or manifest reopens the session.
    package_sources: Vec<String>,
    manifest_digest: Option<blake3::Hash>,
}

impl BuildSession {
    pub(crate) fn open(start: &Path, request: BuildRequest) -> Result<Self, CliFailure> {
        let flags = &request.flags;
        let data_layout = request.data_layout;
        let is_llvm = request.backend_choice == crate::args::RunBackend::Llvm;
        let plan = llvm_product::prepare(is_llvm, flags)?;
        if flags.size && request.debug_info {
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
        let explain_rebuild =
            arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
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
        let wasm_triple = flags
            .target
            .as_deref()
            .unwrap_or("wasm32-wasip1")
            .to_owned();
        if is_wasm {
            let identity =
                arandu_middle::db::TargetIdentity::from_triple(&wasm_triple).map_err(|error| {
                    CliFailure::operational("select compilation target", None, error.to_owned())
                })?;
            db.set_target_identity(identity);
            db.set_target_config(DataLayout::ptr_width(4));
        }
        let package_sources = package_sources(&ctx);
        let manifest_digest = manifest_digest(&ctx);
        Ok(Self {
            start: start.to_path_buf(),
            request,
            db,
            rebuild_log,
            explain_rebuild,
            ctx,
            plan,
            is_llvm,
            is_wasm,
            backend,
            backend_label,
            profile,
            wasm_triple,
            toolchain_identity,
            package_sources,
            manifest_digest,
        })
    }

    /// Brings the database up to date with the sources on disk. A changed
    /// set of package files or manifest reopens the session; otherwise only
    /// files whose text differs are written, so unchanged inputs keep their
    /// memos.
    fn refresh(&mut self) -> Result<(), CliFailure> {
        if package_sources(&self.ctx) != self.package_sources
            || manifest_digest(&self.ctx) != self.manifest_digest
        {
            *self = Self::open(&self.start, self.request.clone())?;
            return Ok(());
        }
        for input in &self.ctx.build_inputs {
            let key = input.path.to_string_lossy();
            let Some(file) = self.db.source_file_by_path(&key) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&input.path) else {
                // A vanished source is a changed file set.
                *self = Self::open(&self.start, self.request.clone())?;
                return Ok(());
            };
            if file.text(&self.db).as_ref() != text.as_str() {
                self.db.update_file_text(file, text);
            }
        }
        Ok(())
    }

    pub(crate) fn build(&mut self) -> CliResult {
        self.refresh()?;
        let Self {
            request,
            db,
            rebuild_log,
            explain_rebuild,
            ctx,
            plan,
            is_llvm,
            is_wasm,
            backend,
            backend_label,
            profile,
            wasm_triple,
            toolchain_identity,
            ..
        } = self;
        let (plan, is_llvm, is_wasm, backend, backend_label, profile, explain_rebuild) = (
            *plan,
            *is_llvm,
            *is_wasm,
            *backend,
            *backend_label,
            *profile,
            *explain_rebuild,
        );
        let flags = &request.flags;
        let (opt, debug_info, genref_report, data_layout) = (
            request.opt,
            request.debug_info,
            request.genref_report,
            request.data_layout,
        );
        let db: &arandu_query::DatabaseImpl = db;
        let wasm_triple = wasm_triple.as_str();
        let toolchain_identity = toolchain_identity.as_str();
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
            toolchain: toolchain_identity,
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
        let (file, filepath) = open_entry_file(db, &mut registry, &ctx.entry_path);
        let artifacts = pipeline_lower(db, file, &filepath);
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
                db,
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
        let level = product_opt_level(opt || plan.stop == LlvmStop::Header, flags, is_llvm);
        let amir_owned = if native_debug_info {
            None
        } else {
            product_amir_or_exit(&artifacts.amir, type_check, level, &filepath)
        };
        let amir = match &amir_owned {
            Some(a) => a,
            None => &artifacts.amir,
        };

        if is_wasm {
            return build_emit::emit_wasm_artifact(
                flags,
                opt,
                ctx,
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
                ctx,
                profile,
                plan,
                backend_label,
                &session_config,
                reusable_input_fingerprints,
                filepath,
                type_check,
                amir,
                &artifacts.docs,
            );
        }
        build_emit::emit_native_artifact(
            flags,
            debug_info,
            data_layout,
            backend,
            backend_label,
            db,
            ctx,
            profile,
            &compiler_path,
            &session_config,
            reusable_input_fingerprints,
            filepath,
            type_check,
            amir,
        )
    }
}

fn package_sources(ctx: &project::ProjectContext) -> Vec<String> {
    ctx.entry_path
        .parent()
        .map(arandu_query::scan_aru_entries)
        .unwrap_or_default()
}

fn manifest_digest(ctx: &project::ProjectContext) -> Option<blake3::Hash> {
    std::fs::read(&ctx.manifest_path)
        .ok()
        .map(|bytes| blake3::hash(&bytes))
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

    mod session {
        use super::super::{BuildRequest, BuildSession};
        use crate::project::ProjectFlags;
        use arandu_middle::layout::DataLayout;
        use std::fs;
        use std::path::{Path, PathBuf};
        use std::process::Command;

        const MANIFEST: &str = "schema = 1\n\n[package]\nname = \"app\"\nversion = \"0.0.1\"\nedition = \"2026\"\n\n[targets.bin]\nname = \"app\"\nroot = \"src/main.aru\"\n";
        const MAIN: &str = "import app.lib as lib\nimport app.user as user\n\nfunc main(): int {\n    return lib.after(1) + user.combined(2)\n}\n";
        const USER: &str = "import app.lib as lib\n\npublic func combined(value: int): int {\n    return lib.grow(value) + lib.after(value)\n}\n";

        fn library(growth: usize) -> String {
            let mut body = String::from("    let mut acc = value\n");
            for step in 1..=growth {
                body.push_str(&format!("    acc = acc + {step}\n"));
            }
            format!(
                "public func grow(value: int): int {{\n{body}    return acc\n}}\n\npublic func after(value: int): int {{\n    return value + 1\n}}\n"
            )
        }

        fn project(label: &str, library_source: &str) -> PathBuf {
            let root = std::env::temp_dir().join(format!(
                "arandu-session-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(root.join("Arandu.toml"), MANIFEST).unwrap();
            fs::write(root.join("src/main.aru"), MAIN).unwrap();
            fs::write(root.join("src/user.aru"), USER).unwrap();
            fs::write(root.join("src/lib.aru"), library_source).unwrap();
            root
        }

        fn request() -> BuildRequest {
            BuildRequest {
                flags: ProjectFlags {
                    // Records executions so the test can observe cutoff.
                    verbose: true,
                    quiet: true,
                    ..ProjectFlags::default()
                },
                opt: false,
                debug_info: false,
                genref_report: false,
                data_layout: DataLayout::host(),
                backend_choice: crate::args::RunBackend::Cranelift,
            }
        }

        fn artifact(root: &Path) -> PathBuf {
            let mut pending = vec![root.join("target/dev")];
            while let Some(directory) = pending.pop() {
                for entry in fs::read_dir(&directory).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        pending.push(path);
                    } else if path
                        .file_name()
                        .is_some_and(|name| name == "build-state.json")
                    {
                        let state: serde_json::Value =
                            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                        return path
                            .parent()
                            .unwrap()
                            .join(state["artifact"].as_str().unwrap());
                    }
                }
            }
            panic!("no build state under {}", root.display());
        }

        fn exit_code(root: &Path) -> Option<i32> {
            Command::new(artifact(root)).status().unwrap().code()
        }

        #[test]
        fn a_live_session_relowers_only_the_edited_unit_and_matches_a_cold_build() {
            let root = project("live", &library(0));
            let mut session = BuildSession::open(&root, request()).unwrap();
            session.build().unwrap();
            // after(1) + grow(2) + after(2) = 2 + 2 + 3.
            assert_eq!(exit_code(&root), Some(7));

            fs::write(root.join("src/lib.aru"), library(3)).unwrap();
            let log = session
                .rebuild_log
                .clone()
                .expect("verbose sessions log rebuilds");
            log.clear();
            session.build().unwrap();
            assert_eq!(exit_code(&root), Some(13));
            // `grow` changed. `after` moved but keeps its item-local unit
            // (RFC 0042), and importers keep theirs.
            assert_eq!(
                log.count_executions_matching("instance_hir"),
                1,
                "{}",
                log.format_chain(false)
            );

            let cold = project("cold", &library(3));
            BuildSession::open(&cold, request())
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(
                fs::read(artifact(&root)).unwrap(),
                fs::read(artifact(&cold)).unwrap(),
                "a warm session must publish the bytes of a cold build"
            );
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(cold);
        }

        fn copy_tree(from: &Path, to: &Path) {
            fs::create_dir_all(to).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let path = entry.unwrap().path();
                let target = to.join(path.file_name().unwrap());
                if path.is_dir() {
                    copy_tree(&path, &target);
                } else {
                    fs::copy(&path, &target).unwrap();
                }
            }
        }

        #[test]
        fn a_stdlib_edit_reaches_a_live_session() {
            let root = project("stdlib", &library(0));
            let stdlib = root.join("stdlib-copy");
            copy_tree(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib"),
                &stdlib,
            );
            let target = stdlib.join("std/target.aru");
            let original = fs::read_to_string(&target).unwrap();
            let probe = |value: i32| {
                format!("{original}\npublic func sessionProbe(): int {{\n    return {value}\n}}\n")
            };
            fs::write(&target, probe(4)).unwrap();
            fs::write(
                root.join("src/main.aru"),
                "import std.target as target\n\nfunc main(): int {\n    return target.sessionProbe()\n}\n",
            )
            .unwrap();
            let mut request = request();
            request.flags.stdlib_path = Some(stdlib);
            let mut session = BuildSession::open(&root, request).unwrap();
            session.build().unwrap();
            assert_eq!(exit_code(&root), Some(4));

            fs::write(&target, probe(9)).unwrap();
            session.build().unwrap();
            assert_eq!(exit_code(&root), Some(9));
            let _ = fs::remove_dir_all(root);
        }

        #[test]
        fn a_new_package_file_reopens_the_session() {
            let root = project("reopen", &library(0));
            let mut session = BuildSession::open(&root, request()).unwrap();
            session.build().unwrap();

            fs::write(
                root.join("src/extra.aru"),
                "public func bonus(): int {\n    return 30\n}\n",
            )
            .unwrap();
            fs::write(
                root.join("src/main.aru"),
                MAIN.replace(
                    "import app.user as user\n",
                    "import app.user as user\nimport app.extra as extra\n",
                )
                .replace("user.combined(2)", "user.combined(2) + extra.bonus()"),
            )
            .unwrap();
            session.build().unwrap();
            assert_eq!(exit_code(&root), Some(37));
            let _ = fs::remove_dir_all(root);
        }
    }
}
