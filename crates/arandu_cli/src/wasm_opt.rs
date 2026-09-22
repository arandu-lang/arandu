//! WebAssembly Binaryen (`wasm-opt`) optimization runner.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use crate::cli_error::CliFailure;

use std::sync::atomic::{AtomicU64, Ordering};

static WASM_OPT_COUNTER: AtomicU64 = AtomicU64::new(0);

struct PrivateTempDir(PathBuf);

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn create_private_temp_dir() -> io::Result<PrivateTempDir> {
    let root = std::env::temp_dir();
    let pid = std::process::id();
    for _ in 0..128 {
        let counter = WASM_OPT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("arandu-wasm-opt-{pid}-{counter}"));

        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(&path)
        };
        #[cfg(not(unix))]
        let result = fs::create_dir(&path);

        match result {
            Ok(()) => return Ok(PrivateTempDir(path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique wasm-opt temporary directory",
    ))
}

/// Check whether `wasm-opt` is present in the system `PATH`.
#[must_use]
pub fn is_wasm_opt_available() -> bool {
    Command::new("wasm-opt")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Optimize Wasm bytecode using `wasm-opt` if available.
///
/// If `wasm-opt` is found and either `release` or `opt` is true, runs `wasm-opt -O3`
/// on `wasm_bytes` and returns the optimized bytecode.
/// If `wasm-opt` is not found, logs an informational notice and returns the input
/// unchanged (graceful fallback per RFC 0014 §4.6 and §5.2).
pub fn optimize_wasm_if_available(
    wasm_bytes: Vec<u8>,
    opt: bool,
    release: bool,
    verbose: bool,
) -> Result<Vec<u8>, CliFailure> {
    if !opt && !release {
        return Ok(wasm_bytes);
    }

    if !is_wasm_opt_available() {
        if verbose {
            eprintln!(
                "[wasm-opt] wasm-opt not found in PATH; skipping Binaryen release optimization pass"
            );
        } else {
            tracing::debug!("wasm-opt not found in PATH; skipping Binaryen pass");
        }
        return Ok(wasm_bytes);
    }

    let temp_dir = create_private_temp_dir().map_err(|error| {
        CliFailure::operational(
            "create wasm-opt temporary directory",
            None,
            error.to_string(),
        )
    })?;
    let in_path = temp_dir.0.join("input.wasm");
    let out_path = temp_dir.0.join("output.wasm");

    fs::write(&in_path, &wasm_bytes).map_err(|e| {
        CliFailure::operational("write wasm-opt input", Some(in_path.clone()), e.to_string())
    })?;

    let opt_flag = if release { "-O3" } else { "-O2" };
    let status = Command::new("wasm-opt")
        .arg(opt_flag)
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        .status();

    match status {
        Ok(s) if s.success() => {
            let optimized = fs::read(&out_path).map_err(|e| {
                CliFailure::operational(
                    "read wasm-opt output",
                    Some(out_path.clone()),
                    e.to_string(),
                )
            })?;
            if verbose {
                let original_len = wasm_bytes.len();
                let opt_len = optimized.len();
                let reduction = if original_len > 0 {
                    (1.0 - (opt_len as f64 / original_len as f64)) * 100.0
                } else {
                    0.0
                };
                eprintln!(
                    "[wasm-opt] optimized wasm bytecode: {original_len} bytes -> {opt_len} bytes ({reduction:.1}% reduction)"
                );
            }
            Ok(optimized)
        }
        Ok(s) => {
            eprintln!("[wasm-opt] wasm-opt exited with status {s}; using unoptimized bytecode");
            Ok(wasm_bytes)
        }
        Err(e) => {
            eprintln!("[wasm-opt] failed to execute wasm-opt ({e}); using unoptimized bytecode");
            Ok(wasm_bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_directory_is_private_and_removed_on_drop() {
        let dir = create_private_temp_dir().unwrap();
        let path = dir.0.clone();
        assert!(path.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }

        drop(dir);
        assert!(!path.exists());
    }
}
