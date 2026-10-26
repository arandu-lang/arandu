use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    LinkAttempt, build_system_linker_command, format_output, linker_name_is_lld,
    strip_verbatim_prefix,
};

#[cfg(windows)]
pub(super) fn run_discovered_msvc(
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
    let sdk_libs_visible = variables
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("LIB") && !value.is_empty());
    let names: &[&str] = if sdk_libs_visible {
        &["lld-link.exe", "lld-link", "link.exe"]
    } else {
        &["link.exe"]
    };
    for name in names {
        let Some(linker) = std::env::split_paths(path_value)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
        else {
            continue;
        };
        let linker = strip_verbatim_prefix(&linker);
        match run_system_linker_with_environment(
            &linker, objects, runtime, output, &variables, size,
        ) {
            Ok(()) => return Ok(()),
            Err(LinkAttempt::NotFound) => {}
            Err(LinkAttempt::Failed(_)) if linker_name_is_lld(linker.as_os_str()) => {}
            Err(failed) => return Err(failed),
        }
    }
    Err(LinkAttempt::NotFound)
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
