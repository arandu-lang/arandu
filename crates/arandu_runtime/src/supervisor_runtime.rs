//! SL_R.1 — process supervisor host (fork/exec + wait + restart policy).
//!
//! Language policy is abort-in-process; blast radius is bounded by **worker
//! processes** supervised by a parent. This module implements that host side.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

struct Worker {
    child: Child,
    /// Path bytes (for restart).
    path: Vec<u8>,
    restarts: i64,
    max_restarts: i64,
}

struct Supervisor {
    workers: Vec<Option<Worker>>,
}

static SUPERS: Mutex<Vec<Option<Supervisor>>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Option<Supervisor>>> {
    SUPERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn checked_path_len(length: i64) -> Option<usize> {
    let length = usize::try_from(length).ok()?;
    (length <= isize::MAX as usize).then_some(length)
}

/// Create a supervisor. Returns id >= 0.
///
/// # Safety
/// C ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_create() -> i64 {
    let mut g = lock();
    let s = Supervisor {
        workers: Vec::new(),
    };
    if let Some(idx) = g.iter().position(|x| x.is_none()) {
        g[idx] = Some(s);
        return idx as i64;
    }
    let id = g.len() as i64;
    g.push(Some(s));
    id
}

/// Destroy supervisor (does not kill children).
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_destroy(id: i64) {
    if id < 0 {
        return;
    }
    let mut g = lock();
    if let Some(slot) = g.get_mut(id as usize) {
        *slot = None;
    }
}

/// Spawn `path` as a worker (fat str: ptr + len). Alias used when the language
/// lowers `path: str` as two ABI slots after `sup`.
///
/// # Safety
/// Fat path string; `max_restarts` >= 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_spawn_str(
    sup: i64,
    path_ptr: *const u8,
    path_len: i64,
    max_restarts: i64,
) -> i64 {
    unsafe { ar_rt_supervisor_spawn(sup, path_ptr, path_len, max_restarts) }
}

/// Spawn `path` as a worker process (no args). Returns worker id >= 0 or -1.
///
/// # Safety
/// Fat path string; `max_restarts` >= 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_spawn(
    sup: i64,
    path_ptr: *const u8,
    path_len: i64,
    max_restarts: i64,
) -> i64 {
    if sup < 0 || path_ptr.is_null() || path_len <= 0 || max_restarts < 0 {
        return -1;
    }
    let Some(path_len) = checked_path_len(path_len) else {
        return -1;
    };
    let path_bytes = unsafe { std::slice::from_raw_parts(path_ptr, path_len) };
    let path = match std::str::from_utf8(path_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let mut g = lock();
    let Some(Some(super_slot)) = g.get_mut(sup as usize) else {
        return -1;
    };
    let child = match Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return -1,
    };
    let worker = Worker {
        child,
        path: path_bytes.to_vec(),
        restarts: 0,
        max_restarts,
    };
    if let Some(idx) = super_slot.workers.iter().position(|w| w.is_none()) {
        super_slot.workers[idx] = Some(worker);
        return idx as i64;
    }
    let id = super_slot.workers.len() as i64;
    super_slot.workers.push(Some(worker));
    id
}

/// Non-blocking poll: if worker exited, optionally restart.
/// Returns:
/// - `0` still running
/// - `1` exited (and not restarted)
/// - `2` exited and restarted
/// - `-1` error
///
/// # Safety
/// Handles from create/spawn.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_poll(sup: i64, worker: i64) -> i64 {
    if sup < 0 || worker < 0 {
        return -1;
    }
    let (should_restart, path_bytes) = {
        let mut g = lock();
        let Some(Some(super_slot)) = g.get_mut(sup as usize) else {
            return -1;
        };
        let Some(Some(w)) = super_slot.workers.get_mut(worker as usize) else {
            return -1;
        };
        match w.child.try_wait() {
            Ok(None) => return 0,
            Ok(Some(_status)) => {
                if w.restarts < w.max_restarts {
                    (true, w.path.clone())
                } else {
                    return 1;
                }
            }
            Err(_) => return -1,
        }
    };

    if should_restart {
        let path = match std::str::from_utf8(&path_bytes) {
            Ok(s) => s,
            Err(_) => return -1,
        };
        let new_child = match Command::new(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return 1,
        };

        let mut g = lock();
        let Some(Some(super_slot)) = g.get_mut(sup as usize) else {
            return -1;
        };
        let Some(Some(w)) = super_slot.workers.get_mut(worker as usize) else {
            return -1;
        };
        w.child = new_child;
        w.restarts += 1;
        2
    } else {
        1
    }
}

/// Blocking wait for worker exit (no restart). Returns exit code or -1.
///
/// # Safety
/// Handles valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_wait(sup: i64, worker: i64) -> i64 {
    if sup < 0 || worker < 0 {
        return -1;
    }
    let mut g = lock();
    let Some(Some(super_slot)) = g.get_mut(sup as usize) else {
        return -1;
    };
    let Some(Some(w)) = super_slot.workers.get_mut(worker as usize) else {
        return -1;
    };
    match w.child.wait() {
        Ok(status) => status.code().unwrap_or(-1) as i64,
        Err(_) => -1,
    }
}

/// Kill a worker (SIGKILL on Unix).
///
/// # Safety
/// Handles valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_kill(sup: i64, worker: i64) -> i64 {
    if sup < 0 || worker < 0 {
        return -1;
    }
    let mut g = lock();
    let Some(Some(super_slot)) = g.get_mut(sup as usize) else {
        return -1;
    };
    let Some(Some(w)) = super_slot.workers.get_mut(worker as usize) else {
        return -1;
    };
    match w.child.kill() {
        Ok(()) => {
            let _ = w.child.wait();
            0
        }
        Err(_) => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_length_must_fit_rust_slice_bounds() {
        assert_eq!(checked_path_len(1), Some(1));
        assert_eq!(checked_path_len(0), Some(0));
        assert_eq!(checked_path_len(-1), None);
        assert_eq!(
            checked_path_len(i64::MAX),
            usize::try_from(i64::MAX)
                .ok()
                .filter(|len| *len <= isize::MAX as usize)
        );
    }

    #[test]
    fn create_destroy() {
        unsafe {
            let s = ar_rt_supervisor_create();
            assert!(s >= 0);
            ar_rt_supervisor_destroy(s);
        }
    }

    #[test]
    fn spawn_and_wait() {
        unsafe {
            let s = ar_rt_supervisor_create();
            #[cfg(unix)]
            let path = if std::path::Path::new("/usr/bin/true").is_file() {
                "/usr/bin/true".to_string()
            } else {
                "/bin/true".to_string()
            };
            #[cfg(windows)]
            let path = std::env::var("WINDIR")
                .map(|windir| format!("{windir}\\System32\\whoami.exe"))
                .unwrap_or_else(|_| r"C:\\Windows\\System32\\whoami.exe".to_string());
            let w = ar_rt_supervisor_spawn(s, path.as_ptr(), path.len() as i64, 0);
            assert!(w >= 0, "spawn platform test worker");
            let code = ar_rt_supervisor_wait(s, w);
            assert_eq!(code, 0);
            ar_rt_supervisor_destroy(s);
        }
    }
}
