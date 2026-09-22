//! Archive verification command for Arandu distributions (RFC 0020, marco DIST).

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde_json::Value as JsonValue;
use tar::Archive as TarArchive;
use zip::ZipArchive;

use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::pipeline::fail_usage;

const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

fn read_limited(reader: &mut impl Read, limit: usize, label: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    if bytes.len() > limit {
        return Err(format!("{label} exceeds the {limit}-byte validation limit"));
    }
    Ok(bytes)
}

pub fn cmd_archive(args: &[String]) -> CliResult {
    if args.len() < 3 {
        fail_usage(
            "usage: arandu archive validate <archive-path> [--root <name>] [--version <ver>] [--target <target>]",
        );
    }

    let subcmd = &args[2];
    if subcmd != "validate" {
        fail_usage(format!(
            "unknown archive subcommand '{subcmd}', expected 'validate'"
        ));
    }

    let mut archive_path: Option<PathBuf> = None;
    let mut root: Option<String> = None;
    let mut version: Option<String> = None;
    let mut target: Option<String> = None;

    let mut i = 3;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--root" && i + 1 < args.len() {
            i += 1;
            root = Some(args[i].clone());
        } else if let Some(val) = arg.strip_prefix("--root=") {
            root = Some(val.to_string());
        } else if arg == "--version" && i + 1 < args.len() {
            i += 1;
            version = Some(args[i].clone());
        } else if let Some(val) = arg.strip_prefix("--version=") {
            version = Some(val.to_string());
        } else if arg == "--target" && i + 1 < args.len() {
            i += 1;
            target = Some(args[i].clone());
        } else if let Some(val) = arg.strip_prefix("--target=") {
            target = Some(val.to_string());
        } else if !arg.starts_with('-') && archive_path.is_none() {
            archive_path = Some(PathBuf::from(arg));
        } else {
            fail_usage(format!(
                "unexpected argument '{arg}' to 'arandu archive validate'"
            ));
        }
        i += 1;
    }

    let Some(archive) = archive_path else {
        fail_usage("missing archive path to 'arandu archive validate'");
    };

    if !archive.is_file() {
        return Err(CliFailure::operational(
            "validate archive",
            Some(archive.clone()),
            format!("file '{}' not found", archive.display()),
        ));
    }

    match validate_archive_file(
        &archive,
        root.as_deref(),
        version.as_deref(),
        target.as_deref(),
    ) {
        Ok(stats) => {
            println!(
                "archive ok: {} files, {} symlink(s), valid release-manifest.json (version: {}, target: {})",
                stats.files, stats.symlinks, stats.version, stats.target
            );
            Ok(CliSuccess::Done)
        }
        Err(err) => Err(CliFailure::operational(
            "validate archive",
            Some(archive),
            err,
        )),
    }
}

pub struct ArchiveStats {
    pub files: usize,
    pub symlinks: usize,
    pub target: String,
    pub version: String,
}

fn validate_archive_file(
    path: &Path,
    explicit_root: Option<&str>,
    explicit_version: Option<&str>,
    explicit_target: Option<&str>,
) -> Result<ArchiveStats, String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        validate_tar_archive(path, explicit_root, explicit_version, explicit_target)
    } else if name.ends_with(".zip") {
        validate_zip_archive(path, explicit_root, explicit_version, explicit_target)
    } else {
        Err(format!(
            "unsupported archive extension for '{}'; expected .tar.gz or .zip",
            path.display()
        ))
    }
}

fn validate_tar_archive(
    path: &Path,
    explicit_root: Option<&str>,
    explicit_version: Option<&str>,
    explicit_target: Option<&str>,
) -> Result<ArchiveStats, String> {
    let file = File::open(path).map_err(|e| format!("cannot open archive: {e}"))?;
    let gz = GzDecoder::new(file);
    let mut tar = TarArchive::new(gz);

    let mut entries_data = Vec::new();
    let mut manifest_bytes = None;
    let mut root_candidate = None;

    let entries = tar
        .entries()
        .map_err(|e| format!("cannot read tar entries: {e}"))?;
    let mut total_size = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("corrupted tar entry: {e}"))?;
        if entries_data.len() >= MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry limit"
            ));
        }
        total_size = total_size
            .checked_add(entry.size())
            .filter(|size| *size <= MAX_ARCHIVE_UNCOMPRESSED_BYTES)
            .ok_or_else(|| "archive exceeds the 4 GiB uncompressed validation limit".to_string())?;
        let entry_path = entry
            .path()
            .map_err(|e| format!("invalid tar entry path: {e}"))?
            .to_string_lossy()
            .to_string();
        validate_path_safety(&entry_path)?;

        if root_candidate.is_none() {
            root_candidate = entry_path
                .split('/')
                .next()
                .filter(|s| !s.is_empty())
                .map(String::from);
        }

        let entry_type = entry.header().entry_type();
        let is_symlink = entry_type.is_symlink();
        let is_file = entry_type.is_file();
        let is_dir = entry_type.is_dir();

        let linkname = if is_symlink {
            Some(
                entry
                    .link_name()
                    .map_err(|e| format!("failed to read link: {e}"))?
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            )
        } else {
            None
        };

        if entry_path.ends_with("/release-manifest.json") {
            let buf = read_limited(&mut entry, MAX_MANIFEST_BYTES, "release manifest")?;
            manifest_bytes = Some(buf);
        }

        entries_data.push((entry_path, is_file, is_symlink, is_dir, linkname));
    }

    let manifest_bytes =
        manifest_bytes.ok_or_else(|| "release-manifest.json missing in archive".to_string())?;
    let manifest_json: JsonValue = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid release-manifest.json: {e}"))?;

    let version = explicit_version
        .or_else(|| manifest_json.get("version").and_then(|v| v.as_str()))
        .ok_or_else(|| "cannot determine version from manifest".to_string())?;

    let target = explicit_target
        .or_else(|| manifest_json.get("target").and_then(|v| v.as_str()))
        .ok_or_else(|| "cannot determine target from manifest".to_string())?;

    let root = explicit_root
        .map(|r| r.to_string())
        .or(root_candidate)
        .unwrap_or_else(|| format!("arandu-{version}"));

    verify_manifest_data(&manifest_json, version, target, "tar.gz")?;

    let required: HashSet<String> = [
        format!("{root}/bin/arandu_cli"),
        format!("{root}/bin/arandu"),
        format!("{root}/bin/arandu-lsp"),
        format!("{root}/lib/{target}/libarandu_runtime.a"),
        format!("{root}/BLAKE3SUMS"),
        format!("{root}/LICENSE-MIT"),
        format!("{root}/LICENSE-APACHE"),
        format!("{root}/release-manifest.json"),
    ]
    .into_iter()
    .collect();

    let mut seen = HashSet::new();
    let mut files = 0;
    let mut symlinks = 0;

    for (name, is_file, is_symlink, is_dir, linkname) in entries_data {
        let folded = name.to_lowercase();
        if !seen.insert(folded) {
            return Err(format!("duplicate entry in archive: '{name}'"));
        }
        if name != root && !name.starts_with(&format!("{root}/")) {
            return Err(format!("entry outside package root '{root}': '{name}'"));
        }

        if is_symlink {
            symlinks += 1;
            if name != format!("{root}/bin/arandu") || linkname.as_deref() != Some("arandu_cli") {
                return Err(format!("unexpected symlink: '{name}' -> {:?}", linkname));
            }
        } else if is_file {
            files += 1;
        } else if !is_dir {
            return Err(format!("unsupported entry type for '{name}'"));
        }
    }

    for req in &required {
        if !seen.contains(&req.to_lowercase()) {
            return Err(format!("archive missing required entry: '{req}'"));
        }
    }

    let stdlib_prefix = format!("{}/share/arandu/stdlib/", root.to_lowercase());
    let has_stdlib_aru = seen
        .iter()
        .any(|s| s.starts_with(&stdlib_prefix) && s.ends_with(".aru"));
    if !has_stdlib_aru {
        return Err("archive contains no stdlib .aru files".to_string());
    }

    Ok(ArchiveStats {
        files,
        symlinks,
        target: target.to_string(),
        version: version.to_string(),
    })
}

fn validate_zip_archive(
    path: &Path,
    explicit_root: Option<&str>,
    explicit_version: Option<&str>,
    explicit_target: Option<&str>,
) -> Result<ArchiveStats, String> {
    let file = File::open(path).map_err(|e| format!("cannot open zip: {e}"))?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("cannot read zip: {e}"))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry limit"
        ));
    }

    let mut manifest_bytes = None;
    let mut root_candidate = None;
    let mut total_size = 0u64;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("corrupted zip entry #{i}: {e}"))?;
        total_size = total_size
            .checked_add(entry.size())
            .filter(|size| *size <= MAX_ARCHIVE_UNCOMPRESSED_BYTES)
            .ok_or_else(|| "archive exceeds the 4 GiB uncompressed validation limit".to_string())?;
        let name = entry.name().to_string();
        validate_path_safety(&name)?;

        if root_candidate.is_none() {
            root_candidate = name
                .split('/')
                .next()
                .filter(|s| !s.is_empty())
                .map(String::from);
        }

        if name.ends_with("/release-manifest.json") {
            let buf = read_limited(&mut entry, MAX_MANIFEST_BYTES, "release manifest")?;
            manifest_bytes = Some(buf);
        }
    }

    let manifest_bytes =
        manifest_bytes.ok_or_else(|| "release-manifest.json missing in zip".to_string())?;
    let manifest_json: JsonValue = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid release-manifest.json: {e}"))?;

    let version = explicit_version
        .or_else(|| manifest_json.get("version").and_then(|v| v.as_str()))
        .ok_or_else(|| "cannot determine version from manifest".to_string())?;

    let target = explicit_target
        .or_else(|| manifest_json.get("target").and_then(|v| v.as_str()))
        .ok_or_else(|| "cannot determine target from manifest".to_string())?;

    let root = explicit_root
        .map(|r| r.to_string())
        .or(root_candidate)
        .unwrap_or_else(|| format!("arandu-{version}"));

    verify_manifest_data(&manifest_json, version, target, "zip")?;

    let required: HashSet<String> = [
        format!("{root}/bin/arandu.exe"),
        format!("{root}/bin/arandu_cli.exe"),
        format!("{root}/bin/arandu-lsp.exe"),
        format!("{root}/lib/{target}/arandu_runtime.lib"),
        format!("{root}/BLAKE3SUMS"),
        format!("{root}/LICENSE-MIT"),
        format!("{root}/LICENSE-APACHE"),
        format!("{root}/release-manifest.json"),
    ]
    .into_iter()
    .collect();

    let mut seen = HashSet::new();
    let mut files = 0;

    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| format!("entry #{i} error: {e}"))?;
        let name = entry.name().to_string();

        let folded = name.to_lowercase();
        if !seen.insert(folded) {
            return Err(format!("duplicate entry in zip: '{name}'"));
        }

        if !name.starts_with(&format!("{root}/")) {
            return Err(format!("entry outside package root '{root}': '{name}'"));
        }

        let ext_attr = entry.unix_mode().unwrap_or(0);
        if ext_attr != 0 && (ext_attr & 0o170000) != 0 && (ext_attr & 0o170000) != 0o100000 {
            return Err(format!("unsupported zip entry type for '{name}'"));
        }

        files += 1;
    }

    for req in &required {
        if !seen.contains(&req.to_lowercase()) {
            return Err(format!("zip missing required entry: '{req}'"));
        }
    }

    let stdlib_prefix = format!("{}/share/arandu/stdlib/", root.to_lowercase());
    let has_stdlib_aru = seen
        .iter()
        .any(|s| s.starts_with(&stdlib_prefix) && s.ends_with(".aru"));
    if !has_stdlib_aru {
        return Err("zip contains no stdlib .aru files".to_string());
    }

    Ok(ArchiveStats {
        files,
        symlinks: 0,
        target: target.to_string(),
        version: version.to_string(),
    })
}

fn validate_path_safety(name: &str) -> Result<(), String> {
    if name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains('\r')
        || name.contains('\n')
        || name == ".."
        || name.starts_with("../")
        || name.contains("/../")
        || name.ends_with("/..")
        || name.contains("//")
    {
        return Err(format!("unsafe archive path: '{name}'"));
    }
    Ok(())
}

fn verify_manifest_data(
    json: &JsonValue,
    version: &str,
    target: &str,
    archive_format: &str,
) -> Result<(), String> {
    let schema = json.get("schema").and_then(|v| v.as_u64());
    if schema != Some(1) {
        return Err(format!(
            "invalid release manifest schema: expected 1, got {schema:?}"
        ));
    }

    let v = json.get("version").and_then(|v| v.as_str());
    if v != Some(version) {
        return Err(format!(
            "version mismatch in manifest: expected '{version}', got {v:?}"
        ));
    }

    let t = json.get("target").and_then(|v| v.as_str());
    if t != Some(target) {
        return Err(format!(
            "target mismatch in manifest: expected '{target}', got {t:?}"
        ));
    }

    let arch = json.get("archive").and_then(|v| v.as_str());
    if arch != Some(archive_format) {
        return Err(format!(
            "archive format mismatch: expected '{archive_format}', got {arch:?}"
        ));
    }

    let expected_components = ["arandu", "arandu-lsp", "runtime", "stdlib"];
    let components = json
        .get("components")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "components missing in release manifest".to_string())?;

    let comp_strs: Vec<&str> = components.iter().filter_map(|c| c.as_str()).collect();
    if comp_strs != expected_components {
        return Err(format!(
            "components mismatch: expected {expected_components:?}, got {comp_strs:?}"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_manifest_reader_rejects_oversized_input() {
        assert_eq!(
            read_limited(&mut Cursor::new(b"small"), 5, "manifest").unwrap(),
            b"small"
        );
        assert!(read_limited(&mut Cursor::new(b"larger"), 5, "manifest").is_err());
    }

    #[test]
    fn test_validate_path_safety() {
        assert!(validate_path_safety("bin/arandu").is_ok());
        assert!(validate_path_safety("lib/x86_64/libarandu.a").is_ok());

        assert!(validate_path_safety("/bin/arandu").is_err());
        assert!(validate_path_safety("bin\\arandu").is_err());
        assert!(validate_path_safety("..").is_err());
        assert!(validate_path_safety("../secret").is_err());
        assert!(validate_path_safety("bin/../secret").is_err());
        assert!(validate_path_safety("bin/..").is_err());
        assert!(validate_path_safety("bin//arandu").is_err());
        assert!(validate_path_safety("bin/arandu\0malicious").is_err());
        assert!(validate_path_safety("bin/arandu\r\n").is_err());
        assert!(validate_path_safety("bin/arandu\n").is_err());
    }
}
