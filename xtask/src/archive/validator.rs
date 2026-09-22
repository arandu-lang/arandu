//! Structural security and taxonomy validator for release archives (RFC 0020).

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use serde_json::Value as JsonValue;
use tar::Archive as TarArchive;
use zip::ZipArchive;

const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

fn read_limited(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read archive metadata: {error}"))?;
    if bytes.len() > limit {
        return Err(format!("archive metadata exceeds the {limit}-byte limit"));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Default)]
pub struct ValidationStats {
    pub files: usize,
    pub symlinks: usize,
    pub target: String,
    pub version: String,
}

pub fn validate_tar_gz(
    path: &Path,
    root: &str,
    version: &str,
    target: &str,
) -> Result<ValidationStats, String> {
    let file = File::open(path)
        .map_err(|e| format!("failed to open tar archive '{}': {e}", path.display()))?;
    let gz = GzDecoder::new(file);
    let mut tar = TarArchive::new(gz);

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
    let mut manifest_bytes = None;
    let mut file_count = 0;
    let mut symlink_count = 0;

    let entries = tar
        .entries()
        .map_err(|e| format!("failed to read tar entries: {e}"))?;

    let mut entry_count = 0usize;
    let mut total_size = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("corrupted tar entry: {e}"))?;
        entry_count += 1;
        if entry_count > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry limit"
            ));
        }
        total_size = total_size
            .checked_add(entry.size())
            .filter(|size| *size <= MAX_ARCHIVE_UNCOMPRESSED_BYTES)
            .ok_or_else(|| "archive exceeds the 4 GiB uncompressed validation limit".to_string())?;
        let name = entry
            .path()
            .map_err(|e| format!("invalid tar entry path: {e}"))?
            .to_string_lossy()
            .to_string();

        validate_path_safety(&name)?;

        let folded = name.to_lowercase();
        if !seen.insert(folded) {
            return Err(format!("duplicate archive entry: '{name}'"));
        }

        if name != root && !name.starts_with(&format!("{root}/")) {
            return Err(format!("entry outside package root: '{name}'"));
        }

        let is_allowed_dir = name == root
            || name == format!("{root}/bin")
            || name == format!("{root}/lib")
            || name == format!("{root}/lib/{target}")
            || name == format!("{root}/share")
            || name == format!("{root}/share/arandu")
            || name == format!("{root}/share/arandu/stdlib");

        let is_stdlib = name.starts_with(&format!("{root}/share/arandu/stdlib/"));
        let is_required = required.contains(&name);

        if !is_required && !is_allowed_dir && !is_stdlib {
            return Err(format!("unexpected archive content: '{name}'"));
        }

        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() {
            symlink_count += 1;
            let linkname = entry
                .link_name()
                .map_err(|e| format!("failed to read link name: {e}"))?
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if name != format!("{root}/bin/arandu") || linkname != "arandu_cli" {
                return Err(format!("unexpected symlink: '{name}' -> '{linkname}'"));
            }
        } else if entry_type.is_file() {
            file_count += 1;
            if name == format!("{root}/release-manifest.json") {
                manifest_bytes = Some(read_limited(&mut entry, MAX_MANIFEST_BYTES)?);
            }
        } else if !entry_type.is_dir() {
            return Err(format!(
                "unexpected archive entry type for '{name}': {entry_type:?}"
            ));
        }
    }

    let mut missing = Vec::new();
    for req in &required {
        if !seen.contains(&req.to_lowercase()) {
            missing.push(req.clone());
        }
    }
    if !missing.is_empty() {
        missing.sort();
        return Err(format!(
            "archive missing required entries: {}",
            missing.join(", ")
        ));
    }

    let stdlib_prefix = format!("{}/share/arandu/stdlib/", root.to_lowercase());
    let has_stdlib_aru = seen
        .iter()
        .any(|s| s.starts_with(&stdlib_prefix) && s.ends_with(".aru"));
    if !has_stdlib_aru {
        return Err("archive contains no stdlib .aru files".to_string());
    }

    let manifest_bytes =
        manifest_bytes.ok_or_else(|| "release-manifest.json was not found".to_string())?;
    verify_manifest_json(&manifest_bytes, version, target, "tar.gz")?;

    Ok(ValidationStats {
        files: file_count,
        symlinks: symlink_count,
        target: target.to_string(),
        version: version.to_string(),
    })
}

pub fn validate_zip(
    path: &Path,
    root: &str,
    version: &str,
    target: &str,
) -> Result<ValidationStats, String> {
    let file = File::open(path)
        .map_err(|e| format!("failed to open zip archive '{}': {e}", path.display()))?;
    let mut zip = ZipArchive::new(file)
        .map_err(|e| format!("failed to read zip archive '{}': {e}", path.display()))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry limit"
        ));
    }

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
    let mut manifest_bytes = None;
    let mut file_count = 0;

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

        let folded = name.to_lowercase();
        if !seen.insert(folded) {
            return Err(format!("duplicate zip entry: '{name}'"));
        }

        if !name.starts_with(&format!("{root}/")) {
            return Err(format!("entry outside package root: '{name}'"));
        }

        let relative = &name[root.len() + 1..];
        let is_required = required.contains(&name);
        let is_stdlib = relative.starts_with("share/arandu/stdlib/");

        if !is_required && !is_stdlib {
            return Err(format!("unexpected zip content: '{name}'"));
        }

        // Check unix permissions in external attributes
        let ext_attr = entry.unix_mode().unwrap_or(0);
        if ext_attr != 0 && (ext_attr & 0o170000) != 0 && (ext_attr & 0o170000) != 0o100000 {
            return Err(format!("unsupported zip entry type: '{name}'"));
        }

        file_count += 1;
        if name == format!("{root}/release-manifest.json") {
            manifest_bytes = Some(read_limited(&mut entry, MAX_MANIFEST_BYTES)?);
        }
    }

    let mut missing = Vec::new();
    for req in &required {
        if !seen.contains(&req.to_lowercase()) {
            missing.push(req.clone());
        }
    }
    if !missing.is_empty() {
        missing.sort();
        return Err(format!(
            "zip missing required entries: {}",
            missing.join(", ")
        ));
    }

    let stdlib_prefix = format!("{}/share/arandu/stdlib/", root.to_lowercase());
    let has_stdlib_aru = seen
        .iter()
        .any(|s| s.starts_with(&stdlib_prefix) && s.ends_with(".aru"));
    if !has_stdlib_aru {
        return Err("zip contains no stdlib .aru files".to_string());
    }

    let manifest_bytes =
        manifest_bytes.ok_or_else(|| "release-manifest.json was not found".to_string())?;
    verify_manifest_json(&manifest_bytes, version, target, "zip")?;

    Ok(ValidationStats {
        files: file_count,
        symlinks: 0,
        target: target.to_string(),
        version: version.to_string(),
    })
}

fn validate_path_safety(name: &str) -> Result<(), String> {
    if name.starts_with('/')
        || name.contains('\\')
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

fn verify_manifest_json(
    bytes: &[u8],
    version: &str,
    target: &str,
    archive_format: &str,
) -> Result<(), String> {
    let json: JsonValue =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid release-manifest.json: {e}"))?;

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
            "archive format mismatch in manifest: expected '{archive_format}', got {arch:?}"
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
            "components mismatch in manifest: expected {expected_components:?}, got {comp_strs:?}"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod bounded_read_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn rejects_archive_metadata_over_the_limit() {
        assert_eq!(read_limited(&mut Cursor::new(b"ok"), 2).unwrap(), b"ok");
        assert!(read_limited(&mut Cursor::new(b"too long"), 2).is_err());
    }
}
