//! Host-native linker selection for project AOT builds.

use crate::artifact;
use crate::cli_error::CliFailure;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(any(windows, test))]
mod msvc_workspace;

/// Linker provenance recorded alongside the final artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkerKind {
    System,
    RustcDevelopmentFallback,
    InProcessElf,
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
        // Cargo development layout: target/{debug,release}/arandu_cli.
        if let Some(hashed) = hashed_development_runtime(bin, filename) {
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
        return match run_system_linker(&explicit, objects, runtime, output, patchable_elf, size) {
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
        match run_system_linker(&candidate, objects, runtime, output, patchable_elf, size) {
            Ok(()) => return Ok(LinkerKind::System),
            Err(LinkAttempt::NotFound) => {}
            Err(LinkAttempt::Failed(message)) => {
                return Err(link_failure(output, message));
            }
        }
    }

    #[cfg(windows)]
    match run_discovered_msvc(objects, runtime, output, size) {
        Ok(()) => return Ok(LinkerKind::System),
        Err(LinkAttempt::NotFound) => {}
        Err(LinkAttempt::Failed(message)) => return Err(link_failure(output, message)),
    }

    // This fallback keeps compiler-development checkouts testable without a
    // separately configured native linker. Public SDKs do not contain rustc;
    // their native release smoke must exercise the system path above.
    link_with_rustc(objects, runtime, output, patchable_elf, size)?;
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
    if cfg!(windows) {
        Vec::new()
    } else {
        vec![OsString::from("cc"), OsString::from("clang")]
    }
}

#[cfg(windows)]
fn run_discovered_msvc(
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    size: bool,
) -> Result<(), LinkAttempt> {
    use std::collections::HashMap;

    let Some(program_files) = std::env::var_os("ProgramFiles(x86)") else {
        return Err(LinkAttempt::NotFound);
    };
    let vswhere = PathBuf::from(program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return Err(LinkAttempt::NotFound);
    }
    let discovery = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .map_err(|error| LinkAttempt::Failed(format!("could not run vswhere: {error}")))?;
    if !discovery.status.success() {
        return Err(LinkAttempt::Failed(format_output(&vswhere, &discovery)));
    }
    let installation = String::from_utf8_lossy(&discovery.stdout).trim().to_owned();
    if installation.is_empty() {
        return Err(LinkAttempt::NotFound);
    }
    let dev_command = strip_verbatim_prefix(
        &PathBuf::from(installation)
            .join("Common7")
            .join("Tools")
            .join("VsDevCmd.bat"),
    );
    if !dev_command.is_file() {
        return Err(LinkAttempt::NotFound);
    }
    let environment_script = strip_verbatim_prefix(&output.with_extension("msvc-env.cmd"));
    fs::write(
        &environment_script,
        format!(
            "@call \"{}\" -no_logo -arch=x64 -host_arch=x64 >nul\r\n@set\r\n",
            dev_command.display()
        ),
    )
    .map_err(|error| {
        LinkAttempt::Failed(format!(
            "could not create the temporary MSVC environment script: {error}"
        ))
    })?;
    let environment = Command::new(&environment_script).output();
    let _ = fs::remove_file(&environment_script);
    let environment = environment.map_err(|error| {
        LinkAttempt::Failed(format!(
            "could not initialize the MSVC environment: {error}"
        ))
    })?;
    if !environment.status.success() {
        return Err(LinkAttempt::Failed(format_output(
            "VsDevCmd.bat",
            &environment,
        )));
    }
    let variables = String::from_utf8_lossy(&environment.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<HashMap<_, _>>();
    let Some(path_value) = variables
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value)
    else {
        return Err(LinkAttempt::Failed(
            "VsDevCmd did not provide PATH".to_string(),
        ));
    };
    let Some(linker) = std::env::split_paths(path_value)
        .map(|directory| directory.join("link.exe"))
        .find(|candidate| candidate.is_file())
    else {
        return Err(LinkAttempt::NotFound);
    };

    let linker = strip_verbatim_prefix(&linker);
    run_system_linker_with_environment(&linker, objects, runtime, output, &variables, size)
}

#[cfg(windows)]
fn run_system_linker_with_environment(
    linker: &Path,
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    environment: &std::collections::HashMap<String, String>,
    size: bool,
) -> Result<(), LinkAttempt> {
    let mut command =
        build_system_linker_command(linker.as_os_str(), objects, runtime, output, false, size);
    command.envs(environment);
    let result = command
        .output()
        .map_err(|error| LinkAttempt::Failed(format!("could not start MSVC linker: {error}")))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(LinkAttempt::Failed(format_output(linker, &result)))
    }
}

fn build_system_linker_command(
    linker: &std::ffi::OsStr,
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    patchable_elf: bool,
    size: bool,
) -> Command {
    #[cfg(not(target_os = "linux"))]
    let _ = patchable_elf;
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = size;
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
    if cfg!(windows) {
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
        command.arg(&runtime_arg).args([
            "kernel32.lib",
            "ntdll.lib",
            "userenv.lib",
            "ws2_32.lib",
            "dbghelp.lib",
            "msvcrt.lib",
        ]);
    } else {
        for obj in &object_args {
            command.arg(obj);
        }
        command.arg(&runtime_arg).arg("-o").arg(&output_arg);
        #[cfg(target_os = "linux")]
        {
            command.arg("-Wl,--gc-sections").arg(if patchable_elf {
                "-Wl,--build-id=none"
            } else {
                "-Wl,--build-id=sha1"
            });
            if size {
                command.arg("-Wl,--strip-all");
            }
            command.args([
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-lc",
            ]);
        }
        #[cfg(target_os = "macos")]
        {
            command.args([
                "-Wl,-dead_strip",
                "-Wl,-x",
                "-Wl,-S",
                "-Wl,-oso_prefix,.",
                "-framework",
                "Security",
                "-framework",
                "CoreFoundation",
                "-liconv",
                "-lSystem",
            ]);
            command.env("ZERO_AR_DATE", "1");
            command.env("LD_DETERMINISTIC_MODE", "YES");
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        command.args(["-lpthread", "-lm", "-lc"]);
    }
    command
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
enum FastLinker {
    Mold,
    Lld,
    Custom(String),
}

#[cfg(target_os = "linux")]
impl FastLinker {
    fn apply_to(&self, command: &mut Command) {
        match self {
            Self::Mold => {
                command.arg("-fuse-ld=mold");
                command.arg("-Wl,--threads=4");
            }
            Self::Lld => {
                command.arg("-fuse-ld=lld");
                command.arg("-Wl,--threads=4");
            }
            Self::Custom(name) => {
                command.arg(format!("-fuse-ld={name}"));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn is_compiler_driver(linker: &std::ffi::OsStr) -> bool {
    let name = Path::new(linker)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    name.contains("cc") || name.contains("gcc") || name.contains("clang")
}

#[cfg(target_os = "linux")]
fn detect_fast_linker() -> Option<FastLinker> {
    if let Some(explicit) = std::env::var_os("ARANDU_USE_LD") {
        let val = explicit.to_string_lossy().trim().to_string();
        if val.is_empty() || val == "0" || val == "default" || val == "none" || val == "bfd" {
            return None;
        }
        if val.eq_ignore_ascii_case("mold") {
            return Some(FastLinker::Mold);
        }
        if val.eq_ignore_ascii_case("lld") {
            return Some(FastLinker::Lld);
        }
        return Some(FastLinker::Custom(val));
    }

    if is_executable_in_path("mold") || is_executable_in_path("ld.mold") {
        Some(FastLinker::Mold)
    } else if is_executable_in_path("lld") || is_executable_in_path("ld.lld") {
        Some(FastLinker::Lld)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn is_executable_in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&candidate)
                .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            candidate.is_file() || candidate.with_extension("exe").is_file()
        }
    })
}

fn run_system_linker(
    linker: &std::ffi::OsStr,
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    patchable_elf: bool,
    size: bool,
) -> Result<(), LinkAttempt> {
    #[cfg(target_os = "linux")]
    if is_compiler_driver(linker)
        && let Some(fast_linker) = detect_fast_linker()
    {
        let mut command =
            build_system_linker_command(linker, objects, runtime, output, patchable_elf, size);
        fast_linker.apply_to(&mut command);
        match command.output() {
            Ok(result) if result.status.success() => return Ok(()),
            Ok(_) | Err(_) => {
                // Fall back to default system linker command if fast linker invocation fails
            }
        }
    }

    let mut command =
        build_system_linker_command(linker, objects, runtime, output, patchable_elf, size);
    match command.output() {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => Err(LinkAttempt::Failed(format_output(linker, &result))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(LinkAttempt::NotFound),
        Err(error) => Err(LinkAttempt::Failed(format!(
            "could not start '{}': {error}",
            Path::new(linker).display()
        ))),
    }
}

fn link_with_rustc(
    objects: &[&Path],
    runtime: &Path,
    output: &Path,
    patchable_elf: bool,
    size: bool,
) -> Result<(), CliFailure> {
    let stub = output.with_extension("link.rs");
    fs::write(&stub, "#![no_main]\n").map_err(|error| {
        CliFailure::operational(
            "create development linker stub",
            Some(stub.clone()),
            error.to_string(),
        )
    })?;
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
    let (stub_arg, output_arg) = if let Some(dir) = work_dir {
        (make_relative(&stub, dir), make_relative(output, dir))
    } else {
        (strip_verbatim_prefix(&stub), strip_verbatim_prefix(output))
    };
    let runtime_arg = if let Some(dir) = work_dir {
        make_relative(runtime, dir)
    } else {
        strip_verbatim_prefix(runtime)
    };
    let mut command = Command::new("rustc");
    if let Some(dir) = work_dir {
        command.current_dir(strip_verbatim_prefix(dir));
    }
    command
        .args(["--crate-name", "arandu_link", "--edition", "2024"])
        .arg(&stub_arg)
        .arg("-o")
        .arg(&output_arg);
    for obj_arg in &object_args {
        command
            .arg("-C")
            .arg(format!("link-arg={}", obj_arg.display()));
    }
    command
        .arg("-C")
        .arg(format!("link-arg={}", runtime_arg.display()))
        .args(rustc_reproducible_link_args(patchable_elf));
    if size {
        command.args(["-C", "strip=symbols"]);
    }
    #[cfg(target_os = "macos")]
    command.env("ZERO_AR_DATE", "1");
    #[cfg(target_os = "macos")]
    command.env("LD_DETERMINISTIC_MODE", "YES");
    let result = command.output();
    let _ = fs::remove_file(&stub);
    match result {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => Err(link_failure(output, format_output("rustc", &result))),
        Err(error) => Err(CliFailure::operational(
            "link native artifact",
            Some(output.to_path_buf()),
            format!(
                "no supported system linker was found and the checkout-only rustc fallback failed: {error}"
            ),
        )),
    }
}

fn rustc_reproducible_link_args(patchable_elf: bool) -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["-C", "link-arg=/Brepro"]
    } else if cfg!(target_os = "macos") {
        vec![
            "-C",
            "link-arg=-Wl,-dead_strip",
            "-C",
            "link-arg=-Wl,-x",
            "-C",
            "link-arg=-Wl,-S",
            "-C",
            "link-arg=-Wl,-oso_prefix,.",
        ]
    } else {
        vec![
            "-C",
            if patchable_elf {
                "link-arg=-Wl,--build-id=none"
            } else {
                "link-arg=-Wl,--build-id=sha1"
            },
        ]
    }
}

// Only rebase absolute inputs. Relative paths (including an explicit runtime
// override) belong to the caller's directory and must keep that meaning.
fn linker_work_dir<'a>(objects: &[&'a Path], runtime: &Path, output: &Path) -> Option<&'a Path> {
    if !runtime.is_absolute()
        || !output.is_absolute()
        || objects.iter().any(|obj| !obj.is_absolute())
    {
        return None;
    }
    objects
        .first()
        .and_then(|obj| obj.parent())
        .filter(|dir| !dir.as_os_str().is_empty())
}

fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn make_relative(target: &Path, base: &Path) -> PathBuf {
    let target = strip_verbatim_prefix(target);
    let base = strip_verbatim_prefix(base);
    let target_components: Vec<_> = target.components().collect();
    let base_components: Vec<_> = base.components().collect();
    let mut common = 0;
    while common < target_components.len()
        && common < base_components.len()
        && target_components[common] == base_components[common]
    {
        common += 1;
    }
    if common == 0 {
        return target;
    }
    let mut result = PathBuf::new();
    for _ in common..base_components.len() {
        result.push("..");
    }
    for component in &target_components[common..] {
        result.push(component.as_os_str());
    }
    result
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
    use super::*;

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
        let output_arg = if cfg!(windows) {
            OsString::from(format!("/OUT:{}", relative_output.display()))
        } else {
            relative_output.into_os_string()
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
}
