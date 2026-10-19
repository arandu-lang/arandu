//! Address-keyed phase-fair reader/writer admission. Storage stays stationary
//! while borrowed. Writers are FIFO; a writer release admits every already
//! queued reader before the next writer. No time bound is implied by fairness.

use super::MonitorWaiter;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default)]
struct PhaseState {
    readers: usize,
    writer: bool,
    waiting_readers: VecDeque<Arc<MonitorWaiter>>,
    waiting_writers: VecDeque<Arc<MonitorWaiter>>,
}

fn states(key: usize) -> &'static Mutex<HashMap<usize, PhaseState>> {
    static STATES: OnceLock<Vec<Mutex<HashMap<usize, PhaseState>>>> = OnceLock::new();
    // Use the same address striping as the sequence-event ParkingLot, but a
    // separate state table: condition waits and lock admissions never nest.
    let bucket = usize::try_from((key as u64).wrapping_mul(0x517cc1b727220a95) >> 58).unwrap_or(0);
    &STATES.get_or_init(|| (0..64).map(|_| Mutex::new(HashMap::new())).collect())[bucket]
}

fn signal(waiter: Arc<MonitorWaiter>) {
    *waiter.notified.lock().unwrap_or_else(|e| e.into_inner()) = true;
    waiter.ready.notify_one();
}

fn admit_writer(state: &mut PhaseState) {
    if let Some(waiter) = state.waiting_writers.pop_front() {
        state.writer = true;
        signal(waiter);
    }
}

fn acquire(key: *mut u8, write: bool, blocking: bool) -> i32 {
    if key.is_null() {
        std::process::abort();
    }
    let address = key as usize;
    let waiter = {
        let mut entries = states(address).lock().unwrap_or_else(|e| e.into_inner());
        let state = entries.entry(address).or_default();
        let available =
            !state.writer && state.waiting_writers.is_empty() && (!write || state.readers == 0);
        if available {
            if write {
                state.writer = true;
            } else {
                state.readers = state
                    .readers
                    .checked_add(1)
                    .unwrap_or_else(|| std::process::abort());
            }
            return 1;
        }
        if !blocking {
            return 0;
        }
        let waiter = Arc::new(MonitorWaiter {
            notified: Mutex::new(false),
            ready: std::sync::Condvar::new(),
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

fn release(key: *mut u8, write: bool) {
    if key.is_null() {
        std::process::abort();
    }
    let address = key as usize;
    let mut entries = states(address).lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = entries.get_mut(&address) else {
        std::process::abort();
    };
    if write {
        if !state.writer || state.readers != 0 {
            std::process::abort();
        }
        state.writer = false;
        // PF3: snapshot the complete waiting reader phase under the bucket
        // lock. Later readers cannot overtake any unsatisfied writer (PF4).
        state.readers = state.waiting_readers.len();
        for waiter in state.waiting_readers.drain(..) {
            signal(waiter);
        }
        if state.readers == 0 {
            admit_writer(state);
        }
    } else {
        if state.writer || state.readers == 0 {
            std::process::abort();
        }
        state.readers -= 1;
        if state.readers == 0 {
            admit_writer(state);
        }
    }
    if !state.writer && state.readers == 0 {
        entries.remove(&address);
    }
}

/// Acquires a phase-fair shared lock without polling.
/// # Safety
/// `key` identifies live stationary lock storage until all holders/waiters
/// finish. Every successful acquire must have exactly one matching release;
/// protected memory must only be accessed through the corresponding guard.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_read(key: *mut u8) {
    let _ = acquire(key, false, true);
}

/// Acquires the FIFO writer phase. Not recursive or cancellable.
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

/// Releases one acquired reader and wakes the next writer if it was last.
/// # Safety
/// Caller owns exactly one outstanding read acquisition at `key`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_read_unlock(key: *mut u8) {
    release(key, false);
}

/// Releases the writer, first admitting the queued reader phase, if any.
/// # Safety
/// Caller owns the unique outstanding writer acquisition at `key`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_rwlock_write_unlock(key: *mut u8) {
    release(key, true);
}

#[cfg(test)]
mod tests;
