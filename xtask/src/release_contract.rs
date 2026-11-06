use std::fs;
use std::path::{Path, PathBuf};

pub fn prepare(root: &Path, version: Option<String>) -> i32 {
    let Some(version) = version else {
        eprintln!("prepare-release: error: usage: prepare-release X.Y.Z[-rc.N]");
        return 2;
    };
    if let Err(error) = validate_version(&version) {
        eprintln!("prepare-release: error: {error}");
        return 2;
    }
    match prepare_workspace(root, &version) {
        Ok(components) => {
            println!(
                "prepare-release: ok (version={version}, {components} component manifests; run cargo check --workspace --locked)"
            );
            0
        }
        Err(error) => {
            eprintln!("prepare-release: error: {error}");
            1
        }
    }
}

pub fn check(root: &Path, explicit_tag: Option<String>) -> i32 {
    let environment_tag = github_tag();
    match validate(root, explicit_tag.as_deref().or(environment_tag.as_deref())) {
        Ok(summary) => {
            println!(
                "check-release-contract: ok (version={}, {} component manifests{})",
                summary.version,
                summary.components,
                summary
                    .tag
                    .as_deref()
                    .map_or_else(String::new, |tag| format!(", tag={tag}"))
            );
            0
        }
        Err(error) => {
            eprintln!("check-release-contract: error: {error}");
            1
        }
    }
}

struct Summary {
    version: String,
    components: usize,
    tag: Option<String>,
}

fn validate(root: &Path, tag: Option<&str>) -> Result<Summary, String> {
    let canonical_path = root.join("crates/arandu_cli/Cargo.toml");
    let canonical = cargo_package_version(&canonical_path)?;
    validate_version(&canonical)?;

    let mut manifests = Vec::new();
    collect_cargo_manifests(&root.join("crates"), &mut manifests)?;
    manifests.extend([
        root.join("arandu_fuzz/Cargo.toml"),
        root.join("xtask/Cargo.toml"),
    ]);
    manifests.sort();
    manifests.dedup();

    for manifest in &manifests {
        let version = cargo_package_version(manifest)?;
        if version != canonical {
            return Err(format!(
                "{} has version {version}, expected {canonical}",
                display_relative(root, manifest)
            ));
        }
    }

    let extension = root.join("editors/vscode/package.json");
    let extension_version = json_string_field(&extension, "version")?;
    if extension_version != canonical {
        return Err(format!(
            "{} has version {extension_version}, expected {canonical}",
            display_relative(root, &extension)
        ));
    }

    let extension_lock = root.join("editors/vscode/package-lock.json");
    let lock_version = json_string_field(&extension_lock, "version")?;
    if lock_version != canonical {
        return Err(format!(
            "{} has version {lock_version}, expected {canonical}",
            display_relative(root, &extension_lock)
        ));
    }

    let normalized_tag = tag.map(str::to_owned);
    if let Some(tag) = tag {
        let Some(tag_version) = tag.strip_prefix('v') else {
            return Err(format!("release tag must start with `v`: {tag}"));
        };
        validate_version(tag_version)?;
        if tag_version != canonical {
            return Err(format!(
                "tag {tag} implies version {tag_version}, expected {canonical}"
            ));
        }
    }

    Ok(Summary {
        version: canonical,
        components: manifests.len() + 1,
        tag: normalized_tag,
    })
}

fn prepare_workspace(root: &Path, version: &str) -> Result<usize, String> {
    let canonical_path = root.join("crates/arandu_cli/Cargo.toml");
    let previous = cargo_package_version(&canonical_path)?;

    let mut manifests = Vec::new();
    collect_cargo_manifests(&root.join("crates"), &mut manifests)?;
    manifests.extend([
        root.join("arandu_fuzz/Cargo.toml"),
        root.join("xtask/Cargo.toml"),
    ]);
    manifests.sort();
    manifests.dedup();

    let report = root.join(format!("docs/releases/{version}.md"));
    let mut touched = manifests.clone();
    touched.extend([
        root.join("editors/vscode/package.json"),
        root.join("editors/vscode/package-lock.json"),
        root.join("docs/diagnostics/SPEC.md"),
        root.join("editors/vscode/CHANGELOG.md"),
        root.join("Cargo.lock"),
        root.join("arandu_fuzz/Cargo.lock"),
        report.clone(),
    ]);
    let originals: Vec<_> = touched
        .iter()
        .map(|path| (path.clone(), fs::read(path).ok()))
        .collect();

    let result = (|| {
        for manifest in &manifests {
            replace_package_version(manifest, version)?;
        }
        replace_json_version(
            &root.join("editors/vscode/package.json"),
            &previous,
            version,
            1,
        )?;
        replace_json_version(
            &root.join("editors/vscode/package-lock.json"),
            &previous,
            version,
            2,
        )?;
        replace_literal(
            &root.join("docs/diagnostics/SPEC.md"),
            &format!("compiler_version: arandu {previous}"),
            &format!("compiler_version: arandu {version}"),
            1,
        )?;
        update_changelog(&root.join("editors/vscode/CHANGELOG.md"), version)?;
        update_workspace_lock(root, &manifests, version)?;
        create_release_report(&report, version)?;
        Ok(())
    })();

    if let Err(error) = result {
        for (path, original) in originals {
            match original {
                Some(bytes) => {
                    let _ = fs::write(path, bytes);
                }
                None => {
                    let _ = fs::remove_file(path);
                }
            }
        }
        return Err(error);
    }

    Ok(manifests.len() + 1)
}

fn create_release_report(path: &Path, version: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let tag = format!("v{version}");
    let report = format!(
        "# Arandu {version} — relatório de promoção\n\n\
**Estado:** preparação; nenhuma tag foi criada.  \n\
**Canal:** release candidate pública do SDK Arandu v0.1.\n\n\
O PR de preparação deve ser mergeado e ficar integralmente verde antes da tag.\n\
Depois, execute na `main` atualizada:\n\n\
```bash\n\
git switch main\n\
git pull --ff-only origin main\n\
cargo run --locked -p xtask -- check-release-contract {tag}\n\
git tag -a {tag} -m \"Arandu {version}\"\n\
git push origin {tag}\n\
```\n\n\
Não mova nem force a tag. O workflow exige o S0 verde do PR mergeado e executa\n\
somente as provas exclusivas de empacotamento, publicação e instalação pública.\n\n\
## Evidência exigida\n\n\
- [ ] Contrato tag ↔ componentes verde.\n\
- [ ] Commit alcançável pela `main` e PR associado com `S0 / Gate` verde.\n\
- [ ] Packages Linux x86-64, macOS ARM64 e Windows x86-64 instalados.\n\
- [ ] Checksums, manifest e provenance verificados.\n\
- [ ] Archives públicos baixados, instalados e aprovados no corpus.\n\
- [ ] Artifact `rc-evidence-{tag}` preservado.\n\
- [ ] Nenhum bloqueador conhecido permanece aberto.\n\n\
## Resultado\n\n\
- Tag: `{tag}`\n\
- Commit: pendente\n\
- Workflow: pendente\n\
- Release imutável: pendente\n\
- Linux x86-64: pendente\n\
- macOS ARM64: pendente\n\
- Windows x86-64: pendente\n\
- Bloqueadores: pendente\n"
    );
    write_text(path, &report)
}

fn replace_package_version(path: &Path, version: &str) -> Result<(), String> {
    let text = read_text(path)?;
    let mut in_package = false;
    let mut replaced = false;
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
        }
        if in_package && !replaced && trimmed.starts_with("version") {
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            output.push_str(&format!("version = \"{version}\"{ending}"));
            replaced = true;
        } else {
            output.push_str(line);
        }
    }
    if !replaced {
        return Err(format!("missing [package] version in {}", path.display()));
    }
    write_text(path, &output)
}

fn replace_json_version(
    path: &Path,
    previous: &str,
    version: &str,
    expected: usize,
) -> Result<(), String> {
    replace_literal(
        path,
        &format!("\"version\": \"{previous}\""),
        &format!("\"version\": \"{version}\""),
        expected,
    )
}

fn replace_literal(path: &Path, from: &str, to: &str, expected: usize) -> Result<(), String> {
    let text = read_text(path)?;
    let count = text.matches(from).count();
    if count != expected {
        return Err(format!(
            "{} contains {count} copies of {from:?}, expected {expected}",
            path.display()
        ));
    }
    write_text(path, &text.replace(from, to))
}

fn update_changelog(path: &Path, version: &str) -> Result<(), String> {
    let text = read_text(path)?;
    let heading = format!("## {version}");
    if text.lines().any(|line| line.trim() == heading) {
        return Ok(());
    }
    let Some(first_break) = text.find('\n') else {
        return Err(format!("malformed changelog: {}", path.display()));
    };
    let mut output = String::with_capacity(text.len() + heading.len() + 48);
    output.push_str(&text[..=first_break]);
    output.push('\n');
    output.push_str(&heading);
    output.push_str("\n\n- Release candidate promotion preparation.\n");
    output.push_str(&text[first_break + 1..]);
    write_text(path, &output)
}

/// Rewrites the Arandu package versions of the workspace lockfile and of the
/// fuzz lockfile, which `arandu_fuzz` keeps outside the workspace.
fn update_workspace_lock(root: &Path, manifests: &[PathBuf], version: &str) -> Result<(), String> {
    let mut names = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        names.push(cargo_package_name(manifest)?);
    }
    let workspace = update_lock_versions(&root.join("Cargo.lock"), &names, version)?;
    let expected = names
        .iter()
        .filter(|name| name.as_str() != "arandu_fuzz")
        .count();
    if workspace != expected {
        return Err(format!(
            "Cargo.lock updated {workspace} workspace packages, expected {expected}"
        ));
    }
    if update_lock_versions(&root.join("arandu_fuzz/Cargo.lock"), &names, version)? == 0 {
        return Err("arandu_fuzz/Cargo.lock lists no Arandu package".to_owned());
    }
    Ok(())
}

/// Sets `version` on every package of the lockfile at `path` named in
/// `names`, returning how many it updated.
fn update_lock_versions(path: &Path, names: &[String], version: &str) -> Result<usize, String> {
    let text = read_text(path)?;
    let mut output = String::with_capacity(text.len());
    let mut current_name: Option<String> = None;
    let mut updated = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            current_name = None;
        } else if let Some(name) = trimmed
            .strip_prefix("name = \"")
            .and_then(|v| v.strip_suffix('"'))
        {
            current_name = Some(name.to_owned());
        }
        if trimmed.starts_with("version = \"")
            && current_name
                .as_ref()
                .is_some_and(|name| names.contains(name))
        {
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            output.push_str(&format!("version = \"{version}\"{ending}"));
            updated += 1;
        } else {
            output.push_str(line);
        }
    }
    write_text(path, &output)?;
    Ok(updated)
}

fn cargo_package_name(path: &Path) -> Result<String, String> {
    let text = read_text(path)?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = line.strip_prefix("name").and_then(parse_assignment_string) {
                return Ok(value.to_owned());
            }
        }
    }
    Err(format!("missing [package] name in {}", path.display()))
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn github_tag() -> Option<String> {
    if std::env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag") {
        return std::env::var("GITHUB_REF_NAME").ok();
    }
    None
}

fn collect_cargo_manifests(dir: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate {}: {error}", dir.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_cargo_manifests(&path, output)?;
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            output.push(path);
        }
    }
    Ok(())
}

fn cargo_package_version(path: &Path) -> Result<String, String> {
    let text = read_text(path)?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = line
                .strip_prefix("version")
                .and_then(parse_assignment_string)
            {
                return Ok(value.to_owned());
            }
        }
    }
    Err(format!("missing [package] version in {}", path.display()))
}

fn parse_assignment_string(value: &str) -> Option<&str> {
    let value = value.trim_start().strip_prefix('=')?.trim();
    value.strip_prefix('"')?.strip_suffix('"')
}

fn json_string_field(path: &Path, field: &str) -> Result<String, String> {
    let text = read_text(path)?;
    let prefix = format!("\"{field}\"");
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix(&prefix).and_then(parse_json_string_value) {
            return Ok(value.to_owned());
        }
    }
    Err(format!(
        "missing JSON string field {field:?} in {}",
        path.display()
    ))
}

fn parse_json_string_value(value: &str) -> Option<&str> {
    let value = value.trim_start().strip_prefix(':')?.trim();
    value
        .strip_suffix(',')
        .unwrap_or(value)
        .strip_prefix('"')?
        .strip_suffix('"')
}

fn validate_version(version: &str) -> Result<(), String> {
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(core, pre)| (core, Some(pre)));
    let parts: Vec<_> = core.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(format!("version is not canonical SemVer: {version}"));
    }
    if prerelease.is_some_and(|pre| {
        pre.is_empty()
            || pre.split('.').any(|identifier| {
                let numeric = identifier.bytes().all(|byte| byte.is_ascii_digit());
                identifier.is_empty()
                    || (numeric && identifier.len() > 1 && identifier.starts_with('0'))
                    || !identifier
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    }) {
        return Err(format!("invalid SemVer prerelease: {version}"));
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn display_relative<'a>(root: &'a Path, path: &'a Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::validate_version;

    #[test]
    fn accepts_release_and_rc_versions() {
        for version in ["0.0.1", "0.1.0-rc.1", "1.2.3-beta.2"] {
            assert!(validate_version(version).is_ok(), "{version}");
        }
    }

    #[test]
    fn rejects_noncanonical_versions() {
        for version in [
            "v0.1.0",
            "0.1",
            "00.1.0",
            "0.1.0-",
            "0.1.0-rc!",
            "0.1.0-rc.01",
        ] {
            assert!(validate_version(version).is_err(), "{version}");
        }
    }
}
