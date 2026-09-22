//! Arandu host runtime (SL_R).
//!
//! C-ABI routines the JIT'd program calls back into: process supervisor
//! ([`supervisor_runtime`]), cooperative reactor with epoll/timerfd and an
//! io_uring fast path on Linux ([`reactor_runtime`]), TCP sockets
//! ([`socket_runtime`]), wakers, coroutine poll/block-on state, SyncExecutor
//! spawn/join plus path/string hosts ([`rt_runtime`]), dynamic vectors
//! ([`vec_runtime`]), generational arenas ([`gen_runtime`]), ownership-aware
//! worker transport ([`worker_runtime`]) and the bounded reusable worker pool
//! ([`worker_scheduler`]), OS essentials, ToStr/IO hosts and fat-string
//! helpers.
//!
//! These functions are plain `extern "C"` host symbols: they hold no dependency
//! on Cranelift or on any IR. `arandu_backend_cranelift` registers them as
//! JIT imports; the C backend emits standalone C mirrors of the same contracts
//! ("keep in sync" markers in `arandu_backend_c::emitter`) because generated C
//! must link without a Rust host.

mod ffi;

pub mod fs_runtime;
pub mod gen_runtime;
pub mod gen_runtime_gold;
pub mod genref;
pub mod genref_payload;
pub mod os_runtime;
pub mod poll_runtime;
pub mod reactor_runtime;
pub mod rt_runtime;
pub mod socket_runtime;
pub mod supervisor_runtime;
pub mod testing_runtime;
pub mod to_str_runtime;
pub mod vec_runtime;
pub mod waker_runtime;
pub mod worker_runtime;
pub mod worker_scheduler;

#[cfg(test)]
mod genref_gold_model;
