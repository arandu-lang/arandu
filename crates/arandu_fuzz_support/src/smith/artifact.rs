//! Artifact handling, failure representations, and test shrinker integration.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const AUTOMATIC_SHRINK_BUDGET: usize = 12;
pub static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn shrink_and_confirm(
    source: &str,
    max_attempts: usize,
    mut reproduces: impl FnMut(&str) -> bool,
) -> (crate::shrinker::ShrinkResult, bool) {
    let minimized =
        crate::shrinker::shrink_source_with_budget(source, max_attempts, &mut reproduces);
    let confirmed = reproduces(&minimized.source);
    (minimized, confirmed)
}

#[derive(Debug)]
pub struct Failure {
    pub kind: &'static str,
    pub scope: Option<String>,
    pub message: String,
    pub shrinkable: bool,
}

impl Failure {
    pub fn new(kind: &'static str, message: impl Into<String>, shrinkable: bool) -> Self {
        Self {
            kind,
            scope: None,
            message: message.into(),
            shrinkable,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.kind == other.kind && self.scope == other.scope
    }
}

pub fn panic_payload_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else {
        "non-string panic payload".to_owned()
    }
}

pub fn catch_backend_panic<T>(
    backend: &'static str,
    level: arandu_mir::OptLevel,
    execute: impl FnOnce() -> T,
) -> Result<T, Failure> {
    catch_unwind(AssertUnwindSafe(execute)).map_err(|payload| {
        let kind = match backend {
            "Cranelift" => "cranelift-panicked",
            "C" => "c-backend-panicked",
            "Wasm" => "wasm-backend-panicked",
            _ => "backend-panicked",
        };
        Failure::new(
            kind,
            format!(
                "{backend} panicked at {level:?}: {}",
                panic_payload_message(payload)
            ),
            true,
        )
        .with_scope(format!("{level:?}"))
    })
}

pub fn artifact_root() -> PathBuf {
    std::env::var_os("ARANDU_FUZZ_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("arandu-smith-artifacts"))
}

pub fn oracle_target_name(compare_c: bool, compare_wasm: bool, corpus_name: &str) -> &'static str {
    if corpus_name != "synthesized" {
        return "emi-corpus";
    }
    match (compare_c, compare_wasm) {
        (false, false) => "synthesized",
        (true, false) => "synthesized-c",
        (false, true) => "synthesized-wasm",
        (true, true) => "synthesized-all",
    }
}

#[allow(clippy::too_many_arguments)]
pub struct FailureArtifact<'a> {
    pub target: &'a str,
    pub corpus_name: &'a str,
    pub seed: u64,
    pub failure: &'a Failure,
    pub source: &'a str,
    pub emi_candidate: &'a str,
    pub shrink_attempts: usize,
    pub shrink_reductions: usize,
    pub shrink_confirmed: bool,
}

pub fn write_failure_artifact(
    root: &Path,
    artifact: FailureArtifact<'_>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .map_err(|error| format!("create artifact directory {}: {error}", root.display()))?;
    let directory = (0..8)
        .find_map(|_| {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let candidate = root.join(format!(
                "{target}-{seed:016x}-{}-{id}",
                std::process::id(),
                target = artifact.target,
                seed = artifact.seed
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => Some(Ok(candidate)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(format!(
                    "create candidate directory {}: {error}",
                    candidate.display()
                ))),
            }
        })
        .ok_or_else(|| "could not allocate a unique candidate directory".to_owned())??;

    let seed_file = directory.join("reproducer.seed");
    let seed_bytes = artifact.seed.to_le_bytes();
    let encoded_seed = format!(
        "encoding=hex\n{}\n",
        seed_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let replay_command = format!(
        "cargo run --locked -p xtask -- run-fuzz-seed {} {:?}",
        artifact.target,
        seed_file.display().to_string()
    );
    let metadata = format!(
        "target={}\ncorpus={}\nseed=0x{:016x}\nfailure_kind={}\nfailure_scope={}\nshrink_attempts={}\nshrink_reductions={}\nshrink_confirmed={}\nreplay={}\n",
        artifact.target,
        artifact.corpus_name,
        artifact.seed,
        artifact.failure.kind,
        artifact.failure.scope.as_deref().unwrap_or(""),
        artifact.shrink_attempts,
        artifact.shrink_reductions,
        artifact.shrink_confirmed,
        replay_command
    );
    for (name, contents) in [
        ("candidate.aru", artifact.source.as_bytes()),
        ("emi-candidate.aru", artifact.emi_candidate.as_bytes()),
        ("reproducer.seed", encoded_seed.as_bytes()),
        ("reproducer.bin", &seed_bytes),
        ("metadata.txt", metadata.as_bytes()),
        ("failure.txt", artifact.failure.message.as_bytes()),
    ] {
        std::fs::write(directory.join(name), contents)
            .map_err(|error| format!("write candidate artifact {name}: {error}"))?;
    }
    Ok(directory)
}

pub struct TemporaryArtifacts(pub Vec<PathBuf>);

impl Drop for TemporaryArtifacts {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}
