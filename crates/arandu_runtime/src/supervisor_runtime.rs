//! SL_R.1 — process supervisor host (fork/exec + wait + restart policy).
//!
//! Language policy is abort-in-process; blast radius is bounded by **worker
//! processes** supervised by a parent. This module implements that host side.
//!
//! `ar_rt_supervisor_destroy` closes the registry entry and then kills and reaps
//! every worker. Close is not detach: a child does not keep running without a
//! handle. Wait and kill take the per-worker lock only after releasing the
//! supervisor lock, so one blocked worker does not stall lifecycle of another.

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

struct Worker {
    child: Arc<Mutex<Child>>,
    /// Path bytes (for restart).
    path: Vec<u8>,
    restarts: i64,
    max_restarts: i64,
}

struct Supervisor {
    workers: Vec<Option<Worker>>,
    closed: bool,
}

type SharedSupervisor = Arc<Mutex<Supervisor>>;

static SUPERS: Mutex<Vec<Option<SharedSupervisor>>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Option<SharedSupervisor>>> {
    SUPERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn get_supervisor(id: i64) -> Option<SharedSupervisor> {
    if id < 0 {
        return None;
    }
    lock().get(id as usize).and_then(Clone::clone)
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
    let s = Arc::new(Mutex::new(Supervisor {
        workers: Vec::new(),
        closed: false,
    }));
    if let Some(idx) = g.iter().position(|x| x.is_none()) {
        g[idx] = Some(s);
        return idx as i64;
    }
    let id = g.len() as i64;
    g.push(Some(s));
    id
}

/// Destroy a supervisor and reap its workers.
///
/// The registry entry is removed before any `Child::wait`. In-flight wait or
/// kill of the same worker still serializes on that worker's lock.
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_supervisor_destroy(id: i64) {
    if id < 0 {
        return;
    }
    let Some(supervisor) = get_supervisor(id) else {
        return;
    };
    let workers = {
        let mut state = supervisor.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        std::mem::take(&mut state.workers)
    };
    {
        let mut table = lock();
        if let Some(slot) = table.get_mut(id as usize)
            && slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &supervisor))
        {
            *slot = None;
        }
    }
    for worker in workers.into_iter().flatten() {
        let mut child = worker.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
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
    let Some(supervisor) = get_supervisor(sup) else {
        return -1;
    };
    let mut super_slot = supervisor.lock().unwrap_or_else(|e| e.into_inner());
    if super_slot.closed {
        return -1;
    }
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
        child: Arc::new(Mutex::new(child)),
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
    let Some(supervisor) = get_supervisor(sup) else {
        return -1;
    };
    let (should_restart, path_bytes) = {
        let super_slot = supervisor.lock().unwrap_or_else(|e| e.into_inner());
        if super_slot.closed {
            return -1;
        }
        let Some(Some(w)) = super_slot.workers.get(worker as usize) else {
            return -1;
        };
        let mut child = w.child.lock().unwrap_or_else(|e| e.into_inner());
        match child.try_wait() {
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

        let mut pending = Some(new_child);
        let outcome = {
            let mut super_slot = supervisor.lock().unwrap_or_else(|e| e.into_inner());
            if !super_slot.closed
                && let Some(Some(w)) = super_slot.workers.get_mut(worker as usize)
            {
                w.child = Arc::new(Mutex::new(pending.take().expect("replacement child")));
                w.restarts += 1;
                Some(2)
            } else {
                None
            }
        };
        if let Some(code) = outcome {
            return code;
        }
        if let Some(mut child) = pending {
            let _ = child.kill();
            let _ = child.wait();
        }
        -1
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
    let Some(supervisor) = get_supervisor(sup) else {
        return -1;
    };
    let child = {
        let super_slot = supervisor.lock().unwrap_or_else(|e| e.into_inner());
        if super_slot.closed {
            return -1;
        }
        let Some(Some(w)) = super_slot.workers.get(worker as usize) else {
            return -1;
        };
        Arc::clone(&w.child)
    };
    let mut child = child.lock().unwrap_or_else(|e| e.into_inner());
    match child.wait() {
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
    let Some(supervisor) = get_supervisor(sup) else {
        return -1;
    };
    let child = {
        let super_slot = supervisor.lock().unwrap_or_else(|e| e.into_inner());
        if super_slot.closed {
            return -1;
        }
        let Some(Some(w)) = super_slot.workers.get(worker as usize) else {
            return -1;
        };
        Arc::clone(&w.child)
    };
    let mut child = child.lock().unwrap_or_else(|e| e.into_inner());
    match child.kill() {
        Ok(()) => {
            let _ = child.wait();
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

    /// One worker blocked in `wait` must not hold the supervisor lock across
    /// the other worker's `kill`.
    #[cfg(unix)]
    #[test]
    fn kill_is_not_blocked_by_another_workers_wait() {
        let dir = std::env::temp_dir().join(format!(
            "arandu-supervisor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("supervisor fixture dir");
        let pidfile = dir.join("slow.pid");
        let slow_script = dir.join("slow.sh");
        let other_script = dir.join("other.sh");
        std::fs::write(
            &slow_script,
            format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
                pidfile.display()
            ),
        )
        .expect("write waited sleeper");
        std::fs::write(&other_script, "#!/bin/sh\nexec sleep 30\n").expect("write other sleeper");
        use std::os::unix::fs::PermissionsExt;
        for script in [&slow_script, &other_script] {
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
                .expect("mark sleeper executable");
        }
        let slow_path = slow_script.to_string_lossy().into_owned();
        let other_path = other_script.to_string_lossy().into_owned();
        unsafe {
            let supervisor = ar_rt_supervisor_create();
            assert!(supervisor >= 0);
            let slow =
                ar_rt_supervisor_spawn(supervisor, slow_path.as_ptr(), slow_path.len() as i64, 0);
            let other =
                ar_rt_supervisor_spawn(supervisor, other_path.as_ptr(), other_path.len() as i64, 0);
            assert!(slow >= 0 && other >= 0, "spawn two sleepers");
            let waiter = std::thread::spawn(move || ar_rt_supervisor_wait(supervisor, slow));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !pidfile.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "sleeper did not publish its pid"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            let started = std::time::Instant::now();
            assert_eq!(ar_rt_supervisor_kill(supervisor, other), 0);
            assert!(started.elapsed() < std::time::Duration::from_millis(800));
            let pid: i32 = std::fs::read_to_string(&pidfile)
                .expect("pidfile")
                .trim()
                .parse()
                .expect("pid");
            let status = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status()
                .expect("signal the waited worker");
            assert!(status.success());
            let _ = waiter
                .join()
                .expect("wait returns after the process is signaled");
            ar_rt_supervisor_destroy(supervisor);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
