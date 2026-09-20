//! Aggregate release assets preparation, cross-hashing, and manifest generation (RFC 0020).

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

const TARGETS: &[(&str, &str)] = &[
    ("x86_64-unknown-linux-gnu", "tar.gz"),
    ("aarch64-apple-darwin", "tar.gz"),
    ("x86_64-pc-windows-msvc", "zip"),
];

pub fn prepare_release_assets(
    directory: &Path,
    version: &str,
    tag: &str,
    commit: &str,
) -> Result<(), String> {
    if tag != format!("v{version}") {
        return Err(format!(
            "tag '{tag}' does not match version '{version}' (expected 'v{version}')"
        ));
    }

    if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("commit must be a 40-character lowercase hexadecimal SHA-1".to_string());
    }

    let mut assets = Vec::new();
    let mut blake_lines = Vec::new();
    let mut sha_lines = Vec::new();

    for &(target, extension) in TARGETS {
        let name = format!("arandu-{version}-{target}.{extension}");
        let archive_path = directory.join(&name);

        if !archive_path.is_file() {
            return Err(format!(
                "missing release archive in directory: '{}'",
                archive_path.display()
            ));
        }

        let archive_bytes = fs::read(&archive_path).map_err(|e| {
            format!(
                "failed to read release archive '{}': {e}",
                archive_path.display()
            )
        })?;

        // Read and verify SHA256 sidecars
        let sha256_file = directory.join(format!("{name}.sha256"));
        let sha256sum_file = directory.join(format!("{name}.sha256sum"));
        let expected_sha256 = read_hex_sidecar(&sha256_file)?;
        let sum_sha256 = read_sum_sidecar(&sha256sum_file, &name)?;
        if expected_sha256 != sum_sha256 {
            return Err(format!(
                "SHA-256 sidecars disagree for '{name}': '{expected_sha256}' vs '{sum_sha256}'"
            ));
        }

        let mut sha_hasher = Sha256::new();
        sha_hasher.update(&archive_bytes);
        let actual_sha256: String = sha_hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual_sha256 != expected_sha256 {
            return Err(format!(
                "computed SHA-256 mismatch for '{name}': computed '{actual_sha256}' vs sidecar '{expected_sha256}'"
            ));
        }

        // Read and verify BLAKE3 sidecars
        let blake3_file = directory.join(format!("{name}.blake3"));
        let blake3sum_file = directory.join(format!("{name}.blake3sum"));
        let expected_blake3 = read_hex_sidecar(&blake3_file)?;
        let sum_blake3 = read_sum_sidecar(&blake3sum_file, &name)?;
        if expected_blake3 != sum_blake3 {
            return Err(format!(
                "BLAKE3 sidecars disagree for '{name}': '{expected_blake3}' vs '{sum_blake3}'"
            ));
        }

        let actual_blake3 = blake3::hash(&archive_bytes).to_hex().to_string();
        if actual_blake3 != expected_blake3 {
            return Err(format!(
                "computed BLAKE3 mismatch for '{name}': computed '{actual_blake3}' vs sidecar '{expected_blake3}'"
            ));
        }

        assets.push(serde_json::json!({
            "archive": name,
            "blake3": actual_blake3,
            "sha256": actual_sha256,
            "size": archive_bytes.len(),
            "target": target,
        }));

        blake_lines.push(format!("{actual_blake3}  {name}\n"));
        sha_lines.push(format!("{actual_sha256}  {name}\n"));
    }

    let manifest = serde_json::json!({
        "assets": assets,
        "commit": commit,
        "schema": 1,
        "tag": tag,
        "version": version,
    });

    let manifest_str = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize release-manifest.json: {e}"))?;

    fs::write(
        directory.join("release-manifest.json"),
        format!("{manifest_str}\n"),
    )
    .map_err(|e| format!("failed to write release-manifest.json: {e}"))?;

    fs::write(directory.join("BLAKE3SUMS"), blake_lines.concat())
        .map_err(|e| format!("failed to write BLAKE3SUMS: {e}"))?;

    fs::write(directory.join("SHA256SUMS"), sha_lines.concat())
        .map_err(|e| format!("failed to write SHA256SUMS: {e}"))?;

    Ok(())
}

fn read_hex_sidecar(path: &Path) -> Result<String, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read sidecar file '{}': {e}", path.display()))?;
    let trimmed = content.trim().to_lowercase();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid hex digest in '{}'", path.display()));
    }
    Ok(trimmed)
}

fn read_sum_sidecar(path: &Path, expected_name: &str) -> Result<String, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read sum file '{}': {e}", path.display()))?;
    let parts: Vec<&str> = content.split_whitespace().collect();
    if parts.len() != 2 || parts[1].trim_start_matches('*') != expected_name {
        return Err(format!(
            "invalid checksum format in '{}' (expected '<hash>  {expected_name}')",
            path.display()
        ));
    }
    let digest = parts[0].to_lowercase();
    if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid hex digest in '{}'", path.display()));
    }
    Ok(digest)
}
