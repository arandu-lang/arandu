//! ST.13 — Windows readiness backend: `WSAPoll` over runtime-owned one-shot
//! registrations.
//!
//! Why WSAPoll and not AFD or IOCP completions (full rationale in
//! `docs/arandu-async-runtime-design-v0.1.md`, "ST.13"):
//!
//! - The ABI is readiness-based: the caller is woken, then performs its own
//!   nonblocking `ar_rt_tcp_read`/`write`. IOCP completions (Go netpoll,
//!   libuv's TCP handles) need runtime-owned overlapped operations and buffers,
//!   which this ABI does not have.
//! - `\Device\Afd` + `IOCTL_AFD_POLL` (wepoll, mio/tokio, libuv `uv_poll`)
//!   scales better but is undocumented. The documented successor
//!   (`ProcessSocketNotifications`) needs Windows build 20348+, so Windows 10
//!   clients would need a second backend.
//! - WSAPoll is documented and level-triggered; one-shot is enforced here by
//!   disarming in user space. Its known defect, failed nonblocking connects
//!   not reported before Windows 10 2004, cannot be reached through this ABI:
//!   `ar_rt_tcp_connect` is blocking and reports failure itself.
//!
//! Handle reuse: registrations are keyed by the generation-tagged socket id,
//! not by the `SOCKET` value. Every poll revalidates the id and holds a
//! [`SocketLease`] across `WSAPoll`, so a socket closed by another thread is
//! dropped from the poll set and its `SOCKET` value cannot be recycled to a
//! new socket while the kernel is still waiting on it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use windows_sys::Win32::Networking::WinSock::{
    POLLRDNORM, POLLWRNORM, SOCKET, SOCKET_ERROR, WSAPOLLFD, WSAPoll,
};

use crate::socket_runtime::{SocketLease, WAIT_READ, WAIT_WRITE, lease_socket};

struct Registration {
    raw: SOCKET,
    events: i16,
    waker_id: i64,
    armed: bool,
}

#[derive(Default)]
pub(super) struct WsaPollState {
    /// Generation-tagged TCP id -> registration. Ordered for a stable poll set.
    sockets: BTreeMap<i64, Registration>,
}

/// Rounds up so a sub-millisecond remainder never turns into a busy loop.
fn ceil_ms(duration: Duration) -> i32 {
    let ms = duration.as_nanos().div_ceil(1_000_000);
    ms.min(i32::MAX as u128) as i32
}

impl WsaPollState {
    /// Drops every registration. Leases are per poll, so nothing else is held.
    pub(super) fn close(&mut self) {
        self.sockets.clear();
    }

    pub(super) fn register(
        &mut self,
        sock_id: i64,
        raw: SOCKET,
        events: i64,
        waker_id: i64,
    ) -> i64 {
        let mut interest = 0i16;
        if events & WAIT_READ != 0 {
            interest |= POLLRDNORM;
        }
        if events & WAIT_WRITE != 0 {
            interest |= POLLWRNORM;
        }
        self.sockets.insert(
            sock_id,
            Registration {
                raw,
                events: interest,
                waker_id,
                armed: true,
            },
        );
        0
    }

    pub(super) fn poll(&mut self, deadline: &mut Option<Instant>, timeout_ms: i64) -> i64 {
        // Revalidate ids: a closed socket loses its registration (close while
        // registered is safe); live ones are leased for the whole wait.
        let mut leases: Vec<SocketLease> = Vec::new();
        let mut ids = Vec::new();
        let mut fds = Vec::new();
        self.sockets.retain(|&sock_id, registration| {
            let Some(lease) = lease_socket(sock_id) else {
                return false;
            };
            if registration.armed && registration.events != 0 {
                leases.push(lease);
                ids.push(sock_id);
                fds.push(WSAPOLLFD {
                    fd: registration.raw,
                    events: registration.events,
                    revents: 0,
                });
            }
            true
        });
        if fds.is_empty() {
            // WSAPoll rejects an empty set; behave like the timer-only path.
            return super::poll_deadline_only(deadline, timeout_ms);
        }
        let Ok(count) = u32::try_from(fds.len()) else {
            return -1;
        };
        let caller_end = u64::try_from(timeout_ms)
            .ok()
            .map(|ms| Instant::now() + Duration::from_millis(ms));

        loop {
            let now = Instant::now();
            let due = deadline.is_some_and(|d| now >= d);
            let wait = if due {
                0
            } else {
                let until_timer = deadline.map(|d| ceil_ms(d.saturating_duration_since(now)));
                let until_caller = caller_end.map(|e| ceil_ms(e.saturating_duration_since(now)));
                match (until_timer, until_caller) {
                    (Some(a), Some(b)) => a.min(b),
                    (Some(a), None) | (None, Some(a)) => a,
                    (None, None) => -1,
                }
            };
            for fd in &mut fds {
                fd.revents = 0;
            }
            // SAFETY: `fds` holds `count` initialized entries whose sockets are
            // kept open by `leases` until this function returns.
            let rc = unsafe { WSAPoll(fds.as_mut_ptr(), count, wait) };
            if rc == SOCKET_ERROR {
                return -1;
            }
            let mut woke = false;
            if rc > 0 {
                for (fd, sock_id) in fds.iter().zip(&ids) {
                    if fd.revents == 0 {
                        continue;
                    }
                    let Some(registration) = self.sockets.get_mut(sock_id) else {
                        continue;
                    };
                    if !registration.armed {
                        continue;
                    }
                    // POLLERR/POLLHUP/POLLNVAL also wake: the caller observes
                    // the error on its next read/write, as with EPOLLERR.
                    registration.armed = false;
                    woke = true;
                    if registration.waker_id >= 0 {
                        // SAFETY: waker ids are generation-checked by the table.
                        unsafe {
                            crate::waker_runtime::ar_rt_waker_wake(registration.waker_id);
                        }
                    }
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                *deadline = None;
                return 1;
            }
            if woke || wait == 0 || caller_end.is_some_and(|e| Instant::now() >= e) {
                return 0;
            }
            // WSAPoll sleeps in scheduler ticks and may return slightly early;
            // keep waiting for the remainder instead of reporting a timeout.
        }
    }
}
