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
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};

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
    // Boxed and never freed *here*: the pointer crosses a host boundary with
    // no language-visible owner at this frame, so its lifetime must be
    // analyzed on its own. Different policy from ToStr / interpolation
    // buffers, which the caller owns and drop elaboration pairs with
    // `Free(str)`.
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

/// Pauses the current OS thread for at least `ms` milliseconds.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_thread_sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Cooperatively gives up the current thread's time slice.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_thread_yield() {
    std::thread::yield_now();
}

struct ThreadRecord {
    id: usize,
    thread: std::thread::Thread,
}

struct ThreadRegistration(Arc<ThreadRecord>);

impl Drop for ThreadRegistration {
    fn drop(&mut self) {
        let mut registry = THREAD_REGISTRY.write().unwrap_or_else(|e| e.into_inner());
        if let Some(registry) = registry.as_mut() {
            registry.remove(&self.0.id);
        }
    }
}

static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(1);
static THREAD_REGISTRY: RwLock<Option<HashMap<usize, Arc<ThreadRecord>>>> = RwLock::new(None);

thread_local! {
    static CURRENT_THREAD_RECORD: RefCell<Option<ThreadRegistration>> = const { RefCell::new(None) };
}

fn get_or_register_current_thread() -> Arc<ThreadRecord> {
    CURRENT_THREAD_RECORD.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if let Some(registration) = borrow.as_ref() {
            return Arc::clone(&registration.0);
        }
        let Ok(id) = NEXT_THREAD_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        else {
            // Identities must never wrap or be reused while stale IDs may exist.
            std::process::abort();
        };
        let record = Arc::new(ThreadRecord {
            id,
            thread: std::thread::current(),
        });
        *borrow = Some(ThreadRegistration(Arc::clone(&record)));
        let mut reg = THREAD_REGISTRY.write().unwrap_or_else(|e| e.into_inner());
        reg.get_or_insert_with(HashMap::new)
            .insert(id, Arc::clone(&record));
        record
    })
}

const PARKING_LOT_BUCKETS: usize = 64;

struct MonitorWaiter {
    notified: Mutex<bool>,
    ready: Condvar,
}

struct BucketState {
    monitors: HashMap<usize, VecDeque<Arc<MonitorWaiter>>>,
}

fn get_parking_lot() -> &'static [Mutex<BucketState>] {
    static LOT: OnceLock<Box<[Mutex<BucketState>]>> = OnceLock::new();
    LOT.get_or_init(|| {
        (0..PARKING_LOT_BUCKETS)
            .map(|_| {
                Mutex::new(BucketState {
                    monitors: HashMap::new(),
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

fn get_bucket(addr: usize) -> &'static Mutex<BucketState> {
    // Widen before mixing: a 58-bit shift on usize is invalid on 32-bit hosts.
    let mixed = (addr as u64).wrapping_mul(0x517cc1b727220a95);
    let idx = usize::try_from(mixed >> 58).unwrap_or(0);
    &get_parking_lot()[idx]
}

/// Returns a process-unique, non-recycled identity for the calling OS thread.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_thread_current_id() -> usize {
    get_or_register_current_thread().id
}

/// Consumes one unpark token, blocking if none is available. May wake spuriously.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_thread_park() {
    // Rust's park already owns the single token. A second atomic token would
    // leave an extra wake pending after unpark-before-park.
    std::thread::park();
}

/// Consumes one unpark token or blocks until unparked or `timeout_ms` expires.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_thread_park_timeout(timeout_ms: u64) {
    std::thread::park_timeout(std::time::Duration::from_millis(timeout_ms));
}

/// Supplies one coalescing unpark token to a live registered thread.
///
/// # Safety
/// Always safe to call; exited and unknown identities are ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_thread_unpark(thread_id: usize) {
    let record = {
        let reg = THREAD_REGISTRY.read().unwrap_or_else(|e| e.into_inner());
        reg.as_ref().and_then(|map| map.get(&thread_id).cloned())
    };
    if let Some(record) = record {
        record.thread.unpark();
    }
}

/// Waits for a notification associated with this sequence word's exact address.
/// This is a sequence event, not an atomic mutex-unlock/condition-wait operation.
///
/// # Safety
/// `key` must point to a live, aligned AtomicUsize with no non-atomic accesses.
/// Its address/allocation must remain stable until every waiter has returned.
/// Notify must increment this same word before calling the notify helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_condvar_wait(key: *const u8, seq: usize) {
    // SAFETY: the caller supplies the sequence allocation for the entire wait.
    let _ = unsafe { wait_on_monitor(key, seq, None) };
}

/// Waits without polling for notification or a monotonic timeout in milliseconds.
/// Returns 1 for notification/changed sequence, 0 for timeout, -1 for bad alignment.
///
/// # Safety
/// Same sequence allocation and notification contract as `ar_rt_condvar_wait`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_condvar_wait_timeout(
    key: *const u8,
    seq: usize,
    timeout_ms: u64,
) -> i32 {
    // SAFETY: the public caller supplies a stable, atomically accessed sequence.
    unsafe { wait_on_monitor(key, seq, Some(std::time::Duration::from_millis(timeout_ms))) }
}

unsafe fn wait_on_monitor(key: *const u8, seq: usize, timeout: Option<std::time::Duration>) -> i32 {
    let addr = key as usize;
    if key.is_null() || !addr.is_multiple_of(std::mem::align_of::<AtomicUsize>()) {
        return -1;
    }
    let waiter = Arc::new(MonitorWaiter {
        notified: Mutex::new(false),
        ready: Condvar::new(),
    });
    let bucket = get_bucket(addr);
    {
        let mut guard = bucket.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the caller guarantees this live, aligned atomic allocation.
        let observed = unsafe { (&*key.cast::<AtomicUsize>()).load(Ordering::Acquire) };
        if observed != seq {
            return 1;
        }
        guard
            .monitors
            .entry(addr)
            .or_default()
            .push_back(Arc::clone(&waiter));
    }
    // This per-wait event deliberately does not consume std::thread::park's
    // token, so notifications and explicit thread unparks cannot steal wakes.
    let mut notified = waiter.notified.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(timeout) = timeout {
        let (guard, _) = waiter
            .ready
            .wait_timeout_while(notified, timeout, |ready| !*ready)
            .unwrap_or_else(|e| e.into_inner());
        notified = guard;
    } else {
        notified = waiter
            .ready
            .wait_while(notified, |ready| !*ready)
            .unwrap_or_else(|e| e.into_inner());
    }
    drop(notified);
    // Bucket -> waiter is the only nested locking order. The timeout removes
    // its registration under the same lock as notify, settling their race.
    let mut guard = bucket.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(waiters) = guard.monitors.get_mut(&addr) {
        waiters.retain(|entry| !Arc::ptr_eq(entry, &waiter));
        if waiters.is_empty() {
            guard.monitors.remove(&addr);
        }
    }
    i32::from(*waiter.notified.lock().unwrap_or_else(|e| e.into_inner()))
}

fn notify_monitor(key: *const u8, all: bool) {
    if key.is_null() {
        return;
    }
    let addr = key as usize;
    let mut guard = get_bucket(addr).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(waiters) = guard.monitors.get_mut(&addr) {
        while let Some(waiter) = waiters.pop_front() {
            *waiter.notified.lock().unwrap_or_else(|e| e.into_inner()) = true;
            waiter.ready.notify_one();
            if !all {
                break;
            }
        }
        if waiters.is_empty() {
            guard.monitors.remove(&addr);
        }
    }
}

/// Wakes the oldest registered waiter for exactly `key`.
///
/// # Safety
/// Notify must follow an atomic increment of the live sequence word. The
/// allocation must remain stable until all concurrent wait/notify calls return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_condvar_notify_one(key: *const u8) {
    notify_monitor(key, false);
}

/// Wakes all registered waiters for exactly `key`.
///
/// # Safety
/// Same allocation and sequence-increment contract as notify_one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_condvar_notify_all(key: *const u8) {
    notify_monitor(key, true);
}

/// Fills a buffer exclusively from the operating system entropy source.
/// Returns -1 and clears the buffer if the source fails; never substitutes PRNGs.
///
/// # Safety
/// For nonzero `len`, `buf` must reference `len` writable, exclusively borrowed
/// bytes in a single live allocation. The length must fit `isize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_get_entropy(buf: *mut u8, len: usize) -> i32 {
    if len == 0 {
        return 0;
    }
    if buf.is_null() || len > isize::MAX as usize {
        return -1;
    }
    // SAFETY: the caller supplies a writable live allocation; null and slice
    // length limits are checked above. No host reference survives the call.
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf, len) };
    fill_entropy_with(bytes, os_entropy)
}

fn fill_entropy_with(
    bytes: &mut [u8],
    source: impl FnOnce(&mut [u8]) -> std::io::Result<()>,
) -> i32 {
    if source(bytes).is_ok() {
        0
    } else {
        bytes.fill(0);
        -1
    }
}

#[cfg(unix)]
fn os_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(bytes)
}

#[cfg(windows)]
fn os_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    // BCrypt accepts a DWORD length. Chunking preserves the whole request.
    for chunk in bytes.chunks_mut(u32::MAX as usize) {
        let length = u32::try_from(chunk.len()).map_err(std::io::Error::other)?;
        // SAFETY: chunk is exclusively writable for length bytes. A null
        // algorithm handle is required with SYSTEM_PREFERRED_RNG.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                length,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(std::io::Error::other("BCryptGenRandom failed"));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn os_entropy(_bytes: &mut [u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no operating system entropy backend",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll_runtime::{ar_co_make_ready_i64, ar_co_pending_once_i64};

    #[test]
    fn entropy_failure_clears_partial_output_without_fallback() {
        let mut bytes = [0x55; 32];
        let status = fill_entropy_with(&mut bytes, |dest| {
            dest[..3].copy_from_slice(&[1, 2, 3]);
            Err(std::io::Error::other("injected entropy failure"))
        });
        assert_eq!(status, -1);
        assert_eq!(bytes, [0; 32]);
        assert_eq!(
            fill_entropy_with(&mut bytes, |dest| {
                dest.fill(0x42);
                Ok(())
            }),
            0
        );
        assert_eq!(bytes, [0x42; 32]);
    }

    #[test]
    fn entropy_ffi_rejects_invalid_buffers_and_accepts_empty_requests() {
        // SAFETY: invalid pointers/lengths are rejected before constructing a
        // slice; the empty request accesses no storage.
        unsafe {
            assert_eq!(ar_rt_get_entropy(std::ptr::null_mut(), 0), 0);
            assert_eq!(ar_rt_get_entropy(std::ptr::null_mut(), 1), -1);
            assert_eq!(ar_rt_get_entropy(std::ptr::dangling_mut(), usize::MAX), -1);
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn entropy_os_source_fills_a_live_buffer() {
        let mut bytes = [0; 32];
        // SAFETY: the local array is live and exclusively borrowed for the call.
        assert_eq!(
            unsafe { ar_rt_get_entropy(bytes.as_mut_ptr(), bytes.len()) },
            0
        );
    }

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

    fn wait_for_monitor(key: usize, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let registered = get_bucket(key)
                .lock()
                .unwrap()
                .monitors
                .get(&key)
                .map_or(0, VecDeque::len);
            if registered == count {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "waiter registration timed out"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn thread_registry_retires_exited_threads_without_reusing_ids() {
        let mut last = 0;
        for _ in 0..32 {
            let id = std::thread::spawn(|| unsafe { ar_rt_thread_current_id() })
                .join()
                .unwrap();
            assert!(id > last);
            last = id;
            assert!(
                !THREAD_REGISTRY
                    .read()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .contains_key(&id)
            );
            // A stale unpark cannot affect another thread or recreate its record.
            unsafe {
                ar_rt_thread_unpark(id);
            }
        }
    }

    #[test]
    fn condvar_preserves_explicit_thread_unpark_token() {
        let seq = Arc::new(AtomicUsize::new(0));
        let address = Arc::as_ptr(&seq) as usize;
        let child_seq = Arc::clone(&seq);
        let (id_tx, id_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let child = std::thread::spawn(move || unsafe {
            let id = ar_rt_thread_current_id();
            ar_rt_thread_unpark(id);
            id_tx.send(id).unwrap();
            ar_rt_condvar_wait(Arc::as_ptr(&child_seq).cast(), 0);
            ar_rt_thread_park(); // must consume the preserved explicit token
            done_tx.send(()).unwrap();
        });
        id_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        wait_for_monitor(address, 1);
        seq.fetch_add(1, Ordering::Release);
        unsafe {
            ar_rt_condvar_notify_one(address as *const u8);
        }
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        child.join().unwrap();
        assert!(
            !get_bucket(address)
                .lock()
                .unwrap()
                .monitors
                .contains_key(&address)
        );
    }

    #[test]
    fn condvar_distinguishes_colliding_keys_and_notifies_fifo() {
        let words: Vec<_> = (0..=PARKING_LOT_BUCKETS)
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect();
        let mut collision = None;
        'outer: for (i, first) in words.iter().enumerate() {
            for second in &words[i + 1..] {
                if std::ptr::eq(
                    get_bucket(Arc::as_ptr(first) as usize),
                    get_bucket(Arc::as_ptr(second) as usize),
                ) {
                    collision = Some((Arc::clone(first), Arc::clone(second)));
                    break 'outer;
                }
            }
        }
        let (first, second) = collision.unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut children = Vec::new();
        // Register two waiters on the first key in a known order, one on the
        // colliding key. notify_one must wake only the oldest first-key waiter.
        for (index, word) in [Arc::clone(&first), Arc::clone(&second), Arc::clone(&first)]
            .into_iter()
            .enumerate()
        {
            let addr = Arc::as_ptr(&word) as usize;
            let tx = tx.clone();
            children.push(std::thread::spawn(move || {
                let result =
                    unsafe { ar_rt_condvar_wait_timeout(Arc::as_ptr(&word).cast(), 0, 5_000) };
                tx.send((index, result)).unwrap();
            }));
            wait_for_monitor(addr, if index == 2 { 2 } else { 1 });
        }
        first.fetch_add(1, Ordering::Release);
        unsafe {
            ar_rt_condvar_notify_one(Arc::as_ptr(&first).cast());
        }
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (0, 1)
        );
        wait_for_monitor(Arc::as_ptr(&first) as usize, 1);
        wait_for_monitor(Arc::as_ptr(&second) as usize, 1);
        unsafe {
            ar_rt_condvar_notify_all(Arc::as_ptr(&first).cast());
        }
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (2, 1)
        );
        second.fetch_add(1, Ordering::Release);
        unsafe {
            ar_rt_condvar_notify_all(Arc::as_ptr(&second).cast());
        }
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (1, 1)
        );
        for child in children {
            child.join().unwrap();
        }
    }

    #[test]
    fn condvar_timeout_and_changed_sequence_remove_registrations() {
        let seq = AtomicUsize::new(1);
        let key = (&seq as *const AtomicUsize).cast::<u8>();
        unsafe {
            assert_eq!(ar_rt_condvar_wait_timeout(key, 0, 5_000), 1);
            assert_eq!(ar_rt_condvar_wait_timeout(key, 1, 0), 0);
            assert_eq!(ar_rt_condvar_wait_timeout(key, 1, 1), 0);
            assert_eq!(ar_rt_condvar_wait_timeout(std::ptr::null(), 0, 0), -1);
            assert_eq!(ar_rt_condvar_wait_timeout(key.wrapping_add(1), 0, 0), -1);
        }
        assert!(
            !get_bucket(key as usize)
                .lock()
                .unwrap()
                .monitors
                .contains_key(&(key as usize))
        );
    }

    #[test]
    fn park_timeout_wakes_early_on_unpark_and_consumes_preexisting_token() {
        // 1. Pre-existing token consumed immediately without waiting for timeout:
        unsafe {
            let id = ar_rt_thread_current_id();
            ar_rt_thread_unpark(id);
            let start = std::time::Instant::now();
            ar_rt_thread_park_timeout(10_000); // 10 seconds timeout
            assert!(
                start.elapsed() < std::time::Duration::from_millis(500),
                "pre-existing unpark token must wake park_timeout immediately"
            );
        }

        // 2. Unpark during wait interrupts park_timeout immediately:
        let (id_tx, id_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let child = std::thread::spawn(move || unsafe {
            let id = ar_rt_thread_current_id();
            id_tx.send(id).unwrap();
            let start = std::time::Instant::now();
            ar_rt_thread_park_timeout(10_000); // 10 seconds timeout
            let elapsed = start.elapsed();
            done_tx.send(elapsed).unwrap();
        });

        let child_id = id_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Give the child thread time to enter park_timeout
        std::thread::sleep(std::time::Duration::from_millis(30));
        unsafe {
            ar_rt_thread_unpark(child_id);
        }

        let elapsed = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "unpark must interrupt park_timeout well before 10s timeout, took: {elapsed:?}"
        );
        child.join().unwrap();

        // 3. Timeout without token expires after requested duration:
        unsafe {
            let start = std::time::Instant::now();
            ar_rt_thread_park_timeout(20); // 20 ms
            let elapsed = start.elapsed();
            assert!(
                elapsed >= std::time::Duration::from_millis(15),
                "park_timeout without token must wait for requested timeout, took: {elapsed:?}"
            );
        }

        // 4. Race timeout vs unpark: completes deterministically without hanging
        let (race_id_tx, race_id_rx) = std::sync::mpsc::channel();
        let race_child = std::thread::spawn(move || unsafe {
            let id = ar_rt_thread_current_id();
            race_id_tx.send(id).unwrap();
            ar_rt_thread_park_timeout(10); // 10 ms
        });
        let race_id = race_id_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        unsafe {
            ar_rt_thread_unpark(race_id);
        }
        race_child.join().unwrap();
    }
}

mod phase_lock;
pub use phase_lock::{
    ar_rt_rwlock_read, ar_rt_rwlock_read_unlock, ar_rt_rwlock_try_read, ar_rt_rwlock_try_write,
    ar_rt_rwlock_write, ar_rt_rwlock_write_unlock,
};
