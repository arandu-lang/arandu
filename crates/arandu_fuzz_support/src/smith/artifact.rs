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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    pub kind: &'static str,
    pub scope: Option<String>,
    pub message: String,
    pub shrinkable: bool,
    pub backend: Option<&'static str>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timeout: bool,
    pub stderr: Option<String>,
}

impl Failure {
    pub fn new(kind: &'static str, message: impl Into<String>, shrinkable: bool) -> Self {
        Self {
            kind,
            scope: None,
            message: message.into(),
            shrinkable,
            backend: None,
            exit_code: None,
            signal: None,
            timeout: false,
            stderr: None,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn with_backend(mut self, backend: &'static str) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn with_exit_status(mut self, exit_code: Option<i32>, signal: Option<i32>) -> Self {
        self.exit_code = exit_code;
        self.signal = signal;
        self
    }

    pub fn with_timeout(mut self, timeout: bool) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_stderr(mut self, stderr: impl Into<String>) -> Self {
        self.stderr = Some(stderr.into());
        self
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.kind == other.kind && self.scope == other.scope && self.signal == other.signal
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

pub fn oracle_target_name(
    compare_c: bool,
    compare_wasm: bool,
    compare_llvm: bool,
    corpus_name: &str,
) -> &'static str {
    if corpus_name != "synthesized" {
        return "emi-corpus";
    }
    match (compare_c, compare_wasm, compare_llvm) {
        (false, false, false) => "synthesized",
        (true, false, false) => "synthesized-c",
        (false, true, false) => "synthesized-wasm",
        (false, false, true) => "synthesized-llvm",
        _ => "synthesized-all",
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
    let backend_str = artifact.failure.backend.unwrap_or_else(|| {
        if artifact.failure.kind.starts_with("cranelift") {
            "Cranelift"
        } else if artifact.failure.kind.starts_with("c-") {
            "C"
        } else if artifact.failure.kind.starts_with("wasm") {
            "Wasm"
        } else {
            "unknown"
        }
    });
    let exit_code_str = artifact
        .failure
        .exit_code
        .map_or(String::new(), |c| c.to_string());
    let signal_str = artifact
        .failure
        .signal
        .map_or(String::new(), |s| s.to_string());
    let metadata = format!(
        "target={}\nbackend={}\ncorpus={}\nseed=0x{:016x}\nfailure_kind={}\nfailure_scope={}\nexit_code={}\nsignal={}\ntimeout={}\nshrink_attempts={}\nshrink_reductions={}\nshrink_confirmed={}\nreplay={}\n",
        artifact.target,
        backend_str,
        artifact.corpus_name,
        artifact.seed,
        artifact.failure.kind,
        artifact.failure.scope.as_deref().unwrap_or(""),
        exit_code_str,
        signal_str,
        artifact.failure.timeout,
        artifact.shrink_attempts,
        artifact.shrink_reductions,
        artifact.shrink_confirmed,
        replay_command
    );
    let mut files: Vec<(&str, &[u8])> = vec![
        ("candidate.aru", artifact.source.as_bytes()),
        ("emi-candidate.aru", artifact.emi_candidate.as_bytes()),
        ("reproducer.seed", encoded_seed.as_bytes()),
        ("reproducer.bin", &seed_bytes),
        ("metadata.txt", metadata.as_bytes()),
        ("failure.txt", artifact.failure.message.as_bytes()),
    ];
    let stderr_bytes;
    if let Some(ref stderr) = artifact.failure.stderr {
        stderr_bytes = stderr.as_bytes();
        files.push(("stderr.txt", stderr_bytes));
    }
    for (name, contents) in files {
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
