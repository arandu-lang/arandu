//! ST.13 — kqueue readiness backend (macOS, FreeBSD, OpenBSD, NetBSD, DragonFly).
//!
//! Mirrors the Linux epoll contract:
//!
//! - Sockets use `EV_ONESHOT` knotes. epoll's `EPOLLONESHOT` disarms the whole
//!   fd after the first event, while kqueue disarms per filter, so dispatch
//!   also deletes the sibling filter and ignores a second event for the same
//!   fd in the same batch. Result: exactly one wake per registration.
//! - The timer is an `EVFILT_TIMER` `EV_ONESHOT` knote in milliseconds.
//!   Re-arming deletes the old knote first, which also drops an undelivered
//!   expiry (the epoll path closes the old timerfd for the same reason).
//! - Closing a socket removes its knotes in the kernel, so close while
//!   registered is safe and a reused fd number never inherits a registration.

use std::collections::HashMap;
use std::time::Instant;

use crate::socket_runtime::{WAIT_READ, WAIT_WRITE};

/// Maximum batch size of kevents collected per poll cycle (same as epoll).
const MAX_KEVENTS: usize = 8;
/// `EVFILT_TIMER` ident. Knotes are keyed by (ident, filter), so this never
/// collides with the `EVFILT_READ`/`EVFILT_WRITE` knote of fd 1.
const TIMER_IDENT: usize = 1;

struct Registration {
    waker_id: i64,
    armed: bool,
}

pub(super) struct KqueueState {
    kq: i32,
    timer_armed: bool,
    /// Raw fd -> registration (same keying as the epoll backend).
    sockets: HashMap<i32, Registration>,
}

fn change(kq: i32, ident: usize, filter: i64, flags: i64, data: i64) -> std::io::Result<()> {
    // SAFETY: `kevent` is plain old data; zero is a valid value for every
    // field on every supported BSD (including FreeBSD's `ext` array).
    let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
    ev.ident = ident as _;
    ev.filter = filter as _;
    ev.flags = flags as _;
    ev.data = data as _;
    // SAFETY: one valid change, no event buffer, no timeout pointer.
    let rc = unsafe { libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Deletes a knote; a knote that already fired (`EV_ONESHOT`) or was never
/// added reports `ENOENT`, which is the desired end state.
fn delete(kq: i32, ident: usize, filter: i64) -> std::io::Result<()> {
    match change(kq, ident, filter, libc::EV_DELETE as i64, 0) {
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        other => other,
    }
}

impl KqueueState {
    pub(super) fn create() -> Option<Self> {
        // SAFETY: plain syscalls on a descriptor owned by this function.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return None;
        }
        // Match `EPOLL_CLOEXEC`; `kqueue1` is not available on every target.
        // SAFETY: `kq` is a live descriptor owned here.
        if unsafe { libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            // SAFETY: closing the descriptor created above exactly once.
            unsafe {
                let _ = libc::close(kq);
            }
            return None;
        }
        Some(Self {
            kq,
            timer_armed: false,
            sockets: HashMap::new(),
        })
    }

    /// Releases the kqueue (and with it every knote). Called exactly once by
    /// `close_reactor_slot`, which is itself guarded by `ReactorSlot::closed`.
    pub(super) fn close(&mut self) {
        if self.kq >= 0 {
            // SAFETY: `kq` is owned by this state and closed only here.
            unsafe {
                let _ = libc::close(self.kq);
            }
            self.kq = -1;
        }
        self.timer_armed = false;
        self.sockets.clear();
    }

    pub(super) fn arm_timer_ms(&mut self, ms: i64) -> std::io::Result<()> {
        let filter = libc::EVFILT_TIMER as i64;
        delete(self.kq, TIMER_IDENT, filter)?;
        self.timer_armed = false;
        // Some BSDs reject a zero period. A 0 ms timer is still reported on the
        // next poll: the stored deadline has already passed, so `poll` uses a
        // zero wait and reports the expiry itself.
        change(
            self.kq,
            TIMER_IDENT,
            filter,
            (libc::EV_ADD | libc::EV_ONESHOT) as i64,
            ms.max(1),
        )?;
        self.timer_armed = true;
        Ok(())
    }

    pub(super) fn register(&mut self, fd: i32, events: i64, waker_id: i64) -> i64 {
        let Ok(ident) = usize::try_from(fd) else {
            return -1;
        };
        self.sockets.insert(
            fd,
            Registration {
                waker_id,
                armed: false,
            },
        );
        for (bit, filter) in [
            (WAIT_READ, libc::EVFILT_READ as i64),
            (WAIT_WRITE, libc::EVFILT_WRITE as i64),
        ] {
            let result = if events & bit != 0 {
                // EV_ADD on an existing knote modifies it, so this rearms.
                change(
                    self.kq,
                    ident,
                    filter,
                    (libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT) as i64,
                    0,
                )
            } else {
                // Drop interest left over from a previous registration.
                delete(self.kq, ident, filter)
            };
            if result.is_err() {
                // Leave nothing half-armed: a later event must not wake.
                let _ = delete(self.kq, ident, libc::EVFILT_READ as i64);
                let _ = delete(self.kq, ident, libc::EVFILT_WRITE as i64);
                return -1;
            }
        }
        if let Some(registration) = self.sockets.get_mut(&fd) {
            registration.armed = true;
        }
        0
    }

    pub(super) fn poll(&mut self, deadline: &mut Option<Instant>, timeout_ms: i64) -> i64 {
        if !self.timer_armed && self.sockets.is_empty() {
            return super::poll_deadline_only(deadline, timeout_ms);
        }
        let due_at_entry = deadline.is_some_and(|d| Instant::now() >= d);
        let timeout = if due_at_entry {
            Some(libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            })
        } else if timeout_ms < 0 {
            None
        } else {
            Some(libc::timespec {
                tv_sec: (timeout_ms / 1000) as libc::time_t,
                tv_nsec: ((timeout_ms % 1000) * 1_000_000) as libc::c_long,
            })
        };
        let timeout_ptr = timeout
            .as_ref()
            .map_or(std::ptr::null(), |ts| ts as *const libc::timespec);

        // SAFETY: `kevent` is plain old data; the kernel fills `n` entries.
        let mut events: [libc::kevent; MAX_KEVENTS] = unsafe { std::mem::zeroed() };
        let n = loop {
            // SAFETY: no changes; the event buffer is valid for MAX_KEVENTS.
            let n = unsafe {
                libc::kevent(
                    self.kq,
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    MAX_KEVENTS as _,
                    timeout_ptr,
                )
            };
            if n >= 0 {
                break n as usize;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return -1;
            }
        };

        let mut timer_event = false;
        for event in events.iter().take(n) {
            let filter = event.filter as i64;
            if filter == libc::EVFILT_TIMER as i64 && event.ident == TIMER_IDENT {
                timer_event = true;
                continue;
            }
            if filter != libc::EVFILT_READ as i64 && filter != libc::EVFILT_WRITE as i64 {
                continue;
            }
            let Ok(fd) = i32::try_from(event.ident) else {
                continue;
            };
            let Some(registration) = self.sockets.get_mut(&fd) else {
                continue;
            };
            if !registration.armed {
                continue;
            }
            registration.armed = false;
            let waker_id = registration.waker_id;
            // The fired filter is gone (EV_ONESHOT); disarm its sibling so
            // the registration behaves like EPOLLONESHOT on the whole fd.
            let _ = delete(self.kq, event.ident, libc::EVFILT_READ as i64);
            let _ = delete(self.kq, event.ident, libc::EVFILT_WRITE as i64);
            if waker_id >= 0 {
                // SAFETY: waker ids are generation-checked by the waker table.
                unsafe {
                    crate::waker_runtime::ar_rt_waker_wake(waker_id);
                }
            }
        }

        if timer_event || due_at_entry {
            if !timer_event {
                let _ = delete(self.kq, TIMER_IDENT, libc::EVFILT_TIMER as i64);
            }
            self.timer_armed = false;
            *deadline = None;
            return 1;
        }
        0
    }
}
