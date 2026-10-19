//! A3.6 — host coroutine poll / block_on for debug JIT.
//!
//! ## State blob layout (handle = `*mut u8`)
//!
//! ```text
//! +0: disc: u32
//!     0 = Ready  — payload valid at +8
//!     1 = PendingOnce — first poll returns Pending, then disc→0
//! +4: padding to 8
//! +8: payload (i64 for this MVP host; codegen may store other sizes)
//! ```
//!
//! Ready-only `CoroutineReady` always writes disc=0. `ar_co_pending_once_i64`
//! builds a one-shot Pending for tests. Full multi-resume disc values land with
//! generated poll functions later.

use crate::vec_runtime::{ar_rt_raw_free, ar_rt_raw_malloc};

const DISC_READY: u32 = 0;
const DISC_PENDING_ONCE: u32 = 1;
const HEADER: usize = 8;
const PAYLOAD_OFF: usize = 8;
const CO_MAGIC: u32 = 0x4152434f; // "ARCO"
/// Maximum number of short spin iterations before yielding thread in cooperative block_on.
const CO_SPIN_LIMIT: u32 = 32;

#[repr(C)]
struct CoHeader {
    disc: u32,
    magic: u32,
}

/// Allocate Ready state with i64 payload (tests / helpers).
///
/// # Safety
/// C ABI for Cranelift JIT symbol table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_co_make_ready_i64(payload: i64) -> *mut u8 {
    let p = unsafe { ar_rt_raw_malloc(HEADER + 8) };
    if p.is_null() {
        std::process::abort();
    }
    unsafe {
        (p as *mut CoHeader).write(CoHeader {
            disc: DISC_READY,
            magic: CO_MAGIC,
        });
        (p.add(PAYLOAD_OFF) as *mut i64).write(payload);
    }
    p
}

/// First `poll` → Pending; subsequent → Ready(payload).
///
/// # Safety
/// C ABI for Cranelift JIT symbol table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_co_pending_once_i64(payload: i64) -> *mut u8 {
    let p = unsafe { ar_rt_raw_malloc(HEADER + 8) };
    if p.is_null() {
        std::process::abort();
    }
    unsafe {
        (p as *mut CoHeader).write(CoHeader {
            disc: DISC_PENDING_ONCE,
            magic: CO_MAGIC,
        });
        (p.add(PAYLOAD_OFF) as *mut i64).write(payload);
    }
    p
}

/// Poll: returns `0` if Ready (`*out` = payload), `1` if Pending.
///
/// # Safety
/// `state` / `out` must be valid for the coroutine blob and write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_co_poll_i64(state: *mut u8, out: *mut i64) -> i32 {
    if state.is_null() || out.is_null() {
        std::process::abort();
    }
    let header = unsafe { &mut *(state as *mut CoHeader) };
    if header.magic != CO_MAGIC {
        std::process::abort();
    }
    match header.disc {
        DISC_READY => {
            let v = unsafe { (state.add(PAYLOAD_OFF) as *const i64).read() };
            unsafe {
                *out = v;
            }
            0
        }
        DISC_PENDING_ONCE => {
            // Consume the pending: next poll will be Ready.
            header.disc = DISC_READY;
            1
        }
        _ => {
            // Unknown disc — treat as Ready payload (forward-compat).
            let v = unsafe { (state.add(PAYLOAD_OFF) as *const i64).read() };
            unsafe {
                *out = v;
            }
            0
        }
    }
}

/// Drive until Ready; returns payload.
///
/// # Safety
/// `state` must be a valid coroutine blob.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_co_block_on_i64(state: *mut u8) -> i64 {
    let mut out: i64 = 0;
    let mut spins: u32 = 0;
    loop {
        let tag = unsafe { ar_co_poll_i64(state, &mut out) };
        if tag == 0 {
            return out;
        }
        // Cooperative backoff: short spin before yielding to prevent CPU core lockup.
        if spins < CO_SPIN_LIMIT {
            std::hint::spin_loop();
            spins += 1;
        } else {
            std::thread::yield_now();
            spins = 0;
        }
    }
}

/// Free a host-allocated coroutine blob (optional; process exit reclaims).
///
/// # Safety
/// `state` must have been allocated by this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_co_free(state: *mut u8) {
    if state.is_null() {
        return;
    }
    let header = unsafe { &*(state as *const CoHeader) };
    if header.magic != CO_MAGIC {
        std::process::abort();
    }
    unsafe {
        ar_rt_raw_free(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_polls_once() {
        unsafe {
            let s = ar_co_make_ready_i64(42);
            let mut out = 0i64;
            assert_eq!(ar_co_poll_i64(s, &mut out), 0);
            assert_eq!(out, 42);
            ar_co_free(s);
        }
    }

    #[test]
    fn pending_once_then_ready() {
        unsafe {
            let s = ar_co_pending_once_i64(7);
            let mut out = 0i64;
            assert_eq!(ar_co_poll_i64(s, &mut out), 1);
            assert_eq!(ar_co_poll_i64(s, &mut out), 0);
            assert_eq!(out, 7);
            assert_eq!(ar_co_block_on_i64(ar_co_pending_once_i64(9)), 9);
        }
    }
}
