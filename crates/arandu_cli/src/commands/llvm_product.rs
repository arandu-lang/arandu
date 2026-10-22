//! Freestanding LLVM products: object, staticlib, IR, or a user-scripted ELF.
//!
//! The compiler never invents a chip memory map. A final executable exists only
//! when the user passes `--linker-script`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::artifact::{self, NativeProfile, PublishedNativeArtifact};
use crate::cli_error::CliFailure;
use crate::project::ProjectFlags;
use arandu_middle::{EmbeddedProfile, embedded_profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LlvmStop {
    /// Host link with the packaged runtime. Unchanged for host LLVM builds.
    Executable,
    Ir,
    Object,
    Staticlib,
    Elf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LlvmPlan {
    pub embedded: Option<&'static EmbeddedProfile>,
    pub stop: LlvmStop,
}

pub(crate) fn prepare(is_llvm: bool, flags: &ProjectFlags) -> Result<LlvmPlan, CliFailure> {
    let triple = flags.target.as_deref();
    let embedded = triple.and_then(embedded_profile);
    let wasm = triple.is_some_and(|target| target.starts_with("wasm32"));
    if triple.is_some() {
        if embedded.is_none() && !wasm && is_llvm {
            return Err(CliFailure::usage(
                "LLVM --target accepts thumbv6m-none-eabi, thumbv7em-none-eabihf, or riscv32imc-unknown-none-elf; the compiler does not guess other chips",
            ));
        }
        if embedded.is_some() && !is_llvm {
            return Err(CliFailure::usage("bare-metal targets require -b llvm"));
        }
        if wasm && is_llvm {
            return Err(CliFailure::usage(
                "experimental LLVM build supports only the host target and the canonical bare-metal triples",
            ));
        }
    }
    let stop = classify(flags, embedded.is_some())?;
    if !is_llvm && stop != LlvmStop::Executable {
        return Err(CliFailure::usage(
            "--emit, --crate-type and --linker-script require -b llvm",
        ));
    }
    Ok(LlvmPlan { embedded, stop })
}

fn classify(flags: &ProjectFlags, embedded: bool) -> Result<LlvmStop, CliFailure> {
    let emit = flags.emit.as_deref();
    let crate_type = flags.crate_type.as_deref();
    let script = flags.linker_script.is_some();
    if emit.is_some_and(|value| value != "obj" && value != "llvm-ir") {
        return Err(CliFailure::usage("--emit accepts obj or llvm-ir"));
    }
    if crate_type.is_some_and(|value| value != "staticlib") {
        return Err(CliFailure::usage("--crate-type accepts staticlib"));
    }
    if (crate_type.is_some() || script) && !embedded {
        return Err(CliFailure::usage(
            "--crate-type=staticlib and --linker-script are bare-metal LLVM options",
        ));
    }
    match (emit, crate_type, script, embedded) {
        (Some("llvm-ir"), None, false, _) => Ok(LlvmStop::Ir),
        (Some("obj"), None, false, _) => Ok(LlvmStop::Object),
        (None, Some("staticlib"), false, true) => Ok(LlvmStop::Staticlib),
        (None, None, true, true) => Ok(LlvmStop::Elf),
        (None, None, false, true) => Ok(LlvmStop::Object),
        (None, None, false, false) => Ok(LlvmStop::Executable),
        (Some("llvm-ir"), _, _, _) => Err(CliFailure::usage(
            "--emit=llvm-ir writes IR and does not archive or link",
        )),
        (_, Some("staticlib"), true, _) => Err(CliFailure::usage(
            "--linker-script and --crate-type=staticlib select different artifacts",
        )),
        _ => Err(CliFailure::usage(
            "combine --emit=obj, --emit=llvm-ir, --crate-type=staticlib, or --linker-script as a single artifact",
        )),
    }
}

pub(crate) fn session_backend(stop: LlvmStop) -> &'static str {
    match stop {
        LlvmStop::Executable => "llvm",
        LlvmStop::Ir => "llvm-ir",
        LlvmStop::Object => "llvm-obj",
        LlvmStop::Staticlib => "llvm-staticlib",
        LlvmStop::Elf => "llvm-elf",
    }
}

pub(crate) fn status_label(stop: LlvmStop) -> &'static str {
    match stop {
        LlvmStop::Executable => "llvm-release",
        other => session_backend(other),
    }
}

pub(crate) fn publish(
    plan: &LlvmPlan,
    flags: &ProjectFlags,
    project_root: &Path,
    package: &str,
    profile: NativeProfile,
    ir: &str,
) -> Result<PublishedNativeArtifact, CliFailure> {
    let extension = match plan.stop {
        LlvmStop::Ir => "ll",
        LlvmStop::Object => "o",
        LlvmStop::Staticlib => "a",
        LlvmStop::Elf => "elf",
        LlvmStop::Executable => "o",
    };
    let output = output_path(plan, flags, project_root, package, profile, extension)?;
    if plan.stop == LlvmStop::Ir {
        return write_bytes(&output, ir.as_bytes());
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temp_ll =
        std::env::temp_dir().join(format!("arandu-build-{}-{unique}.ll", std::process::id()));
    let temp_obj =
        std::env::temp_dir().join(format!("arandu-build-{}-{unique}.o", std::process::id()));
    fs::write(&temp_ll, ir).map_err(|error| {
        CliFailure::operational(
            "write LLVM IR",
            Some(temp_ll.clone()),
            format!("failed to write temporary LLVM IR: {error}"),
        )
    })?;
    // clang canonicalizes the public short triple (`thumbv6m-none-eabi`) by
    // inserting the `unknown` vendor. Passing the canonical form keeps the
    // module header and the driver in agreement, avoiding -Woverride-module.
    let triple = plan.embedded.map(|profile| profile.llvm_triple());
    if let Err(error) = clang_compile(&temp_ll, &temp_obj, triple) {
        let _ = fs::remove_file(&temp_ll);
        let _ = fs::remove_file(&temp_obj);
        return Err(error);
    }
    let _ = fs::remove_file(&temp_ll);

    let published = match plan.stop {
        LlvmStop::Object => {
            let bytes = fs::read(&temp_obj).map_err(|error| {
                CliFailure::operational(
                    "read object file",
                    Some(temp_obj.clone()),
                    error.to_string(),
                )
            })?;
            write_bytes(&output, &bytes)
        }
        LlvmStop::Staticlib => archive(&temp_obj, &output),
        LlvmStop::Elf => link_elf(&temp_obj, &output, triple, flags),
        LlvmStop::Ir | LlvmStop::Executable => {
            return Err(CliFailure::usage(
                "host LLVM executables are published by the runtime linker",
            ));
        }
    };
    let _ = fs::remove_file(&temp_obj);
    published
}

fn output_path(
    plan: &LlvmPlan,
    flags: &ProjectFlags,
    project_root: &Path,
    package: &str,
    profile: NativeProfile,
    extension: &str,
) -> Result<PathBuf, CliFailure> {
    if let Some(path) = &flags.output {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| {
                CliFailure::operational(
                    "create output directory",
                    Some(parent.to_path_buf()),
                    error.to_string(),
                )
            })?;
        }
        return Ok(path.clone());
    }
    let layout = match plan.embedded {
        Some(embedded) => {
            artifact::layout_for_target(project_root, profile.directory(), embedded.triple())
        }
        None => artifact::layout(project_root, profile.directory()),
    };
    fs::create_dir_all(&layout.deps).map_err(|error| {
        CliFailure::operational(
            "create artifact directory",
            Some(layout.deps.clone()),
            error.to_string(),
        )
    })?;
    Ok(layout.deps.join(format!("{package}.{extension}")))
}

fn clang_compile(ir: &Path, object: &Path, triple: Option<&str>) -> Result<(), CliFailure> {
    let mut command = Command::new("clang");
    command.args(["-c", "-O3"]);
    if let Some(triple) = triple {
        command.arg(format!("--target={triple}"));
    } else {
        // Host modules deliberately carry no `target triple` so the artifact is
        // built for the machine compiling it; clang's "override" is the intent.
        command.arg("-Wno-override-module");
    }
    command.arg(ir).arg("-o").arg(object);
    let status = command.status().map_err(|error| {
        CliFailure::operational(
            "assemble LLVM IR",
            Some(ir.to_path_buf()),
            format!("failed to start clang: {error}"),
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CliFailure::operational(
            "assemble LLVM IR",
            Some(ir.to_path_buf()),
            "clang failed to assemble the LLVM IR module",
        ))
    }
}

fn archive(object: &Path, output: &Path) -> Result<PublishedNativeArtifact, CliFailure> {
    let tool = if tool_ok("llvm-ar") { "llvm-ar" } else { "ar" };
    let _ = fs::remove_file(output);
    let status = Command::new(tool)
        .args(["crs"])
        .arg(output)
        .arg(object)
        .status()
        .map_err(|error| {
            CliFailure::operational(
                "archive static library",
                Some(output.to_path_buf()),
                format!("failed to start {tool}: {error}"),
            )
        })?;
    if !status.success() {
        return Err(CliFailure::operational(
            "archive static library",
            Some(output.to_path_buf()),
            format!("{tool} failed to create the static library"),
        ));
    }
    digest_file(output)
}

fn link_elf(
    object: &Path,
    output: &Path,
    triple: Option<&str>,
    flags: &ProjectFlags,
) -> Result<PublishedNativeArtifact, CliFailure> {
    let Some(script) = flags.linker_script.as_ref() else {
        return Err(CliFailure::usage("--linker-script requires a path"));
    };
    if !script.is_file() {
        return Err(CliFailure::operational(
            "read linker script",
            Some(script.clone()),
            "linker script does not exist; the compiler does not invent a memory map",
        ));
    }
    let triple = triple.ok_or_else(|| {
        CliFailure::usage("--linker-script requires a canonical bare-metal --target")
    })?;
    let status = Command::new("clang")
        .args(["-nostdlib", &format!("--target={triple}"), "-T"])
        .arg(script)
        .arg(object)
        .arg("-o")
        .arg(output)
        .status()
        .map_err(|error| {
            CliFailure::operational(
                "link freestanding ELF",
                Some(output.to_path_buf()),
                format!("failed to start clang: {error}"),
            )
        })?;
    if !status.success() {
        return Err(CliFailure::operational(
            "link freestanding ELF",
            Some(output.to_path_buf()),
            "clang -nostdlib failed; no host runtime was linked",
        ));
    }
    digest_file(output)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<PublishedNativeArtifact, CliFailure> {
    fs::write(path, bytes).map_err(|error| {
        CliFailure::operational(
            "write artifact",
            Some(path.to_path_buf()),
            error.to_string(),
        )
    })?;
    digest_file(path)
}

fn digest_file(path: &Path) -> Result<PublishedNativeArtifact, CliFailure> {
    let bytes = fs::read(path).map_err(|error| {
        CliFailure::operational("read artifact", Some(path.to_path_buf()), error.to_string())
    })?;
    Ok(PublishedNativeArtifact {
        path: path.to_path_buf(),
        digest: blake3::hash(&bytes).to_hex().to_string(),
    })
}

fn tool_ok(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::{LlvmStop, prepare};
    use crate::project::ProjectFlags;

    fn flags(
        target: Option<&str>,
        emit: Option<&str>,
        crate_type: Option<&str>,
        script: bool,
    ) -> ProjectFlags {
        ProjectFlags {
            target: target.map(str::to_owned),
            emit: emit.map(str::to_owned),
            crate_type: crate_type.map(str::to_owned),
            linker_script: script.then(|| std::path::PathBuf::from("memory.ld")),
            ..ProjectFlags::default()
        }
    }

    #[test]
    fn emit_obj_plans_an_object_for_the_canonical_triple() {
        let plan = prepare(
            true,
            &flags(Some("thumbv7em-none-eabihf"), Some("obj"), None, false),
        )
        .expect("obj plan");
        assert_eq!(plan.stop, LlvmStop::Object);
        assert_eq!(
            plan.embedded.map(|profile| profile.triple()),
            Some("thumbv7em-none-eabihf")
        );
    }

    #[test]
    fn bare_metal_without_a_script_publishes_an_object() {
        let plan = prepare(
            true,
            &flags(Some("riscv32imc-unknown-none-elf"), None, None, false),
        )
        .expect("default object");
        assert_eq!(plan.stop, LlvmStop::Object);
    }

    #[test]
    fn cortex_m0_triple_plans_an_object() {
        let plan = prepare(true, &flags(Some("thumbv6m-none-eabi"), None, None, false))
            .expect("cortex-m0 object");
        assert_eq!(plan.stop, LlvmStop::Object);
        assert_eq!(
            plan.embedded.map(|profile| profile.triple()),
            Some("thumbv6m-none-eabi")
        );
    }

    #[test]
    fn unknown_triple_is_not_guessed() {
        let error = prepare(true, &flags(Some("thumbv7em-none-eabi"), None, None, false))
            .expect_err("unknown chip");
        assert!(error.to_string().contains("does not guess"));
    }

    #[test]
    fn cranelift_rejects_a_bare_metal_target() {
        let error = prepare(
            false,
            &flags(Some("thumbv7em-none-eabihf"), None, None, false),
        )
        .expect_err("llvm required");
        assert!(error.to_string().contains("require -b llvm"));
    }
}
