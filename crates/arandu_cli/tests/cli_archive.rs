#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, File};
use std::path::Path;
use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn create_synthetic_tar(path: &Path, target: &str, root: &str) {
    let file = File::create(path).unwrap();
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);

    let required = [
        format!("{root}/bin/arandu_cli"),
        format!("{root}/bin/arandu-lsp"),
        format!("{root}/lib/{target}/libarandu_runtime.a"),
        format!("{root}/BLAKE3SUMS"),
        format!("{root}/LICENSE-MIT"),
        format!("{root}/LICENSE-APACHE"),
        format!("{root}/share/arandu/stdlib/core.aru"),
    ];

    for req in &required {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(4);
        header.set_cksum();
        tar.append_data(&mut header, req, &b"data"[..]).unwrap();
    }

    // Symlink bin/arandu -> arandu_cli
    let mut sym_header = tar::Header::new_gnu();
    sym_header.set_entry_type(tar::EntryType::Symlink);
    sym_header.set_mode(0o777);
    sym_header.set_size(0);
    sym_header.set_link_name("arandu_cli").unwrap();
    sym_header.set_cksum();
    tar.append_data(
        &mut sym_header,
        format!("{root}/bin/arandu"),
        std::io::empty(),
    )
    .unwrap();

    // Release manifest
    let manifest = serde_json::json!({
        "schema": 1,
        "version": "0.1.7",
        "target": target,
        "components": ["arandu", "arandu-lsp", "runtime", "stdlib"],
        "archive": "tar.gz"
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let mut man_header = tar::Header::new_gnu();
    man_header.set_entry_type(tar::EntryType::Regular);
    man_header.set_mode(0o644);
    man_header.set_size(manifest_bytes.len() as u64);
    man_header.set_cksum();
    tar.append_data(
        &mut man_header,
        format!("{root}/release-manifest.json"),
        &manifest_bytes[..],
    )
    .unwrap();

    let gz = tar.into_inner().unwrap();
    gz.finish().unwrap();
}

#[test]
fn archive_validate_missing_file_reports_error() {
    let output = run_cli(&["archive", "validate", "nonexistent_file.tar.gz"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("validate archive"));
}

#[test]
fn archive_validate_synthetic_tar_succeeds() {
    let tmp = std::env::temp_dir().join(format!("arandu_test_cli_tar_{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let archive_path = tmp.join("arandu-0.1.7-x86_64-unknown-linux-gnu.tar.gz");

    create_synthetic_tar(&archive_path, "x86_64-unknown-linux-gnu", "arandu-0.1.7");

    let output = run_cli(&["archive", "validate", archive_path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("archive ok:"));
    assert!(stdout.contains("x86_64-unknown-linux-gnu"));

    let _ = fs::remove_dir_all(&tmp);
}
