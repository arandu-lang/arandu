//! Project scaffolding templates, generation, and atomic directory initialization.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_query::MANIFEST_FILENAME;

use super::vcs::{VcsChoice, initialize_git};
use crate::cli_error::{CliFailure, CliResult, CliSuccess};

/// Official default entry path for `arandu new`.
pub const DEFAULT_ENTRY: &str = "src/main.aru";

/// Default `src/main.aru` content — Minimal 0.1 IN surface only.
///
/// Kept in sync with `examples/minimal/TEMPLATE_main.aru` (covered by parse CI).
pub const TEMPLATE_MAIN_ARU: &str = r#"// Default project template for Arandu Minimal 0.1 (installer / `arandu new`).
// Only IN surface — no experimental runtime/OS modules.
module my_app

import io

func main(): int {
    io.println("hello, arandu")
    return 0
}
"#;

/// Create a new project directory with `Arandu.toml` + template entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldKind {
    Binary,
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaffoldOptions {
    pub kind: ScaffoldKind,
    pub vcs: VcsChoice,
}

impl Default for ScaffoldOptions {
    fn default() -> Self {
        Self {
            kind: ScaffoldKind::Binary,
            vcs: VcsChoice::Auto,
        }
    }
}

pub fn parse_scaffold_options(args: &[String]) -> Result<ScaffoldOptions, String> {
    let mut options = ScaffoldOptions::default();
    let mut kind_seen = false;
    for argument in args {
        match argument.as_str() {
            "--bin" if !kind_seen => {
                options.kind = ScaffoldKind::Binary;
                kind_seen = true;
            }
            "--lib" if !kind_seen => {
                options.kind = ScaffoldKind::Library;
                kind_seen = true;
            }
            "--bin" | "--lib" => return Err("use only one of `--bin` or `--lib`".into()),
            "--vcs=auto" => options.vcs = VcsChoice::Auto,
            "--vcs=git" => options.vcs = VcsChoice::Git,
            "--vcs=none" => options.vcs = VcsChoice::None,
            other => return Err(format!("unknown project option `{other}`")),
        }
    }
    Ok(options)
}

pub fn cmd_new(name: &str, options: ScaffoldOptions, quiet: bool) -> CliResult {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(CliFailure::usage(format!(
            "invalid project name `{name}` (use a single path segment)"
        )));
    }
    let root = PathBuf::from(name);
    let parent = root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    reject_case_collision(parent, name)?;
    if root.exists() {
        return Err(CliFailure::operational(
            "create project",
            Some(root),
            "path already exists",
        ));
    }
    arandu_query::validate_package_name(name).map_err(|error| {
        CliFailure::operational(
            "validate project name",
            Some(root.clone()),
            error.to_string(),
        )
    })?;
    let leaf = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let staging = staging_path(parent, leaf, "new");
    if let Err(error) = scaffold_into(&staging, name, options, parent) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::rename(&staging, &root).map_err(|error| {
        let _ = fs::remove_dir_all(&staging);
        CliFailure::operational("publish project", Some(root.clone()), error.to_string())
    })?;

    print_created(name, options.kind, quiet);
    Ok(CliSuccess::Done)
}

pub fn cmd_init(root: &Path, name: &str, options: ScaffoldOptions, quiet: bool) -> CliResult {
    if !root.is_dir() {
        return Err(CliFailure::operational(
            "initialize project",
            Some(root.to_path_buf()),
            "directory does not exist",
        ));
    }
    let source_name = source_name(options.kind);
    let generated = [
        MANIFEST_FILENAME,
        "README.md",
        ".gitignore",
        source_name,
        "tests/smoke.aru",
    ];
    for relative in generated {
        if root.join(relative).exists() {
            return Err(CliFailure::operational(
                "initialize project",
                Some(root.join(relative)),
                "project file already exists",
            ));
        }
    }
    arandu_query::validate_package_name(name).map_err(|error| {
        CliFailure::operational(
            "validate project name",
            Some(root.to_path_buf()),
            error.to_string(),
        )
    })?;

    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    let leaf = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let staging = staging_path(parent, leaf, "init");
    let staged_options = ScaffoldOptions {
        kind: options.kind,
        vcs: VcsChoice::None,
    };
    if let Err(error) = scaffold_into(&staging, name, staged_options, root) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = publish_into_existing(root, &staging, source_name, options.vcs) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&staging);
    print_created(name, options.kind, quiet);
    Ok(CliSuccess::Done)
}

fn source_name(kind: ScaffoldKind) -> &'static str {
    match kind {
        ScaffoldKind::Binary => "src/main.aru",
        ScaffoldKind::Library => "src/lib.aru",
    }
}

fn staging_path(parent: &Path, leaf: &str, operation: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        ".{leaf}.arandu-{operation}-{}-{nonce}",
        std::process::id()
    ))
}

fn reject_case_collision(parent: &Path, requested: &str) -> Result<(), CliFailure> {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(CliFailure::operational(
                "inspect project parent",
                Some(parent.to_path_buf()),
                error.to_string(),
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            CliFailure::operational(
                "inspect project parent",
                Some(parent.to_path_buf()),
                error.to_string(),
            )
        })?;
        let existing = entry.file_name();
        if let Some(existing) = existing.to_str()
            && existing != requested
            && existing.eq_ignore_ascii_case(requested)
        {
            return Err(CliFailure::operational(
                "validate project path",
                Some(parent.join(requested)),
                format!("name differs only by case from existing `{existing}`"),
            ));
        }
    }
    Ok(())
}

fn publish_into_existing(
    root: &Path,
    staging: &Path,
    source_name: &str,
    vcs: VcsChoice,
) -> Result<(), CliFailure> {
    let src_created = !root.join("src").exists();
    let tests_created = !root.join("tests").exists();
    let git_existed = root.join(".git").exists();
    let mut published = Vec::new();

    let result = (|| {
        fs::create_dir_all(root.join("src")).map_err(|error| {
            CliFailure::operational(
                "create source directory",
                Some(root.join("src")),
                error.to_string(),
            )
        })?;
        fs::create_dir_all(root.join("tests")).map_err(|error| {
            CliFailure::operational(
                "create test directory",
                Some(root.join("tests")),
                error.to_string(),
            )
        })?;
        for relative in [
            MANIFEST_FILENAME,
            "README.md",
            ".gitignore",
            source_name,
            "tests/smoke.aru",
        ] {
            let destination = root.join(relative);
            fs::rename(staging.join(relative), &destination).map_err(|error| {
                CliFailure::operational(
                    "publish project file",
                    Some(destination.clone()),
                    error.to_string(),
                )
            })?;
            published.push(destination);
        }
        initialize_git(root, root, vcs)
    })();

    if let Err(error) = result {
        for path in published.iter().rev() {
            let _ = fs::remove_file(path);
        }
        if tests_created {
            let _ = fs::remove_dir(root.join("tests"));
        }
        if src_created {
            let _ = fs::remove_dir(root.join("src"));
        }
        if !git_existed && root.join(".git").exists() {
            let _ = fs::remove_dir_all(root.join(".git"));
        }
        return Err(error);
    }
    Ok(())
}

fn scaffold_into(root: &Path, name: &str, options: ScaffoldOptions, vcs_probe: &Path) -> CliResult {
    fs::create_dir_all(root.join("src")).map_err(|e| {
        CliFailure::operational(
            "create project directories",
            Some(root.to_path_buf()),
            e.to_string(),
        )
    })?;
    fs::create_dir_all(root.join("tests")).map_err(|e| {
        CliFailure::operational(
            "create test directory",
            Some(root.to_path_buf()),
            e.to_string(),
        )
    })?;

    let toolchain = toolchain_requirement(super::load::ARANDU_VERSION);
    let toml = format!(
        r#"# Arandu package manifest
schema = 1

[package]
name = "{name}"
version = "0.0.1"
edition = "2026"

[toolchain]
arandu = "{toolchain}"

[targets.bin]
name = "{name}"
root = "{DEFAULT_ENTRY}"

[dependencies]

# Reserved policy surface. A2 will infer effects; empty capabilities deny authority.
[capabilities]
network = []
filesystem_read = []
filesystem_write = []
environment = []
process = []
foreign = false

[policy.effects]
deny_new_authority = true
warn_new_resources = true
deny = ["UnknownCapability"]
"#
    );
    let (target_table, source_name, source) = match options.kind {
        ScaffoldKind::Binary => (
            "targets.bin",
            "main.aru",
            TEMPLATE_MAIN_ARU.replace("module my_app", &format!("module {name}")),
        ),
        ScaffoldKind::Library => (
            "targets.lib",
            "lib.aru",
            format!("module {name}\n\npublic func answer(): int {{\n    return 42\n}}\n"),
        ),
    };
    let toml = toml
        .replace("targets.bin", target_table)
        .replace(DEFAULT_ENTRY, &format!("src/{source_name}"));

    let manifest_path = root.join(MANIFEST_FILENAME);
    let entry_path = root.join("src").join(source_name);
    fs::write(&manifest_path, toml).map_err(|e| {
        CliFailure::operational("write project manifest", Some(manifest_path), e.to_string())
    })?;
    fs::write(&entry_path, source).map_err(|e| {
        CliFailure::operational("write project entry", Some(entry_path), e.to_string())
    })?;

    fs::write(
        root.join("README.md"),
        format!("# {name}\n\nCreated with the Arandu toolchain.\n"),
    )
    .map_err(|e| {
        CliFailure::operational("write README", Some(root.join("README.md")), e.to_string())
    })?;
    fs::write(root.join(".gitignore"), "/target/\n/.arandu/\n").map_err(|e| {
        CliFailure::operational(
            "write VCS ignore",
            Some(root.join(".gitignore")),
            e.to_string(),
        )
    })?;
    fs::write(
        root.join("tests/smoke.aru"),
        format!("module {name}_tests\n\n@Test\nfunc smoke(): void {{}}\n"),
    )
    .map_err(|e| {
        CliFailure::operational(
            "write test template",
            Some(root.join("tests/smoke.aru")),
            e.to_string(),
        )
    })?;
    initialize_git(root, vcs_probe, options.vcs)?;
    Ok(CliSuccess::Done)
}

/// The toolchain range a new manifest declares: this toolchain up to the next
/// release that may break it (the next minor below 1.0, the next major after).
fn toolchain_requirement(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.').map(str::parse::<u64>);
    match (parts.next(), parts.next()) {
        (Some(Ok(0)), Some(Ok(minor))) => format!(">={version}, <0.{}.0", minor + 1),
        (Some(Ok(major)), Some(Ok(_))) => format!(">={version}, <{}.0.0", major + 1),
        _ => format!(">={version}"),
    }
}

fn print_created(name: &str, kind: ScaffoldKind, quiet: bool) {
    if quiet {
        return;
    }
    println!(
        "created {name} ({})",
        if kind == ScaffoldKind::Binary {
            "bin"
        } else {
            "lib"
        }
    );
    println!("next:\n  cd {name}\n  arandu check");
    if kind == ScaffoldKind::Binary {
        println!("  arandu run");
    }
}

#[cfg(test)]
mod tests {
    use super::toolchain_requirement;

    #[test]
    fn new_manifests_accept_this_toolchain_until_the_next_breaking_release() {
        assert_eq!(toolchain_requirement("0.2.0"), ">=0.2.0, <0.3.0");
        assert_eq!(toolchain_requirement("0.2.1-rc.3"), ">=0.2.1-rc.3, <0.3.0");
        assert_eq!(toolchain_requirement("1.4.2"), ">=1.4.2, <2.0.0");
    }
}
