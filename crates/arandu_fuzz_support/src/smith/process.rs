//! Process supervision, timeouts, and JIT stdout/stderr capture.

use std::cell::RefCell;
use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::process_job::terminate_process_group;
#[cfg(windows)]
use crate::process_job::ProcessJob as BackendProcessJob;

pub const BACKEND_PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_CAPTURED_PROCESS_OUTPUT: usize = 1024 * 1024;
pub const MAX_RESULT_CHANNEL_BYTES: usize = 64;
pub const PREFLIGHT_PROCESS_GROUP_ENV: &str = "ARANDU_SMITH_PREFLIGHT_PROCESS_GROUP";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapturedJitOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub invalid: bool,
}

thread_local! {
    pub static JIT_STDOUT_CAPTURE: RefCell<CapturedJitOutput> = const {
        RefCell::new(CapturedJitOutput {
            bytes: Vec::new(),
            truncated: false,
            invalid: false,
        })
    };
    pub static JIT_STDERR_CAPTURE: RefCell<CapturedJitOutput> = const {
        RefCell::new(CapturedJitOutput {
            bytes: Vec::new(),
            truncated: false,
            invalid: false,
        })
    };
}

pub extern "C" fn capture_jit_println(ptr: *const u8, len: i64) {
    JIT_STDOUT_CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        let Ok(len) = usize::try_from(len) else {
            capture.invalid = true;
            return;
        };
        if len > 0 && ptr.is_null() {
            capture.invalid = true;
            return;
        }

        let remaining = MAX_CAPTURED_PROCESS_OUTPUT.saturating_sub(capture.bytes.len());
        let copied = len.min(remaining);
        if copied > 0 {
            // SAFETY: the generated language string ABI guarantees `ptr`
            // addresses `len` readable bytes whenever `len` is positive.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, copied) };
            capture.bytes.extend_from_slice(bytes);
        }
        if copied < len {
            capture.truncated = true;
            return;
        }
        if capture.bytes.len() < MAX_CAPTURED_PROCESS_OUTPUT {
            capture.bytes.push(b'\n');
        } else {
            capture.truncated = true;
        }
    });
}

pub extern "C" fn capture_jit_eprint(ptr: *const u8, len: i64) {
    JIT_STDERR_CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        let Ok(len) = usize::try_from(len) else {
            capture.invalid = true;
            return;
        };
        if len > 0 && ptr.is_null() {
            capture.invalid = true;
            return;
        }

        let remaining = MAX_CAPTURED_PROCESS_OUTPUT.saturating_sub(capture.bytes.len());
        let copied = len.min(remaining);
        if copied > 0 {
            // SAFETY: the generated language string ABI guarantees `ptr`
            // addresses `len` readable bytes whenever `len` is positive.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, copied) };
            capture.bytes.extend_from_slice(bytes);
        }
        capture.truncated |= copied < len;
    });
}

pub extern "C" fn synthesized_args_len() -> i64 {
    i64::try_from(super::types::SYNTHESIZED_PROGRAM_ARGS.len() + 1)
        .expect("fuzz argument count fits in i64")
}

pub fn synthesized_arg(index: isize) -> arandu_runtime::rt_runtime::ArFatStr {
    let argument = if index == 0 {
        "arandu-smith"
    } else {
        usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| super::types::SYNTHESIZED_PROGRAM_ARGS.get(index))
            .copied()
            .unwrap_or("")
    };
    arandu_runtime::rt_runtime::ArFatStr {
        ptr: if argument.is_empty() {
            std::ptr::null_mut()
        } else {
            argument.as_ptr().cast_mut()
        },
        len: isize::try_from(argument.len()).expect("fuzz argument length fits in isize"),
    }
}

#[cfg(not(windows))]
pub unsafe extern "C" fn synthesized_args_arg(
    index: isize,
) -> arandu_runtime::rt_runtime::ArFatStr {
    synthesized_arg(index)
}

#[cfg(windows)]
pub unsafe extern "sysv64" fn synthesized_args_arg(
    index: isize,
) -> arandu_runtime::rt_runtime::ArFatStr {
    synthesized_arg(index)
}

pub fn reset_jit_stdout_capture() {
    JIT_STDOUT_CAPTURE.with(|capture| *capture.borrow_mut() = CapturedJitOutput::default());
    JIT_STDERR_CAPTURE.with(|capture| *capture.borrow_mut() = CapturedJitOutput::default());
}

pub fn take_jit_stdout_capture() -> CapturedJitOutput {
    JIT_STDOUT_CAPTURE.with(|capture| std::mem::take(&mut *capture.borrow_mut()))
}

pub fn take_jit_stderr_capture() -> CapturedJitOutput {
    JIT_STDERR_CAPTURE.with(|capture| std::mem::take(&mut *capture.borrow_mut()))
}

pub fn parse_backend_result(stdout: &[u8], truncated: bool, producer: &str) -> Result<i32, String> {
    if truncated {
        return Err(format!("{producer} exceeded the capture limit"));
    }
    let stdout = std::str::from_utf8(stdout)
        .map_err(|error| format!("{producer} emitted invalid UTF-8: {error}"))?;
    let result = stdout
        .strip_suffix('\n')
        .ok_or_else(|| format!("{producer} did not terminate its result with a newline"))?;
    if result.is_empty() || result.contains('\n') || result.contains('\r') {
        return Err(format!("{producer} did not emit exactly one result line"));
    }
    result
        .parse()
        .map_err(|error| format!("parse {producer} result {result:?}: {error}"))
}

pub fn read_result_channel(path: &std::path::Path, producer: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("read {producer} result channel: {error}"))?;
    let mut bytes = Vec::new();
    let read_limit = u64::try_from(MAX_RESULT_CHANNEL_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {producer} result channel: {error}"))?;
    if bytes.len() > MAX_RESULT_CHANNEL_BYTES {
        return Err(format!(
            "{producer} result channel exceeded the {MAX_RESULT_CHANNEL_BYTES}-byte limit"
        ));
    }
    Ok(bytes)
}

#[derive(Debug)]
pub struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub struct CapturedStream {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

pub fn capture_stream(mut reader: impl Read) -> io::Result<CapturedStream> {
    let mut bytes = Vec::with_capacity(MAX_CAPTURED_PROCESS_OUTPUT.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_CAPTURED_PROCESS_OUTPUT.saturating_sub(bytes.len());
        let retained = count.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < count;
    }
    Ok(CapturedStream { bytes, truncated })
}

pub fn join_capture(
    reader: thread::JoinHandle<io::Result<CapturedStream>>,
    stream_name: &str,
) -> Result<CapturedStream, String> {
    reader
        .join()
        .map_err(|_| format!("{stream_name} capture thread panicked"))?
        .map_err(|error| format!("failed to collect {stream_name}: {error}"))
}

pub fn captured_stderr(output: &BoundedOutput) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.stderr_truncated {
        text.push_str("\n[stderr truncated after 1048576 bytes]");
    }
    text
}

fn shares_preflight_process_group() -> bool {
    let parent = std::env::var(PREFLIGHT_PROCESS_GROUP_ENV)
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    parent == Some(std::os::unix::process::parent_id())
}

pub fn run_with_timeout(command: &mut Command, timeout: Duration) -> Result<BoundedOutput, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    let job = BackendProcessJob::new()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if !shares_preflight_process_group() {
            command.process_group(0);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start process: {error}"))?;
    #[cfg(windows)]
    if let Err(error) = job.assign(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout was not piped".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "child stderr was not piped".to_owned())?;
    let stdout_reader = thread::spawn(move || capture_stream(stdout));
    let stderr_reader = thread::spawn(move || capture_stream(stderr));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                #[cfg(unix)]
                if !shares_preflight_process_group() {
                    terminate_process_group(child.id());
                }
                #[cfg(windows)]
                job.terminate();
                let stdout = join_capture(stdout_reader, "stdout")?;
                let stderr = join_capture(stderr_reader, "stderr")?;
                return Ok(BoundedOutput {
                    status,
                    stdout: stdout.bytes,
                    stderr: stderr.bytes,
                    stdout_truncated: stdout.truncated,
                    stderr_truncated: stderr.truncated,
                });
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                #[cfg(unix)]
                terminate_process_group(if shares_preflight_process_group() {
                    std::os::unix::process::parent_id()
                } else {
                    child.id()
                });
                #[cfg(windows)]
                job.terminate();
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("process timed out after {timeout:?}"));
            }
            Err(error) => {
                #[cfg(unix)]
                if !shares_preflight_process_group() {
                    terminate_process_group(child.id());
                }
                #[cfg(windows)]
                job.terminate();
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("failed to wait for process: {error}"));
            }
        }
    }
}
