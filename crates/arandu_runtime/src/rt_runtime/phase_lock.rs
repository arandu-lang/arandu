//! Address-keyed phase-fair reader/writer admission. Storage stays stationary
//! while borrowed. Writers are FIFO; a writer release admits every already
//! queued reader before the next writer. Read reentry is tracked by thread ID
//! and allowed only without a queued writer. Blocking self-deadlocking
//! reentry aborts with a diagnostic. No time bound is implied by fairness.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

struct PhaseWaiter {
    owner_id: usize,
    notified: Mutex<bool>,
    ready: Condvar,
}

#[derive(Default)]
struct PhaseState {
    readers: usize,
    writer: bool,
    reader_owners: HashMap<usize, usize>,
    writer_owner: Option<usize>,
    waiting_readers: VecDeque<Arc<PhaseWaiter>>,
    waiting_writers: VecDeque<Arc<PhaseWaiter>>,
}

fn states(key: usize) -> &'static Mutex<HashMap<usize, PhaseState>> {
    static STATES: OnceLock<Vec<Mutex<HashMap<usize, PhaseState>>>> = OnceLock::new();
    // Use the same address striping as the sequence-event ParkingLot, but a
    // separate state table: condition waits and lock admissions never nest.
    let bucket = usize::try_from((key as u64).wrapping_mul(0x517cc1b727220a95) >> 58).unwrap_or(0);
    &STATES.get_or_init(|| (0..64).map(|_| Mutex::new(HashMap::new())).collect())[bucket]
}

fn current_owner_id() -> usize {
    // SAFETY: this runtime helper always returns a process-unique thread ID.
    unsafe { super::ar_rt_thread_current_id() }
}

fn add_reader(state: &mut PhaseState, owner_id: usize) {
    state.readers = state
        .readers
        .checked_add(1)
        .unwrap_or_else(|| std::process::abort());
    let count = state.reader_owners.entry(owner_id).or_default();
    *count = count
        .checked_add(1)
        .unwrap_or_else(|| std::process::abort());
}

fn remove_reader(state: &mut PhaseState, owner_id: usize) -> bool {
    let Some(count) = state.reader_owners.get_mut(&owner_id) else {
        return false;
    };
    if state.readers == 0 || *count == 0 {
        return false;
    }
    state.readers -= 1;
    if *count == 1 {
        state.reader_owners.remove(&owner_id);
    } else {
        *count -= 1;
    }
    true
}

fn would_self_deadlock(state: &PhaseState, owner_id: usize, write: bool) -> bool {
    state.writer_owner == Some(owner_id)
        || (write && state.reader_owners.contains_key(&owner_id))
        || (!write
            && state.reader_owners.contains_key(&owner_id)
            && !state.waiting_writers.is_empty())
}

fn abort_self_deadlock(key: usize, write: bool) -> ! {
    let operation = if write { "write" } else { "read" };
    eprintln!(
        "arandu runtime: reentrant {operation} acquisition on RwLock key 0x{key:x} would deadlock"
    );
    std::process::abort()
}

fn signal(waiter: Arc<PhaseWaiter>) {
    *waiter.notified.lock().unwrap_or_else(|e| e.into_inner()) = true;
    waiter.ready.notify_one();
}

fn admit_writer(state: &mut PhaseState) {
    if let Some(waiter) = state.waiting_writers.pop_front() {
        state.writer = true;
        state.writer_owner = Some(waiter.owner_id);
        signal(waiter);
    }
}

fn acquire(key: *mut u8, write: bool, blocking: bool) -> i32 {
    if key.is_null() {
        std::process::abort();
    }
    let address = key as usize;
    let owner_id = current_owner_id();
    let waiter = {
        let mut entries = states(address).lock().unwrap_or_else(|e| e.into_inner());
        let state = entries.entry(address).or_default();
        if blocking && would_self_deadlock(state, owner_id, write) {
            abort_self_deadlock(address, write);
        }
        let available =
            !state.writer && state.waiting_writers.is_empty() && (!write || state.readers == 0);
        if available {
            if write {
                state.writer = true;
                state.writer_owner = Some(owner_id);
            } else {
                add_reader(state, owner_id);
            }
            return 1;
        }
        if !blocking {
            return 0;
        }
        let waiter = Arc::new(PhaseWaiter {
            owner_id,
            notified: Mutex::new(false),
            ready: Condvar::new(),
        });
        if write {
            state.waiting_writers.push_back(Arc::clone(&waiter));
        } else {
            state.waiting_readers.push_back(Arc::clone(&waiter));
        }
        waiter
    };
    let ready = waiter.notified.lock().unwrap_or_else(|e| e.into_inner());
    drop(
        waiter
            .ready
            .wait_while(ready, |ready| !*ready)
            .unwrap_or_else(|e| e.into_inner()),
    );
    1
}

fn release(key: *mut u8, write: bool, owner_id: usize) -> bool {
    if key.is_null() {
        return false;
    }
    let address = key as usize;
    let mut entries = states(address).lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = entries.get_mut(&address) else {
        return false;
    };
    if write {
        if !state.writer || state.writer_owner != Some(owner_id) || state.readers != 0 {
            return false;
        }
        state.writer = false;
        state.writer_owner = None;
        // PF3: snapshot the complete waiting reader phase under the bucket
        // lock. Later readers cannot overtake any unsatisfied writer (PF4).
        while let Some(waiter) = state.waiting_readers.pop_front() {
            add_reader(state, waiter.owner_id);
            signal(waiter);
        }
        if state.readers == 0 {
            admit_writer(state);
        }
    } else {
        if state.writer || !remove_reader(state, owner_id) {
            return false;
        }
        if state.readers == 0 {
            admit_writer(state);
        }
    }
    if !state.writer && state.readers == 0 {
        if state.writer_owner.is_some() || !state.reader_owners.is_empty() {
            std::process::abort();
        }
        entries.remove(&address);
    }
    true
}

/// Acquires a phase-fair shared lock without polling.
/// # Safety
/// `key` identifies live stationary lock storage until all holders/waiters
/// finish. Every successful acquire must have exactly one matching release;
/// protected memory must only be accessed through the corresponding guard.
/// Read reacquisition by the same thread is allowed only while no writer is
/// queued. A blocking incompatible reacquisition aborts with a diagnostic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_read(key: *mut u8) {
    let _ = acquire(key, false, true);
}

/// Acquires the FIFO writer phase. Incompatible self-reentry aborts with a
/// diagnostic. This blocking operation is not cancellable.
/// # Safety
/// Same storage and guard contract as `ar_rt_rwlock_read`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_write(key: *mut u8) {
    let _ = acquire(key, true, true);
}

/// Attempts reader admission; returns one on acquisition and zero otherwise.
/// # Safety
/// Same storage and guard contract as `ar_rt_rwlock_read`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_try_read(key: *mut u8) -> i32 {
    acquire(key, false, false)
}

/// Attempts writer admission; returns one on acquisition and zero otherwise.
/// # Safety
/// Same storage and guard contract as `ar_rt_rwlock_read`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_try_write(key: *mut u8) -> i32 {
    acquire(key, true, false)
}

/// Releases one read acquisition owned by the current thread and wakes the
/// next writer if it was last.
/// # Safety
/// Caller owns exactly one outstanding read acquisition at `key` on this OS
/// thread. A mismatched owner or duplicate release aborts the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_read_unlock(key: *mut u8) {
    if !release(key, false, current_owner_id()) {
        std::process::abort();
    }
}

/// Releases the writer, first admitting the queued reader phase, if any.
/// # Safety
/// Caller owns the unique outstanding writer acquisition at `key` on this OS
/// thread. A mismatched owner or duplicate release aborts the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_write_unlock(key: *mut u8) {
    if !release(key, true, current_owner_id()) {
        std::process::abort();
    }
}

#[cfg(test)]
mod tests;
