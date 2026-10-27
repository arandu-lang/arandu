//! Host-native linker selection for project AOT builds.

use crate::artifact;
use crate::cli_error::CliFailure;
use arandu_middle::{LinkerFlavor, TargetOs, TargetSpec};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use workspace_paths::{linker_work_dir, make_relative, strip_verbatim_prefix};

#[cfg(any(windows, test))]
mod msvc_workspace;

mod fast_link;
mod workspace_paths;

#[cfg(windows)]
mod msvc_discover;

/// Linker provenance recorded alongside the final artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkerKind {
    System,
    RustcDevelopmentFallback,
    InProcessElf,
}

/// Host libraries needed when invoking Clang for LLVM-generated programs.
pub(crate) const fn host_runtime_link_args() -> &'static [&'static str] {
    TargetSpec::for_host().cc_link_args()
}

impl LinkerKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::RustcDevelopmentFallback => "rustc-development-fallback",
            Self::InProcessElf => "in-process-elf",
        }
    }
}

/// Finds the runtime library belonging to the current Arandu installation.
pub fn runtime_library() -> Result<PathBuf, CliFailure> {
    let filename = if cfg!(windows) {
        "arandu_runtime.lib"
    } else {
        "libarandu_runtime.a"
    };
    if let Some(explicit) = std::env::var_os("ARANDU_RUNTIME_LIB") {
        let explicit = PathBuf::from(explicit);
        return if explicit.is_file() {
            Ok(explicit)
        } else {
            Err(CliFailure::operational(
                "locate Arandu AOT runtime",
                Some(explicit),
                "ARANDU_RUNTIME_LIB does not name a regular file",
            ))
        };
    }
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(bin) = executable.parent()
    {
        // Cargo development layout: target/{debug,release}/arandu_cli, or a
        // test harness that itself lives in target/{debug,release}/deps.
        let profile_dir = if bin.file_name().is_some_and(|name| name == "deps") {
            bin.parent().unwrap_or(bin)
        } else {
            bin
        };
        if let Some(hashed) = hashed_development_runtime(profile_dir, filename) {
            candidates.push(hashed);
        }
        candidates.push(bin.join(filename));
        // Installed SDK layout: bin/arandu + lib/<host>/runtime.
        if let Some(prefix) = bin.parent() {
            candidates.push(
                prefix
                    .join("lib")
                    .join(artifact::host_triple())
                    .join(filename),
            );
            candidates.push(prefix.join("lib").join(filename));
        }
    }

    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .ok_or_else(|| {
            CliFailure::operational(
                "locate Arandu AOT runtime",
                None,
                format!(
                    "expected {filename}; set ARANDU_RUNTIME_LIB or reinstall the SDK (searched: {})",
                    candidates
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
}

/// Finds Cargo's hashed staticlib in a development profile directory.
///
/// Installed SDKs use the exact filename above. Cargo dependencies live under
/// `target/<profile>/deps`; selecting the most recently built matching archive
/// mirrors Cargo's incremental artifact choice and makes `cargo run -p
/// arandu_cli -- build` usable without a separate runtime build command.
fn hashed_development_runtime(profile_dir: &Path, filename: &str) -> Option<PathBuf> {
    let (prefix, extension) = if cfg!(windows) {
        ("arandu_runtime-", "lib")
    } else {
        ("libarandu_runtime-", "a")
    };
    let deps = profile_dir.join("deps");
    let mut candidates = fs::read_dir(deps)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.starts_with(prefix))
                && path.extension().and_then(|ext| ext.to_str()) == Some(extension)
                && path.file_name().and_then(|name| name.to_str()) != Some(filename)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let modified = |path: &Path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        };
        modified(left)
            .cmp(&modified(right))
            .then_with(|| left.cmp(right))
    });
    candidates.pop()
}

/// Links Cranelift objects with the target-matched Arandu runtime.
pub fn link_objects(objects: &[&Path], output: &Path) -> Result<LinkerKind, CliFailure> {
    link_objects_with_mode(objects, output, true, false)
}

fn link_objects_with_mode(
    objects: &[&Path],
    output: &Path,
    patchable_elf: bool,
    size: bool,
) -> Result<LinkerKind, CliFailure> {
    if objects.is_empty() {
        return Err(CliFailure::operational(
            "link native artifact",
            Some(output.to_path_buf()),
            "no object files provided for linking",
        ));
    }
    let runtime = runtime_library()?;
    // MSVC opens fully qualified paths even for relative command-line inputs.
    // Keep all our inputs/output short, including discovery scripts and rustc's
    // fallback stub; the artifact layer still owns final atomic publication.
    #[cfg(windows)]
    {
        let workspace = msvc_workspace::MsvcWorkspace::new(objects, &runtime)
            .map_err(|error| link_failure(output, error.to_string()))?;
        let object_refs: Vec<_> = workspace.objects.iter().map(PathBuf::as_path).collect();
        let kind = link_prepared_objects(
            &object_refs,
            &workspace.runtime,
            &workspace.output,
            patchable_elf,
            size,
        )
        .map_err(|mut failure| {
            if let CliFailure::Operational { context, .. } = &mut failure {
                *context = Some(output.to_path_buf());
            }
            failure
        })?;
        workspace
            .copy_output(output)
            .map_err(|error| link_failure(output, error.to_string()))?;
        Ok(kind)
    }
    #[cfg(not(windows))]
    link_prepared_objects(objects, &runtime, output, patchable_elf, size)
}

fn link_prepared_objects(
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    patchable_elf: bool,
    size: bool,
) -> Result<LinkerKind, CliFailure> {
    if let Some(explicit) = std::env::var_os("ARANDU_LINKER") {
        // A configured relative executable belongs to the caller, not to the
        // temporary linking directory. Bare names continue to use PATH.
        let explicit = resolve_linker_executable(explicit)
            .map_err(|error| link_failure(output, format!("resolve ARANDU_LINKER: {error}")))?;
        return match fast_link::run_system_linker(
            &explicit,
            objects,
            runtime,
            output,
            patchable_elf,
            size,
        ) {
            Ok(()) => Ok(LinkerKind::System),
            Err(LinkAttempt::NotFound) => Err(link_failure(
                output,
                format!(
                    "configured ARANDU_LINKER '{}' was not found",
                    Path::new(&explicit).display()
                ),
            )),
            Err(LinkAttempt::Failed(message)) => Err(link_failure(output, message)),
        };
    }

    for candidate in system_linker_candidates() {
        match fast_link::run_system_linker(
            &candidate,
            objects,
            runtime,
            output,
            patchable_elf,
            size,
        ) {
            Ok(()) => return Ok(LinkerKind::System),
            Err(LinkAttempt::NotFound) => {}
            // A bundled or PATH `lld` that cannot see the SDK must not hide
            // `link.exe` / the system driver. Classic linker failures stay fatal.
            Err(LinkAttempt::Failed(_)) if linker_name_is_lld(&candidate) => {}
            Err(LinkAttempt::Failed(message)) => {
                return Err(link_failure(output, message));
            }
        }
    }

    #[cfg(windows)]
    match msvc_discover::run_discovered_msvc(objects, runtime, output, size) {
        Ok(()) => return Ok(LinkerKind::System),
        Err(LinkAttempt::NotFound) => {}
        Err(LinkAttempt::Failed(message)) => return Err(link_failure(output, message)),
    }

    // This fallback keeps compiler-development checkouts testable without a
    // separately configured native linker. Public SDKs do not contain rustc;
    // their native release smoke must exercise the system path above.
    fast_link::link_with_rustc(objects, runtime, output, patchable_elf, size)?;
    Ok(LinkerKind::RustcDevelopmentFallback)
}

fn resolve_linker_executable(linker: OsString) -> std::io::Result<OsString> {
    let path = Path::new(&linker);
    if !path.is_absolute()
        && path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        // Preserve the final name: lld-link/clang-cl can be symlinks whose
        // names select driver behavior. Canonicalization would change it.
        Ok(std::path::absolute(path)?.into_os_string())
    } else {
        Ok(linker)
    }
}

/// Links a single Cranelift object with the target-matched Arandu runtime.
pub fn link(object: &Path, output: &Path) -> Result<LinkerKind, CliFailure> {
    link_objects_with_mode(&[object], output, false, false)
}

/// Link a size-profile artifact with section GC and symbol stripping.
pub fn link_size(object: &Path, output: &Path) -> Result<LinkerKind, CliFailure> {
    link_objects_with_mode(&[object], output, false, true)
}

enum LinkAttempt {
    NotFound,
    Failed(String),
}

fn system_linker_candidates() -> Vec<OsString> {
    match TargetSpec::for_host().linker_flavor() {
        LinkerFlavor::MsvcLink => {
            let mut candidates = Vec::new();
            if let Some(bundled) = bundled_lld_executable() {
                candidates.push(bundled.into_os_string());
            }
            // `lld-link` only resolves `*.lib` when the Windows SDK `LIB` path
            // is already present. Otherwise discovery below calls VsDevCmd.
            if std::env::var_os("LIB").is_some() {
                candidates.push(OsString::from("lld-link"));
            }
            candidates
        }
        LinkerFlavor::GnuCc | LinkerFlavor::DarwinCc => {
            vec![OsString::from("cc"), OsString::from("clang")]
        }
    }
}

/// `lld-link.exe` contains the text `link.exe`. Match the file name on `lld`
/// so a failed LLD attempt can fall through to the classic linker.
fn linker_name_is_lld(linker: &std::ffi::OsStr) -> bool {
    Path::new(linker)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().contains("lld"))
}

fn bundled_lld_executable() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    for name in TargetSpec::for_host().linker_plan().preferred_lld_names() {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        let with_exe = directory.join(format!("{name}.exe"));
        if with_exe.is_file() {
            return Some(with_exe);
        }
    }
    None
}
fn build_system_linker_command(
    linker: &std::ffi::OsStr,
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    patchable_elf: bool,
    size: bool,
) -> Command {
    let mut command = Command::new(linker);
    let work_dir = linker_work_dir(objects, runtime, output);
    let object_args: Vec<PathBuf> = objects
        .iter()
        .map(|obj| {
            if let Some(dir) = work_dir {
                make_relative(obj, dir)
            } else {
                strip_verbatim_prefix(obj)
            }
        })
        .collect();
    let output_arg = if let Some(dir) = work_dir {
        command.current_dir(strip_verbatim_prefix(dir));
        make_relative(output, dir)
    } else {
        strip_verbatim_prefix(output)
    };
    let runtime_arg = if let Some(dir) = work_dir {
        make_relative(runtime, dir)
    } else {
        strip_verbatim_prefix(runtime)
    };
    let spec = TargetSpec::for_host();
    match spec.linker_flavor() {
        LinkerFlavor::MsvcLink => {
            command
                .arg("/NOLOGO")
                .arg("/INCREMENTAL:NO")
                .arg("/Brepro")
                .arg("/SUBSYSTEM:CONSOLE")
                .arg(format!("/OUT:{}", output_arg.display()));
            if size {
                command.args(["/OPT:REF", "/OPT:ICF"]);
            }
            for obj in &object_args {
                command.arg(obj);
            }
            command.arg(&runtime_arg).args(spec.msvc_runtime_libs());
        }
        LinkerFlavor::DarwinCc => {
            let _ = (patchable_elf, size);
            for obj in &object_args {
                command.arg(obj);
            }
            command.arg(&runtime_arg).arg("-o").arg(&output_arg);
            command.args(["-Wl,-dead_strip", "-Wl,-x", "-Wl,-S", "-Wl,-oso_prefix,."]);
            for framework in spec.darwin_frameworks() {
                command.arg("-framework").arg(framework);
            }
            command.args(spec.native_aot_cc_libs());
            command.env("ZERO_AR_DATE", "1");
            command.env("LD_DETERMINISTIC_MODE", "YES");
        }
        LinkerFlavor::GnuCc => {
            for obj in &object_args {
                command.arg(obj);
            }
            command.arg(&runtime_arg).arg("-o").arg(&output_arg);
            if spec.os() == TargetOs::Linux {
                command.arg("-Wl,--gc-sections").arg(if patchable_elf {
                    "-Wl,--build-id=none"
                } else {
                    "-Wl,--build-id=sha1"
                });
                if size {
                    command.arg("-Wl,--strip-all");
                }
            }
            command.args(spec.native_aot_cc_libs());
        }
    }
    command
}
fn format_output(linker: impl AsRef<std::ffi::OsStr>, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!(
        "'{}' exited with {}\n{}{}",
        Path::new(linker.as_ref()).display(),
        output.status,
        stdout,
        stderr
    )
}

fn link_failure(output: &Path, message: String) -> CliFailure {
    CliFailure::operational("link native artifact", Some(output.to_path_buf()), message)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::fast_link::{FastLinker, is_compiler_driver, select_fast_linker};
    use super::*;

    #[test]
    fn lld_link_is_not_classified_as_the_classic_msvc_linker() {
        assert!(linker_name_is_lld(std::ffi::OsStr::new("lld-link.exe")));
        assert!(linker_name_is_lld(std::ffi::OsStr::new("lld-link")));
        assert!(linker_name_is_lld(std::ffi::OsStr::new(
            r"C:\LLVM\bin\lld-link.exe"
        )));
        assert!(!linker_name_is_lld(std::ffi::OsStr::new("link.exe")));
        assert!(!linker_name_is_lld(std::ffi::OsStr::new("cc")));
    }

    #[test]
    fn host_runtime_link_args_omit_libdl_on_macos() {
        let args = host_runtime_link_args();
        #[cfg(target_os = "macos")]
        assert!(!args.contains(&"-ldl"));
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(args.contains(&"-ldl"));
        #[cfg(windows)]
        {
            assert!(args.contains(&"-lbcrypt"));
            assert!(args.contains(&"-lws2_32"));
            assert!(args.contains(&"-lntdll"));
            assert!(args.contains(&"-luserenv"));
            assert!(args.contains(&"-lkernel32"));
            assert!(args.contains(&"-ldbghelp"));
        }
        #[cfg(not(windows))]
        {
            assert!(args.contains(&"-lpthread"));
            assert!(args.contains(&"-lm"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn relative_linker_preserves_its_symlink_driver_name() {
        let workspace = msvc_workspace::MsvcWorkspace::new(
            &[],
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        )
        .unwrap();
        let driver = workspace.output.with_file_name("lld-link");
        fs::write(driver.with_file_name("lld"), b"driver").unwrap();
        std::os::unix::fs::symlink("lld", &driver).unwrap();
        let relative = make_relative(&driver, &std::env::current_dir().unwrap());
        let resolved = resolve_linker_executable(relative.into_os_string()).unwrap();
        assert!(Path::new(&resolved).is_absolute());
        assert_eq!(Path::new(&resolved).file_name(), driver.file_name());
        assert_eq!(fs::read(resolved).unwrap(), b"driver");
        assert_eq!(
            resolve_linker_executable("link.exe".into()).unwrap(),
            "link.exe"
        );
    }

    #[test]
    fn verbatim_prefix_stripping() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\project\bin")),
            PathBuf::from(r"C:\project\bin")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\file.exe")),
            PathBuf::from(r"\\server\share\file.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"C:\project\bin")),
            PathBuf::from(r"C:\project\bin")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("relative/path")),
            PathBuf::from("relative/path")
        );
    }

    #[test]
    fn make_relative_handles_normal_paths() {
        let base = Path::new("/project/target/dev/cgu");
        let target = Path::new("/project/target/dev/cgu/main-1234.o");
        assert_eq!(make_relative(target, base), PathBuf::from("main-1234.o"));

        let out = Path::new("/project/target/dev/bin/.staging/app");
        assert_eq!(
            make_relative(out, base),
            PathBuf::from("../bin/.staging/app")
        );
    }

    #[test]
    fn bare_object_keeps_the_callers_working_directory() {
        let command = build_system_linker_command(
            std::ffi::OsStr::new("linker"),
            &[Path::new("artifact.o")],
            Path::new("runtime.a"),
            Path::new("artifact"),
            false,
            false,
        );
        assert_eq!(command.get_current_dir(), None);
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("artifact.o")));
        assert!(args.contains(&std::ffi::OsStr::new("runtime.a")));
    }

    #[test]
    fn relative_runtime_override_keeps_the_callers_working_directory() {
        let object = std::env::current_dir().unwrap().join("artifact.o");
        let output = object.with_extension("exe");
        let command = build_system_linker_command(
            std::ffi::OsStr::new("linker"),
            &[&object],
            Path::new("sdk/runtime.a"),
            &output,
            false,
            false,
        );
        assert_eq!(command.get_current_dir(), None);
        assert!(command.get_args().any(|arg| arg == "sdk/runtime.a"));
    }

    #[test]
    fn linker_relativizes_objects_output_and_runtime() {
        let project = std::env::temp_dir().join("project");
        let objects = project.join("target/dev/incremental/cgu");
        let object = objects.join("main.o");
        let runtime = project.join("sdk/runtime.a");
        let output = project.join("target/dev/bin/.staging/app.exe");
        let command = build_system_linker_command(
            std::ffi::OsStr::new("linker"),
            &[&object],
            &runtime,
            &output,
            false,
            false,
        );
        assert_eq!(command.get_current_dir(), Some(objects.as_path()));
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("main.o")));
        assert!(args.contains(&make_relative(&runtime, &objects).as_os_str()));
        let relative_output = make_relative(&output, &objects);
        let output_arg = match TargetSpec::for_host().linker_flavor() {
            LinkerFlavor::MsvcLink => OsString::from(format!("/OUT:{}", relative_output.display())),
            LinkerFlavor::GnuCc | LinkerFlavor::DarwinCc => relative_output.into_os_string(),
        };
        assert!(args.contains(&output_arg.as_os_str()));
    }

    #[test]
    #[cfg(windows)]
    fn make_relative_handles_verbatim_windows_paths() {
        let base = Path::new(r"\\?\C:\project\target\dev\cgu");
        let target = Path::new(r"\\?\C:\project\target\dev\cgu\main-1234.obj");
        assert_eq!(make_relative(target, base), PathBuf::from("main-1234.obj"));

        let out = Path::new(r"\\?\C:\project\target\dev\bin\.staging\app.exe");
        assert_eq!(
            make_relative(out, base),
            PathBuf::from(r"..\bin\.staging\app.exe")
        );
        assert_eq!(
            make_relative(Path::new(r"\\?\D:\sdk\runtime.lib"), base),
            PathBuf::from(r"D:\sdk\runtime.lib")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn size_link_strips_symbols_without_disabling_section_gc() {
        let command = build_system_linker_command(
            std::ffi::OsStr::new("cc"),
            &[Path::new("/tmp/artifact.o")],
            Path::new("/tmp/libarandu_runtime.a"),
            Path::new("/tmp/artifact"),
            false,
            true,
        );
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("-Wl,--gc-sections")));
        assert!(args.contains(&std::ffi::OsStr::new("-Wl,--strip-all")));
        assert!(args.contains(&std::ffi::OsStr::new("-Wl,--build-id=sha1")));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn compiler_driver_detection() {
        assert!(is_compiler_driver(std::ffi::OsStr::new("cc")));
        assert!(is_compiler_driver(std::ffi::OsStr::new("gcc")));
        assert!(is_compiler_driver(std::ffi::OsStr::new("clang")));
        assert!(is_compiler_driver(std::ffi::OsStr::new("/usr/bin/gcc-14")));
        assert!(is_compiler_driver(std::ffi::OsStr::new(
            "/usr/bin/clang-18"
        )));
        assert!(!is_compiler_driver(std::ffi::OsStr::new("ld")));
        assert!(!is_compiler_driver(std::ffi::OsStr::new("mold")));
        assert!(!is_compiler_driver(std::ffi::OsStr::new("link.exe")));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fast_linker_flags_applied_correctly() {
        let mut command = Command::new("cc");
        FastLinker::Mold.apply_to(&mut command);
        let args: Vec<_> = command.get_args().collect();
        assert!(args.iter().any(|arg| *arg == "-fuse-ld=mold"));
        assert!(args.iter().any(|arg| *arg == "-Wl,--threads=4"));

        let mut command = Command::new("cc");
        FastLinker::Lld.apply_to(&mut command);
        let args: Vec<_> = command.get_args().collect();
        assert!(args.iter().any(|arg| *arg == "-fuse-ld=lld"));
        assert!(args.iter().any(|arg| *arg == "-Wl,--threads=4"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fast_linker_search_prefers_lld_before_mold() {
        assert!(matches!(
            select_fast_linker(None, None, true, true),
            Some(FastLinker::Lld)
        ));
        assert!(matches!(
            select_fast_linker(None, Some("/opt/arandu/ld.lld".to_string()), true, true),
            Some(FastLinker::Bundled(path)) if path == "/opt/arandu/ld.lld"
        ));
        assert!(
            select_fast_linker(
                Some("none"),
                Some("/opt/arandu/ld.lld".to_string()),
                true,
                true
            )
            .is_none()
        );
        assert!(matches!(
            select_fast_linker(Some("mold"), None, true, true),
            Some(FastLinker::Mold)
        ));
        assert!(matches!(
            select_fast_linker(None, None, false, true),
            Some(FastLinker::Mold)
        ));
        assert!(select_fast_linker(None, None, false, false).is_none());
    }
}
