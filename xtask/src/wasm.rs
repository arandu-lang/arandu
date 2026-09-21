//! `build-wasm` — build `arandu_web` for `wasm32-unknown-unknown`.
//!
//! Cargo concatenates `[build].rustflags` from all config files, so the mold
//! linker flags from `~/.cargo/config.toml` leak through even when
//! `.cargo/config.toml` sets `[target.wasm32-unknown-unknown].rustflags = []`.
//!
//! The only reliable workaround is to force `RUSTFLAGS=""` via the environment,
//! which has higher priority than any config file.
//!
//! Usage:
//!   cargo run -p xtask -- build-wasm [--release] [-- `<extra cargo args>`]

use std::path::Path;
use std::process::{Command, ExitStatus};

pub fn run(root: &Path, mut args: impl Iterator<Item = String>) -> i32 {
    let mut release = false;
    let mut extra: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    for arg in &mut args {
        if after_dashdash {
            extra.push(arg);
        } else if arg == "--" {
            after_dashdash = true;
        } else if arg == "--release" {
            release = true;
        } else {
            extra.push(arg);
        }
    }

    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        // Clear RUSTFLAGS so host linker flags (mold) don't reach wasm-ld.
        .env("RUSTFLAGS", "")
        .args([
            "build",
            "-p",
            "arandu_web",
            "--target",
            "wasm32-unknown-unknown",
        ]);

    if release {
        cmd.arg("--release");
    }

    if !extra.is_empty() {
        cmd.args(&extra);
    }

    let status: ExitStatus = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("build-wasm: failed to spawn cargo: {e}");
            return 1;
        }
    };

    if status.success() {
        let profile = if release { "release" } else { "debug" };
        let wasm = root
            .join("target/wasm32-unknown-unknown")
            .join(profile)
            .join("arandu_web.wasm");
        println!("build-wasm: ok ({})", wasm.display());
        0
    } else {
        eprintln!(
            "build-wasm: cargo exited with status {}",
            status.code().unwrap_or(-1)
        );
        1
    }
}
