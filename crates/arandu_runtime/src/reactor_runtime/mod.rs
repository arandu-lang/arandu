//! SL_R.2 / SL_R.3 / ST.13 — cooperative reactor host.
//!
//! One C ABI (`ar_rt_reactor_*`) with one readiness backend per host family:
//!
//! - Linux: epoll + timerfd; io_uring timeouts when the kernel allows them.
//! - macOS / FreeBSD / OpenBSD / NetBSD / DragonFly: kqueue (`kqueue.rs`).
//! - Windows: WSAPoll over the reactor's registration table (`wsapoll.rs`).
//! - Anything else: timers only; socket registration fails closed with
//!   [`REGISTER_UNSUPPORTED`].
//!
//! Every backend exposes the same contract: socket registration is one-shot
//! (one wake per registration, register again to rearm), readiness does not
//! depend on an armed timer, and `poll` returns 1 only when the armed timer
//! fired, 0 otherwise and -1 on error.
// All `pub unsafe extern "C"` fns here are ABI host functions called only from JIT-compiled
// Arandu code. Safety invariants are enforced by the compiler and JIT symbol resolution.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Portable / no OS reactor.
pub const BACKEND_PORTABLE: i64 = 0;
/// Linux epoll + timerfd.
pub const BACKEND_EPOLL: i64 = 1;
/// Linux io_uring (timeout ops).
pub const BACKEND_IO_URING: i64 = 2;
/// BSD / macOS kqueue (EVFILT_READ / EVFILT_WRITE / EVFILT_TIMER).
pub const BACKEND_KQUEUE: i64 = 3;
/// Windows WSAPoll readiness with one-shot registrations kept by the runtime.
pub const BACKEND_WSAPOLL: i64 = 4;

/// [`ar_rt_reactor_register_socket`] result on a host without a readiness
/// backend. Distinct from `-1` (invalid handle / OS failure) so callers can
/// tell "unsupported here" from "failed".
pub const REGISTER_UNSUPPORTED: i64 = -2;

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod kqueue;
#[cfg(windows)]
mod wsapoll;

/// Default capacity for the reactor's io_uring ring buffer.
#[cfg(target_os = "linux")]
const IO_URING_DEFAULT_ENTRIES: u32 = 8;
/// User data tag for timeout completions in io_uring.
#[cfg(target_os = "linux")]
const IO_URING_USER_DATA_TIMEOUT: u64 = 1;
/// Maximum batch size of epoll events collected per poll cycle.
#[cfg(target_os = "linux")]
const MAX_EPOLL_EVENTS: usize = 8;
/// Size in bytes of the timerfd notification counter buffer (uint64_t).
#[cfg(target_os = "linux")]
const TIMERFD_COUNTER_SIZE: usize = 8;

/// Opaque reactor id (>= 0). Invalid / closed = negative.
type ReactorId = i64;

#[cfg(target_os = "linux")]
struct RegisteredSocket {
    waker_id: i64,
}

struct ReactorSlot {
    closed: bool,
    /// Linux: epoll fd. Portable fallback: ignored (sleep only).
    #[cfg(target_os = "linux")]
    epoll_fd: i32,
    /// Optional one-shot timer fd still registered (linux).
    #[cfg(target_os = "linux")]
    timer_fd: Option<i32>,
    /// Capped ring reused for timeouts.
    #[cfg(target_os = "linux")]
    ring: Option<io_uring::IoUring>,
    /// Wall-clock deadline for the armed timer (all platforms).
    deadline: Option<Instant>,
    /// Sockets registered to this reactor. Map from raw FD to RegisteredSocket.
    #[cfg(target_os = "linux")]
    sockets: HashMap<i64, RegisteredSocket>,
    /// kqueue fd, kernel timer state and one-shot registrations.
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    kqueue: kqueue::KqueueState,
    /// One-shot registrations keyed by generation-tagged socket id.
    #[cfg(windows)]
    wsapoll: wsapoll::WsaPollState,
}

type SharedReactor = Arc<Mutex<ReactorSlot>>;

/// A published reactor id keeps its generation. Destroy bumps it, so a stale
/// id cannot address the reactor that later occupies the same index (same
/// packing as TCP and waker handles). Generation 0 makes the first id equal
/// the index, so first-use ids are unchanged.
struct ReactorEntry {
    generation: u32,
    slot: Option<SharedReactor>,
}

const REACTOR_INDEX_MASK: u64 = 0xFFFF_FFFF;
const REACTOR_GENERATION_MASK: u32 = 0x7FFF_FFFF;

static REACTORS: Mutex<Vec<ReactorEntry>> = Mutex::new(Vec::new());

fn pack_reactor_id(index: usize, generation: u32) -> ReactorId {
    (((u64::from(generation) & u64::from(REACTOR_GENERATION_MASK)) << 32)
        | (index as u64 & REACTOR_INDEX_MASK)) as i64
}

fn unpack_reactor_id(id: ReactorId) -> Option<(usize, u32)> {
    if id < 0 {
        return None;
    }
    let raw = id as u64;
    let generation = ((raw >> 32) & u64::from(REACTOR_GENERATION_MASK)) as u32;
    Some(((raw & REACTOR_INDEX_MASK) as usize, generation))
}

fn bump_reactor_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1) & REACTOR_GENERATION_MASK;
    if next == 0 { 1 } else { next }
}

fn lock_reactors() -> std::sync::MutexGuard<'static, Vec<ReactorEntry>> {
    lock_reactor_table(&REACTORS)
}

fn lock_reactor_table(
    table: &Mutex<Vec<ReactorEntry>>,
) -> std::sync::MutexGuard<'_, Vec<ReactorEntry>> {
    match table.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            // A panic while holding the registry lock means the interrupted
            // mutation cannot be proven complete. Invalidate every opaque id
            // and release owned OS resources instead of exposing partial state.
            // Entries are kept with a bumped generation so ids published
            // before the panic can never alias a reactor created after it.
            let mut guard = poisoned.into_inner();
            for entry in guard.iter_mut() {
                if let Some(reactor) = entry.slot.take() {
                    let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
                    close_reactor_slot(&mut slot);
                }
                entry.generation = bump_reactor_generation(entry.generation);
            }
            table.clear_poison();
            guard
        }
    }
}

fn get_reactor(id: ReactorId) -> Option<SharedReactor> {
    let (index, generation) = unpack_reactor_id(id)?;
    let table = lock_reactors();
    let entry = table.get(index)?;
    if entry.generation != generation {
        return None;
    }
    entry.slot.clone()
}

/// Publishes a reactor in the first free entry. Returns -1 when the index
/// space is exhausted (the packed id must stay non-negative).
fn publish_reactor(slot: ReactorSlot) -> ReactorId {
    let slot = Arc::new(Mutex::new(slot));
    let mut table = lock_reactors();
    if let Some(index) = table.iter().position(|entry| entry.slot.is_none()) {
        let generation = table[index].generation;
        table[index].slot = Some(slot);
        return pack_reactor_id(index, generation);
    }
    if table.len() > REACTOR_INDEX_MASK as usize {
        drop(table);
        let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        close_reactor_slot(&mut slot);
        return -1;
    }
    let index = table.len();
    table.push(ReactorEntry {
        generation: 0,
        slot: Some(slot),
    });
    pack_reactor_id(index, 0)
}

fn close_reactor_slot(slot: &mut ReactorSlot) {
    if slot.closed {
        return;
    }
    slot.closed = true;
    #[cfg(target_os = "linux")]
    {
        if let Some(timer_fd) = slot.timer_fd {
            unsafe {
                let _ = libc::close(timer_fd);
            }
        }
        unsafe {
            let _ = libc::close(slot.epoll_fd);
        }
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    slot.kqueue.close();
    #[cfg(windows)]
    slot.wsapoll.close();
}

fn probe_backend() -> i64 {
    #[cfg(target_os = "linux")]
    {
        let proc_ok = std::fs::read_to_string("/proc/sys/kernel/io_uring_disabled")
            .map(|s| s.trim() == "0")
            .unwrap_or(true);
        if proc_ok && try_io_uring_setup() {
            return BACKEND_IO_URING;
        }
        BACKEND_EPOLL
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        BACKEND_KQUEUE
    }
    #[cfg(windows)]
    {
        BACKEND_WSAPOLL
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        windows
    )))]
    {
        BACKEND_PORTABLE
    }
}

#[cfg(target_os = "linux")]
fn try_io_uring_setup() -> bool {
    match io_uring::IoUring::new(IO_URING_DEFAULT_ENTRIES) {
        Ok(_ring) => true,
        Err(_) => false,
    }
}

/// Queries the active reactor backend.
///
/// # Safety
/// Safe to call from any thread; queries and caches the detected host backend.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_backend() -> i64 {
    use std::sync::OnceLock;
    static CACHED: OnceLock<i64> = OnceLock::new();
    *CACHED.get_or_init(probe_backend)
}

/// Creates a new reactor instance. Returns reactor ID or -1 on error.
///
/// # Safety
/// Safe to call from any thread; allocates reactor slot in synchronized table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_create() -> ReactorId {
    #[cfg(target_os = "linux")]
    {
        let epfd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epfd < 0 {
            return -1;
        }
        let ring = if unsafe { ar_rt_reactor_backend() } == BACKEND_IO_URING {
            io_uring::IoUring::new(IO_URING_DEFAULT_ENTRIES).ok()
        } else {
            None
        };
        let slot = ReactorSlot {
            closed: false,
            epoll_fd: epfd,
            timer_fd: None,
            ring,
            deadline: None,
            sockets: HashMap::new(),
        };
        publish_reactor(slot)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let slot = ReactorSlot {
            closed: false,
            deadline: None,
            #[cfg(any(
                target_os = "macos",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            kqueue: match kqueue::KqueueState::create() {
                Some(state) => state,
                None => return -1,
            },
            #[cfg(windows)]
            wsapoll: wsapoll::WsaPollState::default(),
        };
        publish_reactor(slot)
    }
}

/// Destroys a reactor instance and closes its associated resources.
///
/// # Safety
/// `id` must be a valid reactor ID created by [`ar_rt_reactor_create`], or negative no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_destroy(id: ReactorId) {
    let Some((index, generation)) = unpack_reactor_id(id) else {
        return;
    };
    let reactor = {
        let mut table = lock_reactors();
        let Some(entry) = table.get_mut(index) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        let Some(reactor) = entry.slot.take() else {
            return;
        };
        entry.generation = bump_reactor_generation(entry.generation);
        reactor
    };
    // Waits on this reactor's own lock, after any in-flight poll returns.
    let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
    close_reactor_slot(&mut slot);
}

/// Sleeps for `ms` milliseconds using the reactor.
///
/// # Safety
/// `id` must be a valid reactor ID created by [`ar_rt_reactor_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_sleep_ms(id: ReactorId, ms: i64) -> i64 {
    if id < 0 || ms < 0 {
        return -1;
    }
    #[cfg(target_os = "linux")]
    {
        if unsafe { ar_rt_reactor_backend() } == BACKEND_IO_URING
            && let Some(reactor) = get_reactor(id)
            && let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner())
            && !slot.closed
            && unsafe { sleep_ms_io_uring(&mut slot, ms as u64) }
        {
            return 0;
        }
    }
    if unsafe { ar_rt_reactor_arm_timer_ms(id, ms) } != 0 {
        return -1;
    }
    let rc = unsafe { ar_rt_reactor_poll_ms(id, -1) };
    if rc < 0 {
        return -1;
    }
    0
}

/// The global reactor table lock is not held here. The caller already cloned
/// this slot's `Arc` and dropped the table guard, so two reactors can wait at
/// the same time. Only this slot's mutex covers `submit_and_wait`.
#[cfg(target_os = "linux")]
unsafe fn sleep_ms_io_uring(slot: &mut ReactorSlot, ms: u64) -> bool {
    let Some(ring) = &mut slot.ring else {
        return false;
    };
    use io_uring::types;
    let ts = types::Timespec::from(Duration::from_millis(ms));
    let entry = io_uring::opcode::Timeout::new(&ts)
        .build()
        .user_data(IO_URING_USER_DATA_TIMEOUT);
    unsafe {
        if ring.submission().push(&entry).is_err() {
            return false;
        }
    }
    if ring.submit_and_wait(1).is_err() {
        return false;
    }
    let mut cq = ring.completion();
    cq.next().is_some()
}

/// Arms a timer in the reactor for `ms` milliseconds.
///
/// # Safety
/// `id` must be a valid reactor ID created by [`ar_rt_reactor_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_arm_timer_ms(id: ReactorId, ms: i64) -> i64 {
    if id < 0 || ms < 0 {
        return -1;
    }
    let Some(reactor) = get_reactor(id) else {
        return -1;
    };
    let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
    if slot.closed {
        return -1;
    }
    slot.deadline = Some(Instant::now() + Duration::from_millis(ms as u64));

    #[cfg(target_os = "linux")]
    {
        if let Some(old) = slot.timer_fd.take() {
            unsafe {
                let _ = libc::epoll_ctl(
                    slot.epoll_fd,
                    libc::EPOLL_CTL_DEL,
                    old,
                    std::ptr::null_mut(),
                );
                let _ = libc::close(old);
            }
        }
        let tfd = unsafe {
            libc::timerfd_create(
                libc::CLOCK_MONOTONIC,
                libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
            )
        };
        if tfd < 0 {
            return -1;
        }
        let its = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: (ms / 1000) as libc::time_t,
                tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
            },
        };
        let mut its = its;
        if its.it_value.tv_sec == 0 && its.it_value.tv_nsec == 0 {
            its.it_value.tv_nsec = 1;
        }
        if unsafe { libc::timerfd_settime(tfd, 0, &its, std::ptr::null_mut()) } < 0 {
            unsafe {
                let _ = libc::close(tfd);
            }
            return -1;
        }
        let mut ev: libc::epoll_event = unsafe { std::mem::zeroed() };
        ev.events = libc::EPOLLIN as u32;
        ev.u64 = tfd as u64;
        if unsafe { libc::epoll_ctl(slot.epoll_fd, libc::EPOLL_CTL_ADD, tfd, &mut ev) } < 0 {
            unsafe {
                let _ = libc::close(tfd);
            }
            return -1;
        }
        slot.timer_fd = Some(tfd);
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        if slot.kqueue.arm_timer_ms(ms).is_err() {
            return -1;
        }
    }
    0
}

/// Polls the reactor for pending events up to `timeout_ms` milliseconds.
///
/// # Safety
/// `id` must be a valid reactor ID created by [`ar_rt_reactor_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_poll_ms(id: ReactorId, timeout_ms: i64) -> i64 {
    if id < 0 {
        return -1;
    }

    #[cfg(target_os = "linux")]
    {
        let Some(reactor) = get_reactor(id) else {
            return -1;
        };
        // Blocking waits hold only this reactor's lock. Other reactor IDs can
        // continue to arm, poll, or destroy independently.
        let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
        if slot.closed {
            return -1;
        }
        let epfd = slot.epoll_fd;
        let tfd_opt = slot.timer_fd;
        let deadline = slot.deadline;
        let has_sockets = !slot.sockets.is_empty();

        // Socket readiness is independent of timer registration. Only use
        // the sleep-only fallback when epoll has no registered I/O sources.
        if tfd_opt.is_none() && !has_sockets {
            if let Some(dl) = deadline {
                let now = Instant::now();
                if now >= dl {
                    slot.deadline = None;
                    return 1;
                }
                let remaining = dl.saturating_duration_since(now);
                let wait_ms = if timeout_ms < 0 {
                    remaining.as_millis() as i64
                } else {
                    remaining.as_millis().min(timeout_ms as u128) as i64
                };
                if wait_ms > 0 {
                    std::thread::sleep(Duration::from_millis(wait_ms as u64));
                }
                if slot.deadline.is_some_and(|d| Instant::now() >= d) {
                    slot.deadline = None;
                    return 1;
                }
                return 0;
            }
            if timeout_ms > 0 {
                std::thread::sleep(Duration::from_millis(timeout_ms as u64));
            }
            return 0;
        }

        let mut events: [libc::epoll_event; MAX_EPOLL_EVENTS] = unsafe { std::mem::zeroed() };
        let timeout_arg = if timeout_ms < 0 {
            -1
        } else {
            timeout_ms.min(i32::MAX as i64) as i32
        };
        let mut n;
        loop {
            n = unsafe {
                libc::epoll_wait(epfd, events.as_mut_ptr(), events.len() as i32, timeout_arg)
            };
            if n >= 0 {
                break;
            }
            let err = std::io::Error::last_os_error().raw_os_error();
            if err != Some(libc::EINTR) {
                return -1;
            }
        }
        if n == 0 {
            return 0;
        }

        let mut timer_fired = false;
        for event in events.iter().take(n as usize) {
            let fd_val = event.u64 as i32;
            if Some(fd_val) == tfd_opt {
                let mut buf = [0u8; TIMERFD_COUNTER_SIZE];
                let _ = unsafe { libc::read(fd_val, buf.as_mut_ptr() as *mut _, buf.len()) };
                timer_fired = true;
            } else {
                let waker_to_wake = slot.sockets.get(&(fd_val as i64)).map(|rs| rs.waker_id);
                if let Some(waker_id) = waker_to_wake
                    && waker_id >= 0
                {
                    unsafe {
                        crate::waker_runtime::ar_rt_waker_wake(waker_id);
                    }
                }
            }
        }

        if timer_fired {
            if let Some(old) = slot.timer_fd.take() {
                unsafe {
                    let _ = libc::epoll_ctl(
                        slot.epoll_fd,
                        libc::EPOLL_CTL_DEL,
                        old,
                        std::ptr::null_mut(),
                    );
                    let _ = libc::close(old);
                }
            }
            slot.deadline = None;
            return 1;
        }
        0
    }

    #[cfg(not(target_os = "linux"))]
    {
        let Some(reactor) = get_reactor(id) else {
            return -1;
        };
        // As on Linux, a blocking wait holds only this reactor's lock.
        let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
        if slot.closed {
            return -1;
        }
        let slot = &mut *slot;
        #[cfg(any(
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            slot.kqueue.poll(&mut slot.deadline, timeout_ms)
        }
        #[cfg(windows)]
        {
            slot.wsapoll.poll(&mut slot.deadline, timeout_ms)
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly",
            windows
        )))]
        {
            poll_deadline_only(&mut slot.deadline, timeout_ms)
        }
    }
}

/// Timer-only wait shared by the non-Linux backends when no I/O source is
/// registered. Returns 1 when the deadline is reached (and clears it), else 0.
#[cfg(not(target_os = "linux"))]
fn poll_deadline_only(deadline: &mut Option<Instant>, timeout_ms: i64) -> i64 {
    let Some(dl) = *deadline else {
        if timeout_ms > 0 {
            std::thread::sleep(Duration::from_millis(timeout_ms as u64));
        }
        return 0;
    };
    let now = Instant::now();
    if now >= dl {
        *deadline = None;
        return 1;
    }
    let remaining = dl.saturating_duration_since(now);
    let wait = if timeout_ms < 0 {
        remaining
    } else {
        remaining.min(Duration::from_millis(timeout_ms as u64))
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        *deadline = None;
        return 1;
    }
    0
}

/// Registers a socket with the reactor.
///
/// # Safety
/// `reactor_id`, `sock_id`, and `waker_id` must be valid descriptors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_register_socket(
    reactor_id: i64,
    sock_id: i64,
    events: i64,
    waker_id: i64,
) -> i64 {
    if reactor_id < 0 || sock_id < 0 {
        return -1;
    }
    #[cfg(target_os = "linux")]
    {
        let fd = match crate::socket_runtime::get_socket_fd(sock_id) {
            Some(fd) => fd,
            None => return -1,
        };

        let Some(reactor) = get_reactor(reactor_id) else {
            return -1;
        };
        let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
        if slot.closed {
            return -1;
        }

        slot.sockets
            .insert(fd as i64, RegisteredSocket { waker_id });

        let mut ev: libc::epoll_event = unsafe { std::mem::zeroed() };
        if events & crate::socket_runtime::WAIT_READ != 0 {
            ev.events |= libc::EPOLLIN as u32;
        }
        if events & crate::socket_runtime::WAIT_WRITE != 0 {
            ev.events |= libc::EPOLLOUT as u32;
        }
        ev.events |= libc::EPOLLONESHOT as u32;
        ev.u64 = fd as u64;

        unsafe {
            let rc = libc::epoll_ctl(slot.epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut ev);
            if rc < 0 {
                let err = *libc::__errno_location();
                if err == libc::EEXIST {
                    libc::epoll_ctl(slot.epoll_fd, libc::EPOLL_CTL_MOD, fd, &mut ev);
                } else {
                    return -1;
                }
            }
        }
        0
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        let Some(fd) = crate::socket_runtime::get_socket_fd(sock_id) else {
            return -1;
        };
        let Some(reactor) = get_reactor(reactor_id) else {
            return -1;
        };
        let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
        if slot.closed {
            return -1;
        }
        slot.kqueue.register(fd, events, waker_id)
    }
    #[cfg(windows)]
    {
        let Some(raw) = crate::socket_runtime::get_socket_raw(sock_id) else {
            return -1;
        };
        let Some(reactor) = get_reactor(reactor_id) else {
            return -1;
        };
        let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
        if slot.closed {
            return -1;
        }
        slot.wsapoll.register(sock_id, raw, events, waker_id)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        windows
    )))]
    {
        // Fail closed: no readiness backend on this host. Validate the
        // reactor handle first so a stale id still reports `-1`.
        let _ = (sock_id, events, waker_id);
        match get_reactor(reactor_id) {
            Some(_) => REGISTER_UNSUPPORTED,
            None => -1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_registry_is_quarantined_before_reuse() {
        let live = ReactorSlot {
            closed: false,
            deadline: None,
            #[cfg(target_os = "linux")]
            epoll_fd: -1,
            #[cfg(target_os = "linux")]
            timer_fd: None,
            #[cfg(target_os = "linux")]
            ring: None,
            #[cfg(target_os = "linux")]
            sockets: HashMap::new(),
            #[cfg(any(
                target_os = "macos",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            kqueue: kqueue::KqueueState::create().expect("kqueue for poison test"),
            #[cfg(windows)]
            wsapoll: wsapoll::WsaPollState::default(),
        };
        let live = Arc::new(Mutex::new(live));
        let table = std::sync::Arc::new(Mutex::new(vec![ReactorEntry {
            generation: 0,
            slot: Some(Arc::clone(&live)),
        }]));
        let poison_target = std::sync::Arc::clone(&table);
        let _ = std::thread::spawn(move || {
            let mut guard = poison_target.lock().expect("fresh registry lock");
            guard.push(ReactorEntry {
                generation: 0,
                slot: None,
            });
            panic!("interrupt registry mutation");
        })
        .join();

        assert!(table.is_poisoned());
        let guard = lock_reactor_table(&table);
        assert!(
            guard.iter().all(|entry| entry.slot.is_none()),
            "partial registry state must be discarded"
        );
        assert!(
            guard.iter().all(|entry| entry.generation != 0),
            "ids published before the panic must not alias later reactors"
        );
        drop(guard);
        assert!(live.lock().expect("live slot").closed, "resources released");
        assert!(!table.is_poisoned(), "recovered registry must be reusable");
    }

    #[test]
    fn stale_reactor_id_does_not_alias_a_reused_slot() {
        unsafe {
            let first = ar_rt_reactor_create();
            assert!(first >= 0);
            ar_rt_reactor_destroy(first);
            let second = ar_rt_reactor_create();
            assert!(second >= 0);
            assert_ne!(first, second);
            assert_eq!(ar_rt_reactor_arm_timer_ms(first, 1), -1);
            assert_eq!(ar_rt_reactor_poll_ms(first, 0), -1);
            // Destroying the stale id must not release the live reactor.
            ar_rt_reactor_destroy(first);
            assert_eq!(ar_rt_reactor_arm_timer_ms(second, 1), 0);
            assert_eq!(ar_rt_reactor_poll_ms(second, 200), 1);
            ar_rt_reactor_destroy(second);
        }
    }

    #[test]
    fn two_reactors_sleep_without_serializing_on_the_table_lock() {
        unsafe {
            let first = ar_rt_reactor_create();
            let second = ar_rt_reactor_create();
            assert!(first >= 0 && second >= 0);
            let started = std::time::Instant::now();
            let other = std::thread::spawn(move || ar_rt_reactor_sleep_ms(second, 200));
            assert_eq!(ar_rt_reactor_sleep_ms(first, 200), 0);
            assert_eq!(other.join().expect("second reactor"), 0);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(350),
                "two reactor sleeps serialized: {:?}",
                started.elapsed()
            );
            ar_rt_reactor_destroy(first);
            ar_rt_reactor_destroy(second);
        }
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        windows
    )))]
    #[test]
    fn portable_reactor_explicitly_rejects_socket_registration() {
        let (listener, client, server) = crate::socket_runtime::tests::connected_pair();
        // SAFETY: this test owns the socket/reactor/waker handles and destroys
        // them only after all operations using them have completed.
        unsafe {
            let reactor = ar_rt_reactor_create();
            let waker = crate::waker_runtime::ar_rt_waker_create();
            assert!(reactor >= 0);
            assert_eq!(ar_rt_reactor_backend(), BACKEND_PORTABLE);
            assert_eq!(
                ar_rt_reactor_register_socket(reactor, server, 1, waker),
                REGISTER_UNSUPPORTED
            );
            ar_rt_reactor_destroy(reactor);
            assert_eq!(ar_rt_reactor_register_socket(reactor, server, 1, waker), -1);
            crate::waker_runtime::ar_rt_waker_destroy(waker);
            crate::socket_runtime::ar_rt_tcp_close(client);
            crate::socket_runtime::ar_rt_tcp_close(server);
            crate::socket_runtime::ar_rt_tcp_close(listener);
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        windows
    ))]
    #[test]
    fn socket_readiness_wakes_without_an_armed_timer_and_can_be_rearmed() {
        use crate::socket_runtime::{
            WAIT_READ, ar_rt_tcp_close, ar_rt_tcp_write, tests::connected_pair,
        };
        use crate::waker_runtime::{ar_rt_waker_create, ar_rt_waker_destroy, ar_rt_waker_wait};
        let (listener, client, server) = connected_pair();
        // SAFETY: all handles remain owned and live until cleanup; the byte
        // passed to write is valid for its stated length for the entire call.
        unsafe {
            let reactor = ar_rt_reactor_create();
            assert!(reactor >= 0);
            let waker = ar_rt_waker_create();
            assert_eq!(
                ar_rt_reactor_register_socket(reactor, server, WAIT_READ, waker),
                0
            );
            assert_eq!(ar_rt_tcp_write(client, b"x".as_ptr(), 1), 1);
            assert!(ar_rt_reactor_poll_ms(reactor, 100) >= 0);
            assert_eq!(
                ar_rt_waker_wait(waker, 0),
                1,
                "readiness must not depend on a timer"
            );
            // EPOLLONESHOT suppresses another notification until rearmed.
            assert!(ar_rt_reactor_poll_ms(reactor, 0) >= 0);
            assert_eq!(ar_rt_waker_wait(waker, 0), 0);
            assert_eq!(
                ar_rt_reactor_register_socket(reactor, server, WAIT_READ, waker),
                0
            );
            assert!(ar_rt_reactor_poll_ms(reactor, 100) >= 0);
            assert_eq!(ar_rt_waker_wait(waker, 0), 1);
            ar_rt_reactor_destroy(reactor);
            ar_rt_waker_destroy(waker);
            ar_rt_tcp_close(client);
            ar_rt_tcp_close(server);
            ar_rt_tcp_close(listener);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn io_uring_sleep_does_not_hold_the_global_reactor_registry() {
        // This regression is meaningful only when the blocking io_uring path
        // is available; other backends already wait without the registry lock.
        if unsafe { ar_rt_reactor_backend() } != BACKEND_IO_URING {
            return;
        }
        let sleeping_reactor = unsafe { ar_rt_reactor_create() };
        assert!(sleeping_reactor >= 0);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let sleeper = std::thread::spawn(move || unsafe {
            started_tx.send(()).expect("signal before blocking sleep");
            let result = ar_rt_reactor_sleep_ms(sleeping_reactor, 500);
            ar_rt_reactor_destroy(sleeping_reactor);
            result
        });
        started_rx.recv().expect("sleep worker started");
        std::thread::sleep(Duration::from_millis(30));

        let (created_tx, created_rx) = std::sync::mpsc::channel();
        let creator = std::thread::spawn(move || unsafe {
            let reactor = ar_rt_reactor_create();
            if reactor >= 0 {
                ar_rt_reactor_destroy(reactor);
            }
            created_tx.send(reactor).expect("report reactor creation");
        });
        let created = created_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("another reactor must remain available during io_uring wait");
        assert!(created >= 0);
        creator.join().expect("creator thread");
        assert_eq!(sleeper.join().expect("sleeper thread"), 0);
    }

    #[test]
    fn create_sleep_destroy() {
        unsafe {
            let r = ar_rt_reactor_create();
            assert!(r >= 0);
            let t0 = Instant::now();
            assert_eq!(ar_rt_reactor_sleep_ms(r, 5), 0);
            assert!(t0.elapsed() >= Duration::from_millis(3));
            ar_rt_reactor_destroy(r);
        }
    }

    #[test]
    fn arm_and_poll() {
        unsafe {
            let r = ar_rt_reactor_create();
            assert_eq!(ar_rt_reactor_arm_timer_ms(r, 5), 0);
            let rc = ar_rt_reactor_poll_ms(r, 200);
            assert_eq!(rc, 1);
            ar_rt_reactor_destroy(r);
        }
    }

    #[test]
    fn backend_matches_the_host_family() {
        unsafe {
            let b = ar_rt_reactor_backend();
            #[cfg(target_os = "linux")]
            assert!(b == BACKEND_EPOLL || b == BACKEND_IO_URING, "backend={b}");
            #[cfg(any(
                target_os = "macos",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            assert_eq!(b, BACKEND_KQUEUE);
            #[cfg(windows)]
            assert_eq!(b, BACKEND_WSAPOLL);
            #[cfg(not(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly",
                windows
            )))]
            assert_eq!(b, BACKEND_PORTABLE);
        }
    }

    #[test]
    fn zero_ms_timer_fires_on_the_next_poll() {
        unsafe {
            let r = ar_rt_reactor_create();
            assert!(r >= 0);
            assert_eq!(ar_rt_reactor_arm_timer_ms(r, 0), 0);
            assert_eq!(ar_rt_reactor_poll_ms(r, 200), 1);
            // The expiry is consumed: no second report without re-arming.
            assert_eq!(ar_rt_reactor_poll_ms(r, 0), 0);
            ar_rt_reactor_destroy(r);
        }
    }

    #[test]
    fn rearming_replaces_the_previous_timer() {
        unsafe {
            let r = ar_rt_reactor_create();
            assert!(r >= 0);
            assert_eq!(ar_rt_reactor_arm_timer_ms(r, 1), 0);
            std::thread::sleep(Duration::from_millis(20));
            // The 1 ms expiry may already be pending in the kernel; re-arming
            // must discard it rather than report the new timer early.
            assert_eq!(ar_rt_reactor_arm_timer_ms(r, 10_000), 0);
            assert_eq!(ar_rt_reactor_poll_ms(r, 30), 0);
            ar_rt_reactor_destroy(r);
        }
    }

    #[test]
    fn destroyed_reactor_rejects_every_operation() {
        unsafe {
            let r = ar_rt_reactor_create();
            assert!(r >= 0);
            assert_eq!(ar_rt_reactor_arm_timer_ms(r, 1_000), 0);
            ar_rt_reactor_destroy(r);
            // Double destroy is a no-op; the slot is released exactly once.
            ar_rt_reactor_destroy(r);
            assert_eq!(ar_rt_reactor_poll_ms(r, 0), -1);
            assert_eq!(ar_rt_reactor_arm_timer_ms(r, 1), -1);
        }
    }

    /// Readiness contract shared by epoll, kqueue and WSAPoll. These run
    /// natively in CI on Linux, macOS and Windows runners.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        windows
    ))]
    mod readiness {
        use super::*;
        use crate::socket_runtime::{
            WAIT_READ, WAIT_WRITE, ar_rt_tcp_close, ar_rt_tcp_listen, ar_rt_tcp_write,
            tests::connected_pair,
        };
        use crate::waker_runtime::{ar_rt_waker_create, ar_rt_waker_destroy, ar_rt_waker_wait};

        #[test]
        fn read_and_write_interest_wakes_once_per_registration() {
            let (listener, client, server) = connected_pair();
            // SAFETY: handles stay live until cleanup; buffers outlive calls.
            unsafe {
                let reactor = ar_rt_reactor_create();
                let waker = ar_rt_waker_create();
                assert!(reactor >= 0);
                assert_eq!(ar_rt_tcp_write(client, b"x".as_ptr(), 1), 1);
                // Both filters are ready at once (data pending + send buffer
                // free). One-shot covers the socket, not each direction.
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, server, WAIT_READ | WAIT_WRITE, waker),
                    0
                );
                assert!(ar_rt_reactor_poll_ms(reactor, 200) >= 0);
                assert_eq!(ar_rt_waker_wait(waker, 0), 1);
                assert!(ar_rt_reactor_poll_ms(reactor, 20) >= 0);
                assert_eq!(ar_rt_waker_wait(waker, 0), 0, "second wake leaked");
                ar_rt_reactor_destroy(reactor);
                ar_rt_waker_destroy(waker);
                ar_rt_tcp_close(client);
                ar_rt_tcp_close(server);
                ar_rt_tcp_close(listener);
            }
        }

        #[test]
        fn write_interest_reports_an_idle_connected_socket() {
            let (listener, client, server) = connected_pair();
            unsafe {
                let reactor = ar_rt_reactor_create();
                let waker = ar_rt_waker_create();
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, client, WAIT_WRITE, waker),
                    0
                );
                assert!(ar_rt_reactor_poll_ms(reactor, 200) >= 0);
                assert_eq!(ar_rt_waker_wait(waker, 0), 1);
                ar_rt_reactor_destroy(reactor);
                ar_rt_waker_destroy(waker);
                ar_rt_tcp_close(client);
                ar_rt_tcp_close(server);
                ar_rt_tcp_close(listener);
            }
        }

        #[test]
        fn listener_read_interest_reports_a_pending_accept() {
            unsafe {
                let (listener, client, server) = connected_pair();
                let reactor = ar_rt_reactor_create();
                let waker = ar_rt_waker_create();
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, listener, WAIT_READ, waker),
                    0
                );
                let port = crate::socket_runtime::tests::listener_port(listener);
                let second = crate::socket_runtime::ar_rt_tcp_connect(i64::from(port));
                assert!(second >= 0);
                assert!(ar_rt_reactor_poll_ms(reactor, 500) >= 0);
                assert_eq!(ar_rt_waker_wait(waker, 0), 1);
                let accepted = crate::socket_runtime::ar_rt_tcp_accept(listener);
                assert!(accepted >= 0);
                ar_rt_reactor_destroy(reactor);
                ar_rt_waker_destroy(waker);
                for handle in [second, accepted, client, server, listener] {
                    ar_rt_tcp_close(handle);
                }
            }
        }

        #[test]
        fn socket_events_do_not_hide_a_due_timer() {
            let (listener, client, server) = connected_pair();
            unsafe {
                let reactor = ar_rt_reactor_create();
                let waker = ar_rt_waker_create();
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, server, WAIT_READ, waker),
                    0
                );
                assert_eq!(ar_rt_reactor_arm_timer_ms(reactor, 5), 0);
                let started = Instant::now();
                assert_eq!(ar_rt_reactor_poll_ms(reactor, -1), 1);
                assert!(started.elapsed() >= Duration::from_millis(3));
                assert_eq!(ar_rt_waker_wait(waker, 0), 0, "no readiness yet");
                ar_rt_reactor_destroy(reactor);
                ar_rt_waker_destroy(waker);
                ar_rt_tcp_close(client);
                ar_rt_tcp_close(server);
                ar_rt_tcp_close(listener);
            }
        }

        #[test]
        fn closing_a_registered_socket_is_safe_and_never_wakes_a_reused_handle() {
            let (listener, client, server) = connected_pair();
            unsafe {
                let reactor = ar_rt_reactor_create();
                let stale_waker = ar_rt_waker_create();
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, server, WAIT_READ, stale_waker),
                    0
                );
                // Close while registered, then let the OS recycle the fd /
                // SOCKET value for unrelated sockets.
                ar_rt_tcp_close(server);
                let (listener2, client2, server2) = connected_pair();
                let other = ar_rt_tcp_listen(0);
                assert!(other >= 0);
                assert_eq!(ar_rt_tcp_write(client2, b"y".as_ptr(), 1), 1);
                assert!(ar_rt_reactor_poll_ms(reactor, 50) >= 0);
                assert_eq!(
                    ar_rt_waker_wait(stale_waker, 0),
                    0,
                    "a closed registration must not observe a recycled handle"
                );
                // The stale id is rejected rather than re-resolved.
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, server, WAIT_READ, stale_waker),
                    -1
                );
                ar_rt_reactor_destroy(reactor);
                ar_rt_waker_destroy(stale_waker);
                for handle in [client, listener, listener2, client2, server2, other] {
                    ar_rt_tcp_close(handle);
                }
            }
        }

        #[test]
        fn destroy_with_live_registrations_releases_the_reactor_once() {
            let (listener, client, server) = connected_pair();
            unsafe {
                let reactor = ar_rt_reactor_create();
                let waker = ar_rt_waker_create();
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, server, WAIT_READ, waker),
                    0
                );
                ar_rt_reactor_destroy(reactor);
                ar_rt_reactor_destroy(reactor);
                assert_eq!(
                    ar_rt_reactor_register_socket(reactor, server, WAIT_READ, waker),
                    -1
                );
                // The socket outlives the reactor and stays usable.
                assert_eq!(ar_rt_tcp_write(client, b"z".as_ptr(), 1), 1);
                let mut buf = [0u8; 1];
                assert_eq!(
                    crate::socket_runtime::ar_rt_tcp_read(server, buf.as_mut_ptr(), 1),
                    1
                );
                ar_rt_waker_destroy(waker);
                ar_rt_tcp_close(client);
                ar_rt_tcp_close(server);
                ar_rt_tcp_close(listener);
            }
        }
    }
}
