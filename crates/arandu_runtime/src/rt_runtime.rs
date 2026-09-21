//! SL_R.0 — cooperative multi-task host for debug JIT (i64 payload MVP).
//!
//! Complements [`crate::poll_runtime`] (single-coroutine poll/block_on).
//!
//! ## Model
//! - Explicit handles, no global language-level executor in user code beyond
//!   these host symbols (stdlib wraps them as `SyncExecutor`).
//! - `spawn` parks a coroutine state blob; `join` drives it with
//!   [`crate::poll_runtime::ar_co_block_on_i64`].
//! - Cooperative only: Pending spins (no OS reactor yet — SL_R.2).

use crate::poll_runtime::{ar_co_block_on_i64, ar_co_free};
use std::sync::Mutex;

/// Ownership of the coroutine blob changes exactly once at Pending -> Running.
/// Running stores no pointer: only the joining thread can poll/free the blob.
enum TaskState {
    Pending(*mut u8),
    Running { cancel_requested: bool },
    Completed(i64),
}

// SAFETY: pending blobs are accessed only while holding the task-table mutex.
// A join transfers the pointer to its own thread and leaves no pointer in the
// shared Running state. Cancellation can only mark that state for retirement.
unsafe impl Send for TaskState {}

type TaskTable = Mutex<Vec<Option<TaskState>>>;
static TASKS: TaskTable = Mutex::new(Vec::new());

/// Spawn a coroutine state onto the SyncExecutor queue. Returns handle (>= 0).
///
/// # Safety
/// `state` must be a valid coroutine blob exclusively transferred to this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_spawn_i64(state: *mut u8) -> i64 {
    spawn_on(&TASKS, state)
}

fn spawn_on(tasks: &TaskTable, state: *mut u8) -> i64 {
    if state.is_null() {
        std::process::abort();
    }
    let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
    let index = guard
        .iter()
        .position(Option::is_none)
        .unwrap_or(guard.len());
    let Ok(handle) = i64::try_from(index) else {
        std::process::abort();
    };
    if index == guard.len() {
        guard.push(Some(TaskState::Pending(state)));
    } else {
        guard[index] = Some(TaskState::Pending(state));
    }
    handle
}

/// Drive task to completion. Rejoining a completed live handle returns its
/// cached result. Cancellation during the join retires the handle on completion.
///
/// # Safety
/// `handle` must be live. Only one concurrent join may claim it. A cancel racing
/// with this call must run after the join has claimed the task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_join_i64(handle: i64) -> i64 {
    // SAFETY: the public caller provides the live, exclusively joined handle;
    // join_on owns the claimed blob until it has been polled and freed.
    unsafe { join_on(&TASKS, handle, |state| ar_co_block_on_i64(state)) }
}

unsafe fn join_on(tasks: &TaskTable, handle: i64, drive: impl FnOnce(*mut u8) -> i64) -> i64 {
    let Ok(index) = usize::try_from(handle) else {
        std::process::abort();
    };
    let state = {
        let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
        let Some(Some(slot)) = guard.get_mut(index) else {
            std::process::abort();
        };
        match slot {
            TaskState::Completed(result) => return *result,
            TaskState::Running { .. } => std::process::abort(),
            TaskState::Pending(state) => {
                let state = *state;
                *slot = TaskState::Running {
                    cancel_requested: false,
                };
                state
            }
        }
    };
    let result = drive(state);
    // SAFETY: the Pending -> Running transition transferred sole ownership to
    // this join; cancel_on cannot access or free the pointer while it is running.
    unsafe { ar_co_free(state) };
    let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
    let Some(slot) = guard.get_mut(index) else {
        std::process::abort();
    };
    match slot {
        Some(TaskState::Running { cancel_requested }) => {
            *slot = if *cancel_requested {
                None
            } else {
                Some(TaskState::Completed(result))
            };
        }
        _ => std::process::abort(),
    }
    result
}

/// Block on a single coroutine without spawn (alias surface for std.runtime).
///
/// # Safety
/// Same as [`ar_co_block_on_i64`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_block_on_i64(state: *mut u8) -> i64 {
    unsafe { ar_co_block_on_i64(state) }
}

/// Release a handle. If a join owns the blob, request retirement when it finishes.
///
/// # Safety
/// Handle from spawn; not usable after this call. When racing with join, the
/// join must already have claimed the task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_cancel_i64(handle: i64) {
    // SAFETY: the caller relinquishes this live task handle.
    unsafe { cancel_on(&TASKS, handle) };
}

unsafe fn cancel_on(tasks: &TaskTable, handle: i64) {
    let Ok(index) = usize::try_from(handle) else {
        return;
    };
    let pending = {
        let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = guard.get_mut(index) else {
            return;
        };
        match slot.take() {
            Some(TaskState::Pending(state)) => Some(state),
            Some(TaskState::Running { .. }) => {
                *slot = Some(TaskState::Running {
                    cancel_requested: true,
                });
                None
            }
            Some(TaskState::Completed(_)) | None => None,
        }
    };
    if let Some(state) = pending {
        // SAFETY: removing Pending transfers sole ownership to this cancel;
        // there is no joining thread using that blob.
        unsafe { ar_co_free(state) };
    }
}

/// Path absolute check for SL_S / Minimal path helpers.
///
/// Uses the host [`std::path::Path::is_absolute`] semantics (Unix `/…`, Windows
/// drive/UNC). Empty and invalid UTF-8 are never absolute.
///
/// # Safety
/// `ptr`/`len` fat string from Arandu JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_is_absolute(ptr: *const u8, len: isize) -> isize {
    if len <= 0 || ptr.is_null() {
        return 0;
    }
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let Ok(text) = std::str::from_utf8(s) else {
        return 0;
    };
    isize::from(std::path::Path::new(text).is_absolute())
}

/// Path empty check.
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_is_empty(_ptr: *const u8, len: isize) -> isize {
    isize::from(len <= 0)
}

/// Fat `str` return for path hosts (matches LayoutEngine / Cranelift multi-value).
///
/// On System V x86_64, two pointer-width fields return in the same registers as
/// Cranelift multi-value `(ptr, len)`.
#[repr(C)]
pub struct ArFatStr {
    pub ptr: *mut u8,
    pub len: isize,
}

pub(crate) fn fat_str_from_string(s: String) -> ArFatStr {
    let len = s.len() as isize;
    // Process-lifetime leak (same policy as ToStr / string interp).
    let boxed = s.into_boxed_str();
    let ptr = Box::into_raw(boxed) as *mut u8;
    ArFatStr { ptr, len }
}

pub(crate) fn path_from_fat(ptr: *const u8, len: isize) -> Option<std::path::PathBuf> {
    if len < 0 || (len > 0 && ptr.is_null()) {
        return None;
    }
    if len == 0 {
        return Some(std::path::PathBuf::new());
    }
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let text = std::str::from_utf8(s).ok()?;
    Some(std::path::PathBuf::from(text))
}

/// Join two path segments (`std::path::Path::join`).
///
/// # Safety
/// Fat string ABI for both inputs; returns malloc-style owned buffer.
unsafe fn ar_path_join_impl(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    let a = path_from_fat(a_ptr, a_len).unwrap_or_default();
    let b = path_from_fat(b_ptr, b_len).unwrap_or_default();
    let joined = a.join(b);
    fat_str_from_string(joined.to_string_lossy().into_owned())
}

/// Join two paths into an exact-size buffer owned by the caller.
///
/// # Safety
/// Input pairs must satisfy the fat-string ABI and all out-pointers must be
/// writable. A successful non-empty buffer must be released with
/// `ar_vec_buf_free(ptr, capacity)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_join_owned(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
    out_cap: *mut usize,
) -> i8 {
    if out_buf.is_null() || out_len.is_null() || out_cap.is_null() {
        return 0;
    }
    unsafe {
        *out_buf = std::ptr::null_mut();
        *out_len = 0;
        *out_cap = 0;
    }
    let (Some(a), Some(b)) = (path_from_fat(a_ptr, a_len), path_from_fat(b_ptr, b_len)) else {
        return 0;
    };
    let joined = a.join(b);
    let Some(bytes) = joined.to_str().map(str::as_bytes) else {
        return 0;
    };
    if bytes.is_empty() {
        return 1;
    }
    let data = unsafe { crate::vec_runtime::ar_vec_malloc(bytes.len()) };
    if data.is_null() {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        *out_buf = data;
        *out_len = bytes.len();
        *out_cap = bytes.len();
    }
    1
}

/// File name component (`Path::file_name`); empty string when none.
///
/// # Safety
/// Fat string ABI.
unsafe fn ar_path_file_name_impl(ptr: *const u8, len: isize) -> ArFatStr {
    let Some(path) = path_from_fat(ptr, len) else {
        return fat_str_from_string(String::new());
    };
    match path.file_name() {
        Some(name) => fat_str_from_string(name.to_string_lossy().into_owned()),
        None => fat_str_from_string(String::new()),
    }
}

fn slice_from_fat(ptr: *const u8, len: isize) -> &'static [u8] {
    if len <= 0 || ptr.is_null() {
        return b"";
    }
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }
}

/// Concatenate two fat strings (malloc-style process-lifetime buffer).
///
/// # Safety
/// Fat string ABI for both inputs.
unsafe fn ar_str_concat_impl(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    let a = slice_from_fat(a_ptr, a_len);
    let b = slice_from_fat(b_ptr, b_len);
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    let len = out.len() as isize;
    let ptr = Box::into_raw(out.into_boxed_slice()) as *mut u8;
    ArFatStr { ptr, len }
}

/// Bytes after the last occurrence of `sep` (byte-wise). Empty sep → full `s`.
///
/// # Safety
/// Fat string ABI.
unsafe fn ar_str_split_last_impl(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    let s = slice_from_fat(s_ptr, s_len);
    let sep = slice_from_fat(sep_ptr, sep_len);
    if sep.is_empty() {
        return fat_str_from_string(String::from_utf8_lossy(s).into_owned());
    }
    if let Some(pos) = s.windows(sep.len()).rposition(|w| w == sep) {
        let after = &s[pos + sep.len()..];
        return fat_str_from_string(String::from_utf8_lossy(after).into_owned());
    }
    fat_str_from_string(String::from_utf8_lossy(s).into_owned())
}

// Cranelift represents `str` as two return registers. Windows x64's C ABI
// returns this 16-byte struct indirectly, whereas SysV returns it in RAX/RDX.
// Keep the exported JIT boundary on SysV there; native Rust callers use the
// platform ABI only through the private implementations above.
/// Join two valid fat-string paths.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[cfg(not(windows))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_join(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_path_join_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(windows)]
/// Join two valid fat-string paths using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_path_join(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_path_join_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(not(windows))]
/// Return the final component of a valid fat-string path.
///
/// # Safety
/// `ptr` and `len` must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_file_name(ptr: *const u8, len: isize) -> ArFatStr {
    unsafe { ar_path_file_name_impl(ptr, len) }
}

#[cfg(windows)]
/// Return the final path component using the System V ABI expected by JIT code.
///
/// # Safety
/// `ptr` and `len` must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_path_file_name(ptr: *const u8, len: isize) -> ArFatStr {
    unsafe { ar_path_file_name_impl(ptr, len) }
}

#[cfg(not(windows))]
/// Concatenate two valid fat strings.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_concat(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_str_concat_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(windows)]
/// Concatenate two fat strings using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_str_concat(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_str_concat_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(not(windows))]
/// Return the suffix after the final occurrence of `sep`.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_split_last(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    unsafe { ar_str_split_last_impl(s_ptr, s_len, sep_ptr, sep_len) }
}

#[cfg(windows)]
/// Split a fat string using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_str_split_last(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    unsafe { ar_str_split_last_impl(s_ptr, s_len, sep_ptr, sep_len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll_runtime::{ar_co_make_ready_i64, ar_co_pending_once_i64};

    #[test]
    fn spawn_join_ready() {
        unsafe {
            let s = ar_co_make_ready_i64(42);
            let h = ar_rt_spawn_i64(s);
            assert_eq!(ar_rt_join_i64(h), 42);
            ar_rt_cancel_i64(h);
        }
    }

    #[test]
    fn completed_join_is_cached_until_cancel_releases_the_slot() {
        let tasks = TaskTable::new(Vec::new());
        // SAFETY: every blob is transferred to its own live task and freed by
        // join/cancel; the isolated table prevents unrelated tests reusing slots.
        unsafe {
            for value in 0..64 {
                let handle = spawn_on(&tasks, ar_co_make_ready_i64(value));
                assert_eq!(handle, 0, "cancel must make the slot reusable");
                assert_eq!(join_on(&tasks, handle, |s| ar_co_block_on_i64(s)), value);
                assert_eq!(
                    join_on(&tasks, handle, |_| unreachable!(
                        "cached join must not poll"
                    )),
                    value
                );
                cancel_on(&tasks, handle);
                assert!(tasks.lock().unwrap()[0].is_none());
            }
        }
    }

    #[test]
    fn cancel_before_join_releases_pending_state_and_slot() {
        let tasks = TaskTable::new(Vec::new());
        // SAFETY: no join exists and cancellation receives exclusive ownership.
        unsafe {
            for value in 0..64 {
                let handle = spawn_on(&tasks, ar_co_pending_once_i64(value));
                assert_eq!(handle, 0);
                cancel_on(&tasks, handle);
                assert!(tasks.lock().unwrap()[0].is_none());
            }
        }
    }

    #[test]
    fn concurrent_join_then_cancel_frees_once() {
        use std::sync::{Arc, mpsc};
        let tasks = Arc::new(TaskTable::new(Vec::new()));
        for value in 0..64 {
            // SAFETY: the channels establish that join owns the blob before
            // cancellation. Neither thread accesses the blob after join frees it.
            unsafe {
                let handle = spawn_on(&tasks, ar_co_pending_once_i64(value));
                assert_eq!(handle, 0);
                let (claimed_tx, claimed_rx) = mpsc::channel();
                let (resume_tx, resume_rx) = mpsc::channel();
                let joining_tasks = Arc::clone(&tasks);
                let joiner = std::thread::spawn(move || {
                    join_on(&joining_tasks, handle, |state| {
                        claimed_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        ar_co_block_on_i64(state)
                    })
                });
                claimed_rx.recv().unwrap();
                cancel_on(&tasks, handle);
                assert!(matches!(
                    tasks.lock().unwrap()[0],
                    Some(TaskState::Running {
                        cancel_requested: true
                    })
                ));
                resume_tx.send(()).unwrap();
                assert_eq!(joiner.join().unwrap(), value);
                assert!(tasks.lock().unwrap()[0].is_none());
            }
        }
    }

    #[test]
    fn path_absolute() {
        unsafe {
            let current_dir = std::env::current_dir().unwrap();
            let current_dir = current_dir.to_string_lossy();
            assert_eq!(
                ar_path_is_absolute(current_dir.as_ptr(), current_dir.len() as isize),
                1
            );
            assert_eq!(ar_path_is_absolute(b"rel".as_ptr(), 3), 0);
            assert_eq!(ar_path_is_absolute(b"".as_ptr(), 0), 0);
            assert_eq!(ar_path_is_absolute(b"./x".as_ptr(), 3), 0);
            assert_eq!(ar_path_is_empty(b"".as_ptr(), 0), 1);
        }
    }

    #[test]
    fn path_join_and_file_name() {
        unsafe {
            let j = ar_path_join(b"/tmp".as_ptr(), 4, b"x".as_ptr(), 1);
            let s = std::slice::from_raw_parts(j.ptr, j.len as usize);
            assert_eq!(
                s,
                std::path::Path::new("/tmp")
                    .join("x")
                    .to_string_lossy()
                    .as_bytes()
            );

            let j2 = ar_path_join(b"a".as_ptr(), 1, b"b".as_ptr(), 1);
            let s2 = std::slice::from_raw_parts(j2.ptr, j2.len as usize);
            assert_eq!(
                s2,
                std::path::Path::new("a")
                    .join("b")
                    .to_string_lossy()
                    .as_bytes()
            );

            let fnm = ar_path_file_name(b"/tmp/leaf".as_ptr(), 9);
            let sn = std::slice::from_raw_parts(fnm.ptr, fnm.len as usize);
            assert_eq!(sn, b"leaf");

            let leaf = ar_path_file_name(b"leaf".as_ptr(), 4);
            let sl = std::slice::from_raw_parts(leaf.ptr, leaf.len as usize);
            assert_eq!(sl, b"leaf");
        }
    }

    #[test]
    fn owned_path_join_returns_an_exact_caller_owned_buffer() {
        unsafe {
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            let mut capacity = 0;
            assert_eq!(
                ar_path_join_owned(
                    b"/tmp".as_ptr(),
                    4,
                    b"owned".as_ptr(),
                    5,
                    &mut data,
                    &mut len,
                    &mut capacity,
                ),
                1
            );
            let expected = std::path::Path::new("/tmp")
                .join("owned")
                .to_string_lossy()
                .into_owned();
            assert_eq!(std::slice::from_raw_parts(data, len), expected.as_bytes());
            assert_eq!(capacity, len);
            crate::vec_runtime::ar_vec_buf_free(data, capacity);
        }
    }

    #[test]
    fn str_concat_prefix_suffix_split() {
        unsafe {
            let c = ar_str_concat(b"ab".as_ptr(), 2, b"cd".as_ptr(), 2);
            let cs = std::slice::from_raw_parts(c.ptr, c.len as usize);
            assert_eq!(cs, b"abcd");
            let tail = ar_str_split_last(b"a/b/c".as_ptr(), 5, b"/".as_ptr(), 1);
            let ts = std::slice::from_raw_parts(tail.ptr, tail.len as usize);
            assert_eq!(ts, b"c");
        }
    }
}
