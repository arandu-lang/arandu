use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Atomically reserve a fresh directory. Clock resolution is not an identity:
/// parallel tests can observe the same timestamp on any platform.
#[allow(dead_code)] // This module is shared by integration binaries with different helpers.
pub fn temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..1024 {
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("{prefix}-{}-{sequence}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve a fresh temporary directory after 1024 attempts",
    ))
}

/// Host-independent view of how a child program ended.
///
/// A program result reaches the caller through the process exit status, and the
/// two hosts report it differently. Unix truncates the value passed to `exit`
/// to eight bits; Windows keeps the full 32-bit value passed to `ExitProcess`.
/// A trap differs even more, because Unix signals have no Windows equivalent:
/// `ExitStatus::code` is `None` on Unix, while Windows reports the fault status
/// of the terminated process, such as `STATUS_INTEGER_DIVIDE_BY_ZERO`
/// (`0xC0000094`) or `STATUS_ILLEGAL_INSTRUCTION` (`0xC000001D`).
///
/// Tests therefore compare this normalized view so the same expectation holds
/// on every host: the low eight bits for a returned value, or [`Self::Trapped`]
/// for a process that never returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramOutcome {
    /// The value the program returned, as a POSIX shell observes it.
    Returned(i32),
    /// The program never returned: fatal signal or hardware fault.
    Trapped,
}

#[allow(dead_code)]
impl ProgramOutcome {
    #[must_use]
    pub fn returned(self) -> Option<i32> {
        match self {
            Self::Returned(code) => Some(code),
            Self::Trapped => None,
        }
    }
}

/// `NTSTATUS` values Windows reports when a process dies instead of returning.
/// Exception and fast-fail codes live in the `0xC0000000` range, which an
/// eight-bit Unix exit status can never reach, so this predicate is meaningful
/// on both hosts.
#[allow(dead_code)]
const WINDOWS_FAULT_STATUSES: &[u32] = &[
    0xC000_0005, // STATUS_ACCESS_VIOLATION
    0xC000_001D, // STATUS_ILLEGAL_INSTRUCTION
    0xC000_0025, // STATUS_NONCONTINUABLE_EXCEPTION
    0xC000_008C, // STATUS_ARRAY_BOUNDS_EXCEEDED
    0xC000_008E, // STATUS_FLOAT_DIVIDE_BY_ZERO
    0xC000_0090, // STATUS_FLOAT_INVALID_OPERATION
    0xC000_0091, // STATUS_FLOAT_OVERFLOW
    0xC000_0094, // STATUS_INTEGER_DIVIDE_BY_ZERO
    0xC000_0095, // STATUS_INTEGER_OVERFLOW
    0xC000_00FD, // STATUS_STACK_OVERFLOW
    0xC000_0374, // STATUS_HEAP_CORRUPTION
    0xC000_0409, // STATUS_STACK_BUFFER_OVERRUN
    0xC000_0602, // STATUS_FAIL_FAST_EXCEPTION
];

/// Normalize a child's exit status; see [`ProgramOutcome`].
#[allow(dead_code)]
pub fn outcome(status: &std::process::ExitStatus) -> ProgramOutcome {
    match status.code() {
        None => ProgramOutcome::Trapped,
        Some(code) if WINDOWS_FAULT_STATUSES.contains(&(code as u32)) => ProgramOutcome::Trapped,
        Some(code) => ProgramOutcome::Returned(code & 0xFF),
    }
}

/// Outcome of `source_path` built through AMIR O2 (`emit-c --release`) and the
/// host C compiler, or `None` when no `cc` is available. `run` rejects build
/// profiles, so this is how a single file reaches O2.
#[allow(dead_code)]
pub fn c_release_outcome(source_path: &std::path::Path) -> Option<ProgramOutcome> {
    Command::new("cc").arg("--version").output().ok()?;
    let emitted = cli_command()
        .args(["emit-c", "--release"])
        .arg(source_path)
        .output()
        .expect("emit-c should run");
    assert!(
        emitted.status.success(),
        "emit-c --release: {}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let c_file = source_path.with_extension("release.c");
    let binary = source_path.with_extension("release.bin");
    std::fs::write(&c_file, &emitted.stdout).expect("write C");
    let compiled = Command::new("cc")
        .args(["-O1", "-w", "-fwrapv"])
        .arg(&c_file)
        .arg("-o")
        .arg(&binary)
        .arg("-lm")
        .output()
        .expect("cc should run");
    assert!(
        compiled.status.success(),
        "cc: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    Some(outcome(
        &Command::new(&binary).status().expect("run binary"),
    ))
}

/// The CLI in batch mode: suites never leave build daemons behind. Daemon
/// tests turn it back on with `ARANDU_DAEMON=1`.
pub fn cli_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arandu_cli"));
    command.env("ARANDU_RUNTIME_LIB", runtime_library());
    command.env("ARANDU_DAEMON", "0");
    command
}

fn runtime_library() -> PathBuf {
    if let Some(explicit) = std::env::var_os("ARANDU_RUNTIME_LIB") {
        return PathBuf::from(explicit);
    }
    let deps = std::env::current_exe()
        .expect("resolve integration-test executable")
        .parent()
        .expect("integration-test executable must live in target profile/deps")
        .to_path_buf();
    let (prefix, extension) = if cfg!(windows) {
        ("arandu_runtime-", "lib")
    } else {
        ("libarandu_runtime-", "a")
    };
    let mut candidates = fs::read_dir(&deps)
        .expect("read Cargo dependency artifacts")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.starts_with(prefix))
                && path.extension().and_then(|value| value.to_str()) == Some(extension)
        })
        .collect::<Vec<_>>();
    // A developer checkout can contain staticlibs from several feature sets
    // and source revisions. Cargo's hash is not chronological, so choosing the
    // lexicographically last filename can silently link a stale runtime.
    candidates.sort_by(|left, right| {
        let modified = |path: &PathBuf| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        };
        modified(left)
            .cmp(&modified(right))
            .then_with(|| left.cmp(right))
    });
    candidates
        .pop()
        .expect("build arandu_runtime staticlib before running AOT integration tests")
}
