use std::fs;
use std::path::Path;

use crate::archive::{create_archive, validate_archive, ArchiveOptions};

fn setup_synthetic_staging(dir: &Path, target: &str, is_windows: bool) {
    let bin_dir = dir.join("bin");
    let lib_dir = dir.join("lib").join(target);
    let stdlib_dir = dir.join("share/arandu/stdlib/core");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&lib_dir).unwrap();
    fs::create_dir_all(&stdlib_dir).unwrap();

    if is_windows {
        fs::write(bin_dir.join("arandu.exe"), b"exe").unwrap();
        fs::write(bin_dir.join("arandu_cli.exe"), b"cli").unwrap();
        fs::write(bin_dir.join("arandu-lsp.exe"), b"lsp").unwrap();
        fs::write(lib_dir.join("arandu_runtime.lib"), b"lib").unwrap();
    } else {
        fs::write(bin_dir.join("arandu_cli"), b"cli").unwrap();
        fs::write(bin_dir.join("arandu-lsp"), b"lsp").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("arandu_cli", bin_dir.join("arandu")).unwrap();
        #[cfg(not(unix))]
        fs::write(bin_dir.join("arandu"), b"cli").unwrap();
        fs::write(lib_dir.join("libarandu_runtime.a"), b"runtime").unwrap();
    }

    fs::write(dir.join("BLAKE3SUMS"), b"dummy").unwrap();
    fs::write(dir.join("LICENSE-MIT"), b"MIT").unwrap();
    fs::write(dir.join("LICENSE-APACHE"), b"Apache").unwrap();
    fs::write(stdlib_dir.join("math.aru"), b"module std.math\n").unwrap();

    let manifest = serde_json::json!({
        "schema": 1,
        "version": "0.1.7",
        "target": target,
        "components": ["arandu", "arandu-lsp", "runtime", "stdlib"],
        "archive": if is_windows { "zip" } else { "tar.gz" }
    });
    fs::write(
        dir.join("release-manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn test_tar_gz_creation_and_validation() {
    let tmp = tempfile_dir("test_tar_gz");
    let staging = tmp.join("arandu-0.1.7");
    setup_synthetic_staging(&staging, "x86_64-unknown-linux-gnu", false);

    let tar_out = tmp.join("arandu-0.1.7-x86_64-unknown-linux-gnu.tar.gz");
    let opts = ArchiveOptions {
        source: staging.clone(),
        output: tar_out.clone(),
        epoch: 1700000000,
        root: Some("arandu-0.1.7".to_string()),
    };

    create_archive(&opts).expect("create tar.gz failed");
    assert!(tar_out.is_file());

    let stats = validate_archive(
        &tar_out,
        "arandu-0.1.7",
        "0.1.7",
        "x86_64-unknown-linux-gnu",
    )
    .expect("validate tar.gz failed");

    assert_eq!(stats.target, "x86_64-unknown-linux-gnu");
    assert_eq!(stats.version, "0.1.7");
    assert!(stats.files >= 8);

    // Byte determinism test
    let tar_out2 = tmp.join("arandu-0.1.7-x86_64-unknown-linux-gnu-copy.tar.gz");
    let opts2 = ArchiveOptions {
        source: staging,
        output: tar_out2.clone(),
        epoch: 1700000000,
        root: Some("arandu-0.1.7".to_string()),
    };
    create_archive(&opts2).expect("create tar.gz copy failed");

    let b1 = fs::read(&tar_out).unwrap();
    let b2 = fs::read(&tar_out2).unwrap();
    assert_eq!(b1, b2, "tar.gz creation must be 100% byte deterministic");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_zip_creation_and_validation() {
    let tmp = tempfile_dir("test_zip");
    let staging = tmp.join("arandu-0.1.7");
    setup_synthetic_staging(&staging, "x86_64-pc-windows-msvc", true);

    let zip_out = tmp.join("arandu-0.1.7-x86_64-pc-windows-msvc.zip");
    let opts = ArchiveOptions {
        source: staging.clone(),
        output: zip_out.clone(),
        epoch: 1700000000,
        root: Some("arandu-0.1.7".to_string()),
    };

    create_archive(&opts).expect("create zip failed");
    assert!(zip_out.is_file());

    let stats = validate_archive(&zip_out, "arandu-0.1.7", "0.1.7", "x86_64-pc-windows-msvc")
        .expect("validate zip failed");

    assert_eq!(stats.target, "x86_64-pc-windows-msvc");
    assert_eq!(stats.version, "0.1.7");
    assert!(stats.files >= 8);

    // Byte determinism test
    let zip_out2 = tmp.join("arandu-0.1.7-x86_64-pc-windows-msvc-copy.zip");
    let opts2 = ArchiveOptions {
        source: staging,
        output: zip_out2.clone(),
        epoch: 1700000000,
        root: Some("arandu-0.1.7".to_string()),
    };
    create_archive(&opts2).expect("create zip copy failed");

    let b1 = fs::read(&zip_out).unwrap();
    let b2 = fs::read(&zip_out2).unwrap();
    assert_eq!(b1, b2, "zip creation must be 100% byte deterministic");

    let _ = fs::remove_dir_all(&tmp);
}

fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("arandu_test_{}_{}", prefix, std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}
