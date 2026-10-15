//! Process spawning, worker threadpool isolation, timeouts, and process-tree cleanup.

use arandu_codegen::testing::{TestEventV1, TestFailure, TestStatus};
use std::fs;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::test_runner::ipc::{
    create_ipc_pipe_pair, drain, empty_capture, join_capture, read_ipc_frame,
};
use crate::test_runner::reporters::report;
use crate::test_runner::types::RunnerOptions;

pub static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Registers Ctrl-C signal handler to set global cancellation flag.
pub fn install_ctrlc_handler() {
    #[cfg(windows)]
    {
        unsafe extern "system" fn handler(_type: u32) -> i32 {
            CANCELLED.store(true, Ordering::Release);
            1 // Handled
        }
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
    #[cfg(unix)]
    {
        // On Unix, default SIGINT terminates process group or can be caught if needed.
    }
}

pub fn run_cases(
    project: &Path,
    stdlib_root: &Path,
    mut cases: Vec<String>,
    options: &RunnerOptions,
) -> Result<bool, String> {
    if cases.is_empty() {
        let empty_events: Vec<TestEventV1> = Vec::new();
        report(&empty_events, options)?;
        return Ok(true);
    }

    deterministic_shuffle(&mut cases, options.seed);
    let cases = Arc::new(cases);
    let next = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let system_limit = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .saturating_mul(2)
        .clamp(1, 64);
    let workers = options.jobs.max(1).min(cases.len()).min(system_limit);
    let channel_bound = workers.saturating_mul(2).max(1);
    let (sender, receiver) = mpsc::sync_channel(channel_bound);
    let mut handles = Vec::new();
    let mut spawn_error = None;

    for _ in 0..workers {
        let cases = Arc::clone(&cases);
        let next = Arc::clone(&next);
        let stop = Arc::clone(&stop);
        let sender = sender.clone();
        let project = project.to_path_buf();
        let stdlib_root = stdlib_root.to_path_buf();
        let timeout = options.timeout;
        let fail_fast = options.fail_fast;
        let doc_tests = options.doc_tests;

        match thread::Builder::new()
            .name("arandu-test-worker".to_string())
            .spawn(move || {
                loop {
                    if CANCELLED.load(Ordering::Acquire)
                        || (fail_fast && stop.load(Ordering::Acquire))
                    {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::AcqRel);
                    let Some(id) = cases.get(index) else { break };
                    let sequence = u64::try_from(index).unwrap_or(u64::MAX);
                    let started = Instant::now();
                    let event = run_case_guarded(sequence, id, started, || {
                        run_case(&project, &stdlib_root, id, timeout, sequence, doc_tests)
                    });
                    if !matches!(event.status, TestStatus::Passed | TestStatus::Skipped) {
                        stop.store(true, Ordering::Release);
                    }
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            }) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                spawn_error = Some(error.to_string());
                break;
            }
        }
    }
    drop(sender);

    let mut events: Vec<_> = receiver.into_iter().collect();
    let mut join_error = false;
    for handle in handles {
        join_error |= handle.join().is_err();
    }

    events.sort_by(|left, right| left.id.cmp(&right.id));
    report(&events, options)?;

    if let Some(error) = spawn_error {
        return Err(format!("failed spawning test worker: {error}"));
    }
    if join_error {
        return Err("test worker terminated without a structured result".to_string());
    }

    Ok(events
        .iter()
        .all(|event| matches!(event.status, TestStatus::Passed | TestStatus::Skipped)))
}

fn run_case_guarded(
    sequence: u64,
    id: &str,
    started: Instant,
    run: impl FnOnce() -> TestEventV1,
) -> TestEventV1 {
    catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|payload| {
        failed_event(
            sequence,
            id,
            started,
            TestStatus::Crashed,
            format!("test worker panicked: {}", panic_message(&payload)),
        )
    })
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

fn run_case(
    project: &Path,
    stdlib_root: &Path,
    id: &str,
    timeout: Duration,
    sequence: u64,
    doc_tests: bool,
) -> TestEventV1 {
    let started = Instant::now();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_root = std::env::temp_dir().join(format!(
        "arandu-test-run-{}-{sequence}-{nonce}",
        blake3::hash(id.as_bytes()).to_hex()
    ));
    if let Err(error) = fs::create_dir(&temp_root) {
        return failed_event(
            sequence,
            id,
            started,
            TestStatus::Crashed,
            format!("failed creating test sandbox: {error}"),
        );
    }

    let (parent_reader, child_stdio) = match create_ipc_pipe_pair() {
        Ok(pair) => pair,
        Err(error) => {
            return failed_event(
                sequence,
                id,
                started,
                TestStatus::Crashed,
                format!("failed creating IPC pipe: {error}"),
            );
        }
    };

    let executable = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return failed_event(
                sequence,
                id,
                started,
                TestStatus::Crashed,
                error.to_string(),
            );
        }
    };

    let mut command = Command::new(executable);
    command.args([
        "test",
        project.to_string_lossy().as_ref(),
        "--exact",
        id,
        "--harness-child",
    ]);
    if doc_tests {
        command.arg("--doc");
    }
    command
        .env("ARANDU_TEST_SEQUENCE", sequence.to_string())
        .env("ARANDU_TEST_TEMP_ROOT", &temp_root)
        .env("ARANDU_STDLIB", stdlib_root)
        .stdin(child_stdio)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed_event(
                sequence,
                id,
                started,
                TestStatus::Crashed,
                format!("failed spawning child process: {error}"),
            );
        }
    };

    let stdout_handle = drain(child.stdout.take());
    let stderr_handle = drain(child.stderr.take());

    let (status_outcome, frame_result) =
        wait_and_read_frame(&mut child, parent_reader, timeout, sequence, id);

    let stdout = join_capture(stdout_handle);
    let stderr = join_capture(stderr_handle);
    let _ = fs::remove_dir_all(&temp_root);

    let (event_status, failure, secondary_failures, logs, logs_truncated) = match status_outcome {
        WaitOutcome::TimedOut => (
            TestStatus::TimedOut,
            Some(TestFailure::simple("test timed out")),
            Vec::new(),
            Vec::new(),
            false,
        ),
        WaitOutcome::Exited(exit) => match frame_result {
            Ok(event) if exit.success() && event.status == TestStatus::Passed => (
                TestStatus::Passed,
                None,
                event.secondary_failures,
                event.logs,
                event.logs_truncated,
            ),
            Ok(event) => (
                event.status,
                event.failure,
                event.secondary_failures,
                event.logs,
                event.logs_truncated,
            ),
            Err(error) => (
                TestStatus::Crashed,
                Some(TestFailure::simple(format!("protocol failure: {error}"))),
                Vec::new(),
                Vec::new(),
                false,
            ),
        },
    };

    TestEventV1 {
        sequence,
        id: id.into(),
        status: event_status,
        duration: started.elapsed(),
        stdout,
        stderr,
        failure,
        secondary_failures,
        logs,
        logs_truncated,
    }
}

enum WaitOutcome {
    Exited(std::process::ExitStatus),
    TimedOut,
}

fn wait_and_read_frame<R: Read + Send + 'static>(
    child: &mut Child,
    mut reader: R,
    timeout: Duration,
    sequence: u64,
    id: &str,
) -> (WaitOutcome, Result<TestEventV1, String>) {
    let (frame_tx, frame_rx) = mpsc::channel::<Result<TestEventV1, String>>();
    let id_clone = id.to_string();
    thread::spawn(move || {
        let result = read_ipc_frame(&mut reader, sequence, &id_clone);
        let _ = frame_tx.send(result);
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let frame_res = frame_rx
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap_or_else(|_| {
                        Err("child exited without sending IPC control frame".to_string())
                    });
                return (WaitOutcome::Exited(status), frame_res);
            }
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            _ => {
                kill_process_tree(child);
                let frame_res = frame_rx
                    .recv_timeout(Duration::from_millis(200))
                    .unwrap_or_else(|_| Err("process timed out without IPC frame".to_string()));
                return (WaitOutcome::TimedOut, frame_res);
            }
        }
    }
}

pub fn kill_process_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(unix)]
    {
        if let Ok(pid_i32) = i32::try_from(pid) {
            // SAFETY: `pid_i32` is the checked process id returned by `Child`.
            // The child is spawned as the leader of its own process group, so
            // `killpg` targets only that test and its descendants.
            let _ = unsafe { libc::killpg(pid_i32, libc::SIGKILL) };
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn failed_event(
    sequence: u64,
    id: &str,
    started: Instant,
    status: TestStatus,
    failure: String,
) -> TestEventV1 {
    TestEventV1 {
        sequence,
        id: id.into(),
        status,
        duration: started.elapsed(),
        stdout: empty_capture(),
        stderr: empty_capture(),
        failure: Some(TestFailure::simple(failure)),
        secondary_failures: Vec::new(),
        logs: Vec::new(),
        logs_truncated: false,
    }
}

pub fn deterministic_shuffle(cases: &mut [String], mut state: u64) {
    if state == 0 {
        return;
    }
    for index in (1..cases.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let upper = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let selected = usize::try_from(state % upper).unwrap_or(0);
        cases.swap(index, selected);
    }
}

pub fn atomic_write_file(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create report directory {}: {error}", parent.display()))?;
    }
    let staging = path.with_extension("arandu-test-staging");
    fs::write(&staging, content).map_err(|error| error.to_string())?;

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let src: Vec<u16> = staging
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let dst: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let res = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                src.as_ptr(),
                dst.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
        };
        if res == 0 {
            // Fallback if target file replace failed
            let _ = fs::remove_file(path);
            let _ = fs::rename(&staging, path);
        }
        let _ = fs::remove_file(&staging);
        Ok(())
    }

    #[cfg(not(windows))]
    {
        fs::rename(&staging, path).map_err(|error| {
            let _ = fs::remove_file(&staging);
            error.to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_panic_becomes_a_structured_crash_event() {
        let event = run_case_guarded(7, "panic-case", Instant::now(), || {
            panic!("synthetic runner panic")
        });
        assert_eq!(event.sequence, 7);
        assert_eq!(event.id, "panic-case");
        assert_eq!(event.status, TestStatus::Crashed);
        assert!(event.failure.is_some());
    }
}
