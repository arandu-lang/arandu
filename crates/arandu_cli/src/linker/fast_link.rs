use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use arandu_middle::{LinkerFlavor, TargetSpec};

use crate::cli_error::CliFailure;

#[cfg(target_os = "linux")]
use super::bundled_lld_executable;
use super::{
    LinkAttempt, build_system_linker_command, format_output, link_failure, linker_work_dir,
    make_relative, strip_verbatim_prefix,
};

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
pub(super) enum FastLinker {
    Mold,
    Lld,
    Bundled(String),
    Custom(String),
}

#[cfg(target_os = "linux")]
impl FastLinker {
    pub(super) fn apply_to(&self, command: &mut Command) {
        match self {
            Self::Mold => {
                command.arg("-fuse-ld=mold");
                command.arg("-Wl,--threads=4");
            }
            Self::Lld => {
                command.arg(TargetSpec::for_host().linker_plan().cc_fuse_lld_flag());
                command.arg("-Wl,--threads=4");
            }
            Self::Bundled(path) => {
                command.arg(format!("-fuse-ld={path}"));
                command.arg("-Wl,--threads=4");
            }
            Self::Custom(name) => {
                command.arg(format!("-fuse-ld={name}"));
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn is_compiler_driver(linker: &std::ffi::OsStr) -> bool {
    let name = Path::new(linker)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    name.contains("cc") || name.contains("gcc") || name.contains("clang")
}

#[cfg(target_os = "linux")]
fn detect_fast_linker() -> Option<FastLinker> {
    let explicit =
        std::env::var_os("ARANDU_USE_LD").map(|value| value.to_string_lossy().trim().to_string());
    let bundled = bundled_lld_executable().map(|path| path.to_string_lossy().into_owned());
    let path_has_lld = is_executable_in_path("ld.lld") || is_executable_in_path("lld");
    let path_has_mold = is_executable_in_path("mold") || is_executable_in_path("ld.mold");
    select_fast_linker(explicit.as_deref(), bundled, path_has_lld, path_has_mold)
}

#[cfg(target_os = "linux")]
pub(super) fn select_fast_linker(
    explicit: Option<&str>,
    bundled: Option<String>,
    path_has_lld: bool,
    path_has_mold: bool,
) -> Option<FastLinker> {
    if let Some(value) = explicit {
        if value.is_empty()
            || value == "0"
            || value.eq_ignore_ascii_case("default")
            || value.eq_ignore_ascii_case("none")
            || value.eq_ignore_ascii_case("bfd")
        {
            return None;
        }
        if value.eq_ignore_ascii_case("mold") {
            return Some(FastLinker::Mold);
        }
        if value.eq_ignore_ascii_case("lld") {
            return Some(FastLinker::Lld);
        }
        return Some(FastLinker::Custom(value.to_string()));
    }
    if let Some(path) = bundled {
        return Some(FastLinker::Bundled(path));
    }
    if path_has_lld {
        return Some(FastLinker::Lld);
    }
    if path_has_mold {
        return Some(FastLinker::Mold);
    }
    None
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

pub(super) fn run_system_linker(
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

pub(super) fn link_with_rustc(
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
        .args(TargetSpec::for_host().msvc_rustc_link_args())
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
    match TargetSpec::for_host().linker_flavor() {
        LinkerFlavor::MsvcLink => vec!["-C", "link-arg=/Brepro"],
        LinkerFlavor::DarwinCc => vec![
            "-C",
            "link-arg=-Wl,-dead_strip",
            "-C",
            "link-arg=-Wl,-x",
            "-C",
            "link-arg=-Wl,-S",
            "-C",
            "link-arg=-Wl,-oso_prefix,.",
        ],
        LinkerFlavor::GnuCc => vec![
            "-C",
            if patchable_elf {
                "link-arg=-Wl,--build-id=none"
            } else {
                "link-arg=-Wl,--build-id=sha1"
            },
        ],
    }
}
