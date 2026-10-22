//! SL_R.2 / SL_R.3 — cooperative reactor host (epoll + timerfd; io_uring when available).
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
}

type SharedReactor = Arc<Mutex<ReactorSlot>>;

static REACTORS: Mutex<Vec<Option<SharedReactor>>> = Mutex::new(Vec::new());

fn lock_reactors() -> std::sync::MutexGuard<'static, Vec<Option<SharedReactor>>> {
    lock_reactor_table(&REACTORS)
}

fn lock_reactor_table(
    table: &Mutex<Vec<Option<SharedReactor>>>,
) -> std::sync::MutexGuard<'_, Vec<Option<SharedReactor>>> {
    match table.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            // A panic while holding the registry lock means the interrupted
            // mutation cannot be proven complete. Invalidate every opaque id
            // and release owned OS resources instead of exposing partial state.
            let mut guard = poisoned.into_inner();
            for reactor in guard.drain(..).flatten() {
                let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
                close_reactor_slot(&mut slot);
            }
            table.clear_poison();
            guard
        }
    }
}

fn get_reactor(id: ReactorId) -> Option<SharedReactor> {
    if id < 0 {
        return None;
    }
    lock_reactors().get(id as usize).and_then(Clone::clone)
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
    #[cfg(not(target_os = "linux"))]
    {
        let _ = slot;
    }
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
    #[cfg(not(target_os = "linux"))]
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
        let mut guard = lock_reactors();
        let slot = Arc::new(Mutex::new(slot));
        if let Some(idx) = guard.iter().position(|s| s.is_none()) {
            guard[idx] = Some(slot);
            return idx as i64;
        }
        let id = guard.len() as i64;
        guard.push(Some(slot));
        id
    }
    #[cfg(not(target_os = "linux"))]
    {
        let slot = ReactorSlot {
            closed: false,
            deadline: None,
        };
        let mut guard = lock_reactors();
        let slot = Arc::new(Mutex::new(slot));
        if let Some(idx) = guard.iter().position(|s| s.is_none()) {
            guard[idx] = Some(slot);
            return idx as i64;
        }
        let id = guard.len() as i64;
        guard.push(Some(slot));
        id
    }
}

/// Destroys a reactor instance and closes its associated resources.
///
/// # Safety
/// `id` must be a valid reactor ID created by [`ar_rt_reactor_create`], or negative no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_reactor_destroy(id: ReactorId) {
    if id < 0 {
        return;
    }
    let Some(reactor) = lock_reactors().get_mut(id as usize).and_then(Option::take) else {
        return;
    };
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
        let mut slot = reactor.lock().unwrap_or_else(|e| e.into_inner());
        if slot.closed {
            return -1;
        }
        let deadline = slot.deadline;
        let Some(dl) = deadline else {
            if timeout_ms > 0 {
                std::thread::sleep(Duration::from_millis(timeout_ms as u64));
            }
            return 0;
        };
        let now = Instant::now();
        if now >= dl {
            slot.deadline = None;
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
        if slot
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            slot.deadline = None;
            return 1;
        }
        0
    }
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
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (reactor_id, sock_id, events, waker_id);
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_registry_is_quarantined_before_reuse() {
        let table = std::sync::Arc::new(Mutex::new(vec![None]));
        let poison_target = std::sync::Arc::clone(&table);
        let _ = std::thread::spawn(move || {
            let mut guard = poison_target.lock().expect("fresh registry lock");
            guard.push(None);
            panic!("interrupt registry mutation");
        })
        .join();

        assert!(table.is_poisoned());
        let guard = lock_reactor_table(&table);
        assert!(guard.is_empty(), "partial registry state must be discarded");
        drop(guard);
        assert!(!table.is_poisoned(), "recovered registry must be reusable");
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

    #[cfg(not(target_os = "linux"))]
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
            assert_eq!(ar_rt_reactor_register_socket(reactor, server, 1, waker), -1);
            ar_rt_reactor_destroy(reactor);
            crate::waker_runtime::ar_rt_waker_destroy(waker);
            crate::socket_runtime::ar_rt_tcp_close(client);
            crate::socket_runtime::ar_rt_tcp_close(server);
            crate::socket_runtime::ar_rt_tcp_close(listener);
        }
    }

    #[cfg(target_os = "linux")]
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
    fn backend_is_linux_or_portable() {
        unsafe {
            let b = ar_rt_reactor_backend();
            assert!(
                b == BACKEND_PORTABLE || b == BACKEND_EPOLL || b == BACKEND_IO_URING,
                "backend={b}"
            );
            #[cfg(target_os = "linux")]
            assert!(b == BACKEND_EPOLL || b == BACKEND_IO_URING);
        }
    }
}
