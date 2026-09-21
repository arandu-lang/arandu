//! Arandu workspace automation (xtask pattern).
//!
//! ```text
//! cargo run -p xtask -- check-diag-docs
//! cargo run -p xtask -- check-project-corpus
//! cargo run -p xtask -- check-project-churn
//! cargo run -p xtask -- check-project-performance
//! cargo run -p xtask -- help
//! ```

mod architecture;
mod archive;
mod churn;
mod corpus;
mod docs_taxonomy;
mod fuzz_regressions;
mod incremental;
mod line_endings;
mod performance;
mod release_assets;
mod release_contract;
mod slt6;
mod wasm;

use std::env;
use std::path::PathBuf;
use std::process;

fn main() {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "help".into());
    let code = match cmd.as_str() {
        "check-diag-docs" => cmd_check_diag_docs(),
        "check-docs-taxonomy" => docs_taxonomy::check(&workspace_root()),
        "check-project-corpus" => corpus::cmd_check_project_corpus(&workspace_root()),
        "check-project-churn" => churn::cmd_check_project_churn(&workspace_root()),
        "check-project-performance" => {
            performance::cmd_check_project_performance(&workspace_root())
        }
        "check-fuzz-regressions" => fuzz_regressions::check(&workspace_root()),
        "check-architecture" => architecture::check(&workspace_root()),
        "check-line-endings" => line_endings::check(&workspace_root()),
        "bench-incremental" => incremental::run(&workspace_root(), args),
        "run-fuzz-seed" => fuzz_regressions::run_one(args),
        "check-release-contract" => release_contract::check(&workspace_root(), args.next()),
        "prepare-release" => release_contract::prepare(&workspace_root(), args.next()),
        "check-slt6-sdk" => slt6::check(&workspace_root(), args),
        "package-archive" => cmd_package_archive(args),
        "validate-archive" => cmd_validate_archive(args),
        "prepare-release-assets" => cmd_prepare_release_assets(args),
        "build-wasm" => wasm::run(&workspace_root(), args),
        "help" | "-h" | "--help" => {
            print_help();
            0
        }
        other => {
            eprintln!("unknown xtask command: {other}");
            print_help();
            2
        }
    };
    process::exit(code);
}

fn print_help() {
    eprintln!(
        "\
xtask — Arandu workspace tasks

Commands:
  check-diag-docs   Bijection: DiagCode (user-facing) ↔ docs/errors/*.md
  check-docs-taxonomy  Validate permanent docs and the single-roadmap rule
  check-project-corpus  Validate S2 projects and incremental ↔ clean equivalence
  check-project-churn   Run deterministic S2 module and identity churn
  check-project-performance  Measure S2 cold/noop/edit and retention budgets
  check-fuzz-regressions  Run the versioned adversarial corpus with isolation
  check-architecture  Enforce compiler crate and effect boundaries
  check-line-endings  Reject CRLF or mixed text stored in the Git index
  bench-incremental  Measure five edit classes and prove native binary determinism
  check-release-contract  Validate component versions and an optional v* tag
  prepare-release    Update every Arandu component to one version atomically
  check-slt6-sdk     Exercise an installed SDK outside the repository
  package-archive   Create canonical deterministic .tar.gz or .zip (RFC 0020)
  validate-archive  Validate archive safety, layout, and release manifest
  prepare-release-assets  Generate SHA256SUMS, BLAKE3SUMS and release manifest
  build-wasm        Build arandu_web for wasm32-unknown-unknown (clears host linker flags)
  help              This message

Examples:
  cargo run -p xtask -- check-diag-docs
  cargo run -p xtask -- check-docs-taxonomy
  cargo run -p xtask -- check-project-corpus
  cargo run -p xtask -- check-project-churn
  cargo run -p xtask -- check-project-performance
  cargo run -p xtask -- check-fuzz-regressions
  cargo run -p xtask -- check-architecture
  cargo run -p xtask -- check-line-endings
  cargo run -p xtask -- bench-incremental
  cargo run -p xtask -- check-release-contract [vX.Y.Z[-rc.N]]
  cargo run -p xtask -- prepare-release X.Y.Z[-rc.N]
  cargo run -p xtask -- check-slt6-sdk --arandu PATH --work-dir DIR --evidence-dir DIR
  cargo run -p xtask -- build-wasm [--release]
  ./scripts/check-diag-docs.sh
"
    );
}

/// Source of truth = `DiagCode` enum; docs must match exactly (no manual code list).
fn cmd_check_diag_docs() -> i32 {
    let root = workspace_root();
    let docs_dir = root.join("docs/errors");
    let (missing, orphaned) = arandu_diagnostics::diag_doc_diff(&docs_dir);

    if missing.is_empty() && orphaned.is_empty() {
        let n = arandu_diagnostics::DiagCode::ALL
            .iter()
            .filter(|c| c.requires_error_doc())
            .count();
        println!("check-diag-docs: ok ({n} user-facing DiagCode(s) ↔ docs/errors)");
        return 0;
    }

    if !missing.is_empty() {
        eprintln!("error: missing docs/errors/{{CODE}}.md for DiagCode variants:");
        for code in &missing {
            eprintln!("  - docs/errors/{code}.md   (add doc when declaring DiagCode)");
        }
    }
    if !orphaned.is_empty() {
        eprintln!("error: orphaned docs (no matching DiagCode / not user-facing):");
        for doc in &orphaned {
            eprintln!("  - docs/errors/{doc}.md   (remove or rename after code change)");
        }
    }
    eprintln!();
    eprintln!("DiagCode is the single source of truth — do not maintain a parallel list.");
    1
}

fn workspace_root() -> PathBuf {
    // xtask lives at $ROOT/xtask — walk up from CARGO_MANIFEST_DIR.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("xtask parent = workspace root")
        .to_path_buf()
}

fn cmd_package_archive(mut args: impl Iterator<Item = String>) -> i32 {
    let mut source = None;
    let mut output = None;
    let mut epoch = None;
    let mut root = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--source" => source = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--epoch" => {
                if let Some(val) = args.next() {
                    epoch = val.parse::<u64>().ok();
                }
            }
            "--root" => root = args.next(),
            other if other.starts_with("--source=") => {
                source = Some(PathBuf::from(&other["--source=".len()..]));
            }
            other if other.starts_with("--output=") => {
                output = Some(PathBuf::from(&other["--output=".len()..]));
            }
            other if other.starts_with("--epoch=") => {
                epoch = other["--epoch=".len()..].parse::<u64>().ok();
            }
            other if other.starts_with("--root=") => {
                root = Some(other["--root=".len()..].to_string());
            }
            other if !other.starts_with('-') => {
                if source.is_none() {
                    source = Some(PathBuf::from(other));
                } else if output.is_none() {
                    output = Some(PathBuf::from(other));
                }
            }
            _ => {}
        }
    }

    let (Some(source), Some(output)) = (source, output) else {
        eprintln!("usage: cargo run -p xtask -- package-archive --source <dir> --output <archive> [--epoch <secs>] [--root <name>]");
        return 2;
    };

    let epoch = epoch.unwrap_or_else(|| {
        env::var("SOURCE_DATE_EPOCH")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1700000000)
    });

    let opts = archive::ArchiveOptions {
        source,
        output: output.clone(),
        epoch,
        root,
    };

    match archive::create_archive(&opts) {
        Ok(()) => {
            println!("package-archive: ok (output: {})", output.display());
            0
        }
        Err(err) => {
            eprintln!("package-archive: error: {err}");
            1
        }
    }
}

fn cmd_validate_archive(mut args: impl Iterator<Item = String>) -> i32 {
    let mut archive = None;
    let mut root = None;
    let mut version = None;
    let mut target = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--archive" => archive = args.next().map(PathBuf::from),
            "--root" => root = args.next(),
            "--version" => version = args.next(),
            "--target" => target = args.next(),
            other if other.starts_with("--root=") => {
                root = Some(other["--root=".len()..].to_string());
            }
            other if other.starts_with("--version=") => {
                version = Some(other["--version=".len()..].to_string());
            }
            other if other.starts_with("--target=") => {
                target = Some(other["--target=".len()..].to_string());
            }
            other if !other.starts_with('-') && archive.is_none() => {
                archive = Some(PathBuf::from(other));
            }
            _ => {}
        }
    }

    let (Some(archive), Some(root), Some(version), Some(target)) = (archive, root, version, target)
    else {
        eprintln!("usage: cargo run -p xtask -- validate-archive <path> --root <name> --version <ver> --target <target>");
        return 2;
    };

    match archive::validate_archive(&archive, &root, &version, &target) {
        Ok(stats) => {
            println!(
                "validate-archive: ok ({} files, {} symlinks, version={}, target={})",
                stats.files, stats.symlinks, stats.version, stats.target
            );
            0
        }
        Err(err) => {
            eprintln!("validate-archive: error: {err}");
            1
        }
    }
}

fn cmd_prepare_release_assets(mut args: impl Iterator<Item = String>) -> i32 {
    let mut directory = None;
    let mut version = None;
    let mut tag = None;
    let mut commit = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" | "--directory" => directory = args.next().map(PathBuf::from),
            "--version" => version = args.next(),
            "--tag" => tag = args.next(),
            "--commit" => commit = args.next(),
            other if other.starts_with("--version=") => {
                version = Some(other["--version=".len()..].to_string());
            }
            other if other.starts_with("--tag=") => {
                tag = Some(other["--tag=".len()..].to_string());
            }
            other if other.starts_with("--commit=") => {
                commit = Some(other["--commit=".len()..].to_string());
            }
            other if !other.starts_with('-') && directory.is_none() => {
                directory = Some(PathBuf::from(other));
            }
            _ => {}
        }
    }

    let (Some(directory), Some(version), Some(tag), Some(commit)) =
        (directory, version, tag, commit)
    else {
        eprintln!("usage: cargo run -p xtask -- prepare-release-assets <dir> --version <ver> --tag <tag> --commit <sha1>");
        return 2;
    };

    match release_assets::prepare_release_assets(&directory, &version, &tag, &commit) {
        Ok(()) => {
            println!(
                "prepare-release-assets: ok (directory: {}, version={})",
                directory.display(),
                version
            );
            0
        }
        Err(err) => {
            eprintln!("prepare-release-assets: error: {err}");
            1
        }
    }
}
