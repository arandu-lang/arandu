//! Owned native-thread transport for compiler-generated `std.thread` thunks.
//!
//! Dropping a handle joins it. This intentionally keeps every callback and
//! payload alive until execution and destruction finish; it is not detachment.
//! Language traps/abort are process failures, not recoverable child panics.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

type Entry = unsafe extern "C-unwind" fn(*mut u8, *mut u8) -> i32;
type DropValue = unsafe extern "C" fn(*mut u8);

struct Buffer {
    pointer: *mut u8,
    size: usize,
    align: usize,
    drop_value: DropValue,
    initialized: bool,
}

// SAFETY: the raw ABI requires Send payloads, unique ownership of their
// allocations, and Send-safe drop glue. No shared access is exposed.
unsafe impl Send for Buffer {}

impl Drop for Buffer {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: this buffer uniquely owns one initialized payload.
            unsafe { (self.drop_value)(self.pointer) };
        }
        // SAFETY: the ABI transfers an ar_rt_alloc_aligned allocation with
        // its original, validated, nonzero size and alignment.
        unsafe { crate::vec_runtime::ar_rt_free_aligned(self.pointer, self.size, self.align) };
    }
}

struct Task {
    context: Buffer,
    result: Buffer,
    entry: Entry,
}

/// Sentinel pointer returned by `ar_rt_native_thread_join` when the child thread panicked or failed.
pub const AR_THREAD_PANICKED: *mut u8 = std::ptr::without_provenance_mut(1);

impl Task {
    fn execute(mut self) -> Option<Buffer> {
        // SAFETY: the compiler pairs entry with these exact context/result
        // types. It consumes context on every return and initializes result
        // exactly when returning zero. If entry unwinds or fails, Task::drop
        // ensures context is destroyed and both buffers are freed safely.
        let status = unsafe { (self.entry)(self.context.pointer, self.result.pointer) };
        self.context.initialized = false;
        if status != 0 {
            return None;
        }
        self.result.initialized = true;
        Some(self.result)
    }
}

struct Running {
    callbacks: [usize; 3],
    // None means a join/drop claimed this handle. Keep the row visible until
    // the claimant finishes joining and any result destructor has returned.
    thread: Option<std::thread::JoinHandle<Option<Buffer>>>,
    finished: Arc<AtomicBool>,
}

static THREADS: OnceLock<Mutex<HashMap<usize, Running>>> = OnceLock::new();
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static COMPLETED: Condvar = Condvar::new();

fn threads() -> &'static Mutex<HashMap<usize, Running>> {
    THREADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_threads() -> std::sync::MutexGuard<'static, HashMap<usize, Running>> {
    threads().lock().unwrap_or_else(|error| error.into_inner())
}

fn valid_buffer(pointer: *mut u8, size: usize, align: usize) -> bool {
    !pointer.is_null()
        && size != 0
        && std::alloc::Layout::from_size_align(size, align).is_ok()
        && pointer.addr().is_multiple_of(align)
}

/// Starts one native thread, transferring both allocations even if OS spawn
/// fails. Returns a process-unique nonzero handle, or zero on failure.
///
/// # Safety
/// `context` contains a uniquely owned Send value; `result` is uninitialized
/// storage for a Send value. Both must come from `ar_rt_alloc_aligned` using
/// their exact nonzero size/alignment (use size 1 for ZSTs). Callbacks must be
/// valid, non-unwinding and remain alive until join/drop. Entry consumes the
/// context on every return and initializes result iff its status is zero.
/// Drop callbacks destroy values in place without freeing the allocation.
/// Invalid pointers/layouts/callbacks are rejected before ownership transfer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_native_thread_spawn(
    context: *mut u8,
    context_size: usize,
    context_align: usize,
    entry: *const u8,
    drop_context: *const u8,
    result: *mut u8,
    result_size: usize,
    result_align: usize,
    drop_result: *const u8,
) -> usize {
    if !valid_buffer(context, context_size, context_align)
        || !valid_buffer(result, result_size, result_align)
        || entry.is_null()
        || drop_context.is_null()
        || drop_result.is_null()
    {
        return 0;
    }
    // SAFETY: the compiler-generated raw ABI supplies the documented function
    // pointer types; all pointers have been checked for null above.
    let (entry_fn, context_drop_fn, result_drop_fn) = unsafe {
        (
            std::mem::transmute::<*const u8, Entry>(entry),
            std::mem::transmute::<*const u8, DropValue>(drop_context),
            std::mem::transmute::<*const u8, DropValue>(drop_result),
        )
    };
    let task = Task {
        context: Buffer {
            pointer: context,
            size: context_size,
            align: context_align,
            drop_value: context_drop_fn,
            initialized: true,
        },
        result: Buffer {
            pointer: result,
            size: result_size,
            align: result_align,
            drop_value: result_drop_fn,
            initialized: false,
        },
        entry: entry_fn,
    };
    let Ok(id) = NEXT_ID.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
    else {
        return 0;
    };
    let finished = Arc::new(AtomicBool::new(false));
    let child_finished = Arc::clone(&finished);
    let Ok(thread) = std::thread::Builder::new().spawn(move || {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || task.execute()))
                .unwrap_or(None);
        // Pair completion publication with the registry mutex so the module
        // barrier can wait without polling or losing a wakeup.
        let _registry = lock_threads();
        child_finished.store(true, Ordering::Release);
        COMPLETED.notify_all();
        outcome
    }) else {
        // Builder drops its closure on failure, destroying context exactly
        // once and freeing result without touching uninitialized bytes.
        return 0;
    };
    lock_threads().insert(
        id,
        Running {
            callbacks: [entry.addr(), drop_context.addr(), drop_result.addr()],
            thread: Some(thread),
            finished,
        },
    );
    id
}

/// Consumes a handle and joins it, returning the initialized result allocation,
/// `AR_THREAD_PANICKED` if the child panicked or failed, or null on invalid/consumed
/// handle. The caller owns the returned value and must free its allocation using the
/// original layout after moving it out.
#[unsafe(no_mangle)]
pub extern "C" fn ar_rt_native_thread_join(handle: usize) -> *mut u8 {
    let Some(thread) = claim_thread(handle) else {
        return std::ptr::null_mut();
    };
    let result = match thread.join() {
        Ok(Some(result)) => std::mem::ManuallyDrop::new(result).pointer,
        Ok(None) | Err(_) => AR_THREAD_PANICKED,
    };
    finish_claim(handle);
    result
}

/// Returns the sentinel pointer returned by `ar_rt_native_thread_join` when the child thread panicked.
#[unsafe(no_mangle)]
pub extern "C" fn ar_rt_native_thread_panicked_sentinel() -> *mut u8 {
    AR_THREAD_PANICKED
}

/// Whether a live handle has completed. Unknown/consumed handles return false.
#[unsafe(no_mangle)]
pub extern "C" fn ar_rt_native_thread_is_finished(handle: usize) -> bool {
    lock_threads()
        .get(&handle)
        .and_then(|running| running.thread.as_ref())
        .is_some_and(|thread| thread.is_finished())
}

/// Consumes a handle, waits for completion and destroys any unclaimed result.
#[unsafe(no_mangle)]
pub extern "C" fn ar_rt_native_thread_drop(handle: usize) {
    if let Some(thread) = claim_thread(handle) {
        drop(thread.join());
        finish_claim(handle);
    }
}

fn claim_thread(handle: usize) -> Option<std::thread::JoinHandle<Option<Buffer>>> {
    let thread = lock_threads().get_mut(&handle)?.thread.take();
    COMPLETED.notify_all();
    thread
}

fn finish_claim(handle: usize) {
    lock_threads().remove(&handle);
    COMPLETED.notify_all();
}

/// Completes all threads referencing a module before its callbacks disappear.
/// Repeats after joining: a child may start descendants while it is running.
/// The caller must already have stopped external calls into the module.
pub fn join_module_threads(contains_callback: impl Fn(usize) -> bool) {
    loop {
        let batch: Vec<_> = {
            let mut registry = lock_threads();
            // Do not steal a completed child handle while a parent callback
            // could still join it. First let every matching callback return.
            while registry.values().any(|running| {
                running.callbacks.iter().any(|&f| contains_callback(f))
                    && (running.thread.is_none() || !running.finished.load(Ordering::Acquire))
            }) {
                registry = COMPLETED
                    .wait(registry)
                    .unwrap_or_else(|error| error.into_inner());
            }
            let ids: Vec<_> = registry
                .iter()
                .filter(|(_, running)| running.callbacks.iter().any(|&f| contains_callback(f)))
                .map(|(&id, _)| id)
                .collect();
            ids.into_iter()
                .filter_map(|id| {
                    registry
                        .get_mut(&id)?
                        .thread
                        .take()
                        .map(|thread| (id, thread))
                })
                .collect()
        };
        if batch.is_empty() {
            break;
        }
        for (id, thread) in batch {
            drop(thread.join());
            finish_claim(id);
        }
    }
}

#[cfg(test)]
mod tests;
