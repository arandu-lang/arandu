use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};

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

pub(super) struct ThreadRecord {
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
pub(super) static THREAD_REGISTRY: RwLock<Option<HashMap<usize, Arc<ThreadRecord>>>> =
    RwLock::new(None);

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

pub(super) const PARKING_LOT_BUCKETS: usize = 64;

pub(super) struct MonitorWaiter {
    notified: Mutex<bool>,
    ready: Condvar,
}

pub(super) struct BucketState {
    pub(super) monitors: HashMap<usize, VecDeque<Arc<MonitorWaiter>>>,
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

pub(super) fn get_bucket(addr: usize) -> &'static Mutex<BucketState> {
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
