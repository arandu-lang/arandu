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

/// Exit status of `source_path` built through AMIR O2 (`emit-c --release`)
/// and the host C compiler, or `None` when no `cc` is available. `run`
/// rejects build profiles, so this is how a single file reaches O2.
#[allow(dead_code)]
pub fn c_release_exit_code(source_path: &std::path::Path) -> Option<Option<i32>> {
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
    Some(Command::new(&binary).status().expect("run binary").code())
}

pub fn cli_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arandu_cli"));
    command.env("ARANDU_RUNTIME_LIB", runtime_library());
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
