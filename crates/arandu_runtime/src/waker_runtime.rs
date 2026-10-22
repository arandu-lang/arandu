//! SL_R — host Waker / Context for cooperative wake (no OS thread park yet).
//!
//! Wakers are explicit handles (like SyncExecutor). `wake` sets a flag;
//! `wait` blocks on a local Condvar until set or timeout.

use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::{Duration, Instant};

struct WakerSlot {
    state: Arc<(StdMutex<bool>, Condvar)>,
}

struct WakerEntry {
    /// Zero means the packed id equals the index, so the first occupant keeps
    /// the historical handle. Later generations never reuse that integer.
    generation: u32,
    slot: Option<WakerSlot>,
}

struct WakerTable {
    entries: Vec<WakerEntry>,
    free: Vec<u32>,
}

static WAKERS: StdMutex<WakerTable> = StdMutex::new(WakerTable {
    entries: Vec::new(),
    free: Vec::new(),
});

fn lock() -> std::sync::MutexGuard<'static, WakerTable> {
    WAKERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn pack_waker(index: u32, generation: u32) -> i64 {
    (((u64::from(generation) & 0x7FFF_FFFF) << 32) | u64::from(index)) as i64
}

fn unpack_waker(id: i64) -> Option<(usize, u32)> {
    if id < 0 {
        return None;
    }
    let raw = id as u64;
    Some(((raw & 0xFFFF_FFFF) as usize, (raw >> 32) as u32))
}

fn bump_waker_generation(generation: u32) -> u32 {
    let next = (generation + 1) & 0x7FFF_FFFF;
    if next == 0 { 1 } else { next }
}

fn fresh_slot() -> WakerSlot {
    WakerSlot {
        state: Arc::new((StdMutex::new(false), Condvar::new())),
    }
}

fn waker_state(id: i64) -> Option<Arc<(StdMutex<bool>, Condvar)>> {
    let (index, generation) = unpack_waker(id)?;
    let table = lock();
    let entry = table.entries.get(index)?;
    if entry.generation != generation {
        return None;
    }
    entry.slot.as_ref().map(|slot| Arc::clone(&slot.state))
}

/// Create a waker. Returns id >= 0.
///
/// # Safety
/// C ABI for JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_create() -> i64 {
    let mut table = lock();
    if let Some(index) = table.free.pop() {
        let entry = &mut table.entries[index as usize];
        let generation = entry.generation;
        entry.slot = Some(fresh_slot());
        return pack_waker(index, generation);
    }
    let Ok(index) = u32::try_from(table.entries.len()) else {
        return -1;
    };
    table.entries.push(WakerEntry {
        generation: 0,
        slot: Some(fresh_slot()),
    });
    pack_waker(index, 0)
}

/// Mark waker ready (idempotent).
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_wake(id: i64) {
    if id < 0 {
        return;
    }
    let Some(state) = waker_state(id) else {
        return;
    };
    {
        let (lock, cvar) = &*state;
        {
            let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
            *woken = true;
        }
        cvar.notify_one();
    }
}

/// Wait until woken or `timeout_ms` elapses (-1 = forever).
/// Returns 1 if woken, 0 on timeout, -1 on error.
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_wait(id: i64, timeout_ms: i64) -> i64 {
    if id < 0 {
        return -1;
    }
    let Some(state) = waker_state(id) else {
        return -1;
    };

    let (lock, cvar) = &*state;
    let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());

    if *woken {
        *woken = false;
        return 1;
    }

    if timeout_ms == 0 {
        return 0;
    }

    if timeout_ms < 0 {
        while !*woken {
            woken = cvar.wait(woken).unwrap_or_else(|e| e.into_inner());
        }
        *woken = false;
        return 1;
    }

    let timeout = Duration::from_millis(timeout_ms as u64);
    let start = Instant::now();
    while !*woken {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return 0;
        }
        let (new_woken, wait_result) = cvar
            .wait_timeout(woken, timeout - elapsed)
            .unwrap_or_else(|e| e.into_inner());
        woken = new_woken;
        if wait_result.timed_out() && !*woken {
            return 0;
        }
    }
    *woken = false;
    1
}

/// Destroy a waker handle.
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_destroy(id: i64) {
    let Some((index, generation)) = unpack_waker(id) else {
        return;
    };
    let mut table = lock();
    let Some(entry) = table.entries.get_mut(index) else {
        return;
    };
    if entry.generation != generation || entry.slot.is_none() {
        return;
    }
    entry.slot = None;
    entry.generation = bump_waker_generation(entry.generation);
    if let Ok(index) = u32::try_from(index) {
        table.free.push(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_wait() {
        unsafe {
            let w = ar_rt_waker_create();
            ar_rt_waker_wake(w);
            assert_eq!(ar_rt_waker_wait(w, 100), 1);
            ar_rt_waker_destroy(w);
        }
    }

    #[test]
    fn closed_waker_does_not_wake_the_reused_slot() {
        unsafe {
            let first = ar_rt_waker_create();
            ar_rt_waker_destroy(first);
            let second = ar_rt_waker_create();
            assert_ne!(first, second);
            ar_rt_waker_wake(first);
            assert_eq!(ar_rt_waker_wait(second, 0), 0);
            ar_rt_waker_wake(second);
            assert_eq!(ar_rt_waker_wait(second, 0), 1);
            ar_rt_waker_destroy(second);
        }
    }
}
