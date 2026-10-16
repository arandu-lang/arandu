//! SL_R — host TCP sockets (blocking + nonblocking + reactor wait + io_uring I/O).
//!
//! - Blocking: [`ar_rt_tcp_read`] / [`ar_rt_tcp_write`] (default).
//! - Async surface: [`ar_rt_tcp_set_nonblocking`] + [`ar_rt_tcp_wait`] (poll/epoll)
//!   then read/write; optional waker via [`ar_rt_tcp_wait_wake`].
//! - SL_R.3: when reactor backend is io_uring, read/write use io_uring submit.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

struct SockSlot {
    kind: SockKind,
    nonblocking: bool,
}

enum SockKind {
    Listener(TcpListener),
    Stream(TcpStream),
}

static SOCKS: Mutex<Vec<Option<SockSlot>>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Option<SockSlot>>> {
    SOCKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn insert(kind: SockKind) -> i64 {
    let mut g = lock();
    let slot = SockSlot {
        kind,
        nonblocking: false,
    };
    if let Some(idx) = g.iter().position(|s| s.is_none()) {
        g[idx] = Some(slot);
        return idx as i64;
    }
    let id = g.len() as i64;
    g.push(Some(slot));
    id
}

#[cfg(unix)]
fn raw_fd_of(kind: &SockKind) -> i32 {
    match kind {
        SockKind::Listener(l) => l.as_raw_fd(),
        SockKind::Stream(s) => s.as_raw_fd(),
    }
}

#[cfg(windows)]
fn raw_socket_of(kind: &SockKind) -> windows_sys::Win32::Networking::WinSock::SOCKET {
    match kind {
        SockKind::Listener(listener) => listener.as_raw_socket() as usize,
        SockKind::Stream(stream) => stream.as_raw_socket() as usize,
    }
}

#[cfg(unix)]
pub fn get_socket_fd(sock_id: i64) -> Option<i32> {
    let g = lock();
    g.get(sock_id as usize)
        .and_then(|s| s.as_ref())
        .map(|s| raw_fd_of(&s.kind))
}

/// Events for [`ar_rt_tcp_wait`]: bit0 = readable, bit1 = writable.
pub const WAIT_READ: i64 = 1;
pub const WAIT_WRITE: i64 = 2;

/// Default connect timeout for socket establishment.
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default queue size for temporary io_uring instances.
#[cfg(target_os = "linux")]
const IO_URING_DEFAULT_ENTRIES: u32 = 8;
/// User data identifying read completions in io_uring.
#[cfg(target_os = "linux")]
const IO_URING_USER_DATA_READ: u64 = 1;
/// User data identifying write completions in io_uring.
#[cfg(target_os = "linux")]
const IO_URING_USER_DATA_WRITE: u64 = 2;

/// Listen on `127.0.0.1:port`. Returns handle >= 0 or -1.
///
/// # Safety
/// C ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_listen(port: i64) -> i64 {
    if !(0..=65535).contains(&port) {
        return -1;
    }
    let addr = format!("127.0.0.1:{port}");
    match TcpListener::bind(&addr) {
        Ok(l) => {
            let _ = l.set_nonblocking(false);
            insert(SockKind::Listener(l))
        }
        Err(_) => -1,
    }
}

/// Accept one connection. Returns stream handle >= 0 or -1.
///
/// # Safety
/// `listener` from listen.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_accept(listener: i64) -> i64 {
    if listener < 0 {
        return -1;
    }
    let cloned_listener = {
        let mut g = lock();
        let Some(Some(slot)) = g.get_mut(listener as usize) else {
            return -1;
        };
        let SockKind::Listener(l) = &slot.kind else {
            return -1;
        };
        match l.try_clone() {
            Ok(cloned) => cloned,
            Err(_) => return -1,
        }
    };
    match cloned_listener.accept() {
        Ok((stream, _)) => insert(SockKind::Stream(stream)),
        Err(_) => -1,
    }
}

/// Connect to `127.0.0.1:port`. Returns stream handle or -1.
///
/// # Safety
/// C ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_connect(port: i64) -> i64 {
    if !(0..=65535).contains(&port) {
        return -1;
    }
    let addr = format!("127.0.0.1:{port}");
    match TcpStream::connect(&addr) {
        Ok(s) => {
            let _ = s.set_read_timeout(Some(TCP_CONNECT_TIMEOUT));
            let _ = s.set_write_timeout(Some(TCP_CONNECT_TIMEOUT));
            insert(SockKind::Stream(s))
        }
        Err(_) => -1,
    }
}

/// Read up to `len` bytes into `buf`. Returns bytes read, 0 EOF, -1 error.
///
/// # Safety
/// `buf` valid for `len`; `sock` stream handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_read(sock: i64, buf: *mut u8, len: i64) -> i64 {
    if sock < 0 || buf.is_null() || len <= 0 {
        return -1;
    }
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(sock as usize) else {
        return -1;
    };
    let SockKind::Stream(s) = &mut slot.kind else {
        return -1;
    };
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, len as usize) };
    match s.read(slice) {
        Ok(n) => n as i64,
        Err(e) if e.kind() == ErrorKind::WouldBlock => -2,
        Err(_) => -1,
    }
}

/// Write `len` bytes from `buf`. Returns bytes written or -1.
///
/// # Safety
/// Fat buffer ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_write(sock: i64, buf: *const u8, len: i64) -> i64 {
    if sock < 0 || buf.is_null() || len < 0 {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(sock as usize) else {
        return -1;
    };
    let SockKind::Stream(s) = &mut slot.kind else {
        return -1;
    };
    let slice = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    match s.write(slice) {
        Ok(n) => n as i64,
        Err(e) if e.kind() == ErrorKind::WouldBlock => -2,
        Err(_) => -1,
    }
}

/// Close socket handle.
///
/// # Safety
/// Handle from listen/accept/connect.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_close(sock: i64) {
    if sock < 0 {
        return;
    }
    let mut g = lock();
    if let Some(slot) = g.get_mut(sock as usize) {
        *slot = None;
    }
}

/// Set nonblocking mode. `flag` != 0 → nonblocking.
///
/// # Safety
/// Valid socket handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_set_nonblocking(sock: i64, flag: i64) -> i64 {
    if sock < 0 {
        return -1;
    }
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(sock as usize) else {
        return -1;
    };
    let nb = flag != 0;
    let ok = match &mut slot.kind {
        SockKind::Listener(l) => l.set_nonblocking(nb).is_ok(),
        SockKind::Stream(s) => s.set_nonblocking(nb).is_ok(),
    };
    if ok {
        slot.nonblocking = nb;
        0
    } else {
        -1
    }
}

/// Wait until `events` (WAIT_READ/WAIT_WRITE) are ready, or timeout.
/// Returns bitmask of ready events, 0 on timeout, -1 on error.
///
/// # Safety
/// Valid socket handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_wait(sock: i64, events: i64, timeout_ms: i64) -> i64 {
    if sock < 0 || events == 0 {
        return -1;
    }
    #[cfg(unix)]
    {
        let fd = {
            let g = lock();
            let Some(Some(slot)) = g.get(sock as usize) else {
                return -1;
            };
            raw_fd_of(&slot.kind)
        };
        let mut pfd = libc::pollfd {
            fd,
            events: 0,
            revents: 0,
        };
        if events & WAIT_READ != 0 {
            pfd.events |= libc::POLLIN;
        }
        if events & WAIT_WRITE != 0 {
            pfd.events |= libc::POLLOUT;
        }
        let timeout_arg = if timeout_ms < 0 {
            -1
        } else {
            timeout_ms.min(i32::MAX as i64) as i32
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_arg) };
        if rc < 0 {
            return -1;
        }
        if rc == 0 {
            return 0;
        }
        let mut out = 0i64;
        if pfd.revents & libc::POLLIN != 0 {
            out |= WAIT_READ;
        }
        if pfd.revents & libc::POLLOUT != 0 {
            out |= WAIT_WRITE;
        }
        // Errors report as readable so callers attempt read and see EOF/err.
        if pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            out |= WAIT_READ;
        }
        out
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Networking::WinSock::{
                POLLERR, POLLHUP, POLLRDNORM, POLLWRNORM, WSAPOLLFD, WSAPoll,
            };

            let fd = {
                let g = lock();
                let Some(Some(slot)) = g.get(sock as usize) else {
                    return -1;
                };
                raw_socket_of(&slot.kind)
            };
            let mut pfd = WSAPOLLFD {
                fd,
                events: 0,
                revents: 0,
            };
            if events & WAIT_READ != 0 {
                pfd.events |= POLLRDNORM;
            }
            if events & WAIT_WRITE != 0 {
                pfd.events |= POLLWRNORM;
            }
            let timeout_arg = if timeout_ms < 0 {
                -1
            } else {
                timeout_ms.min(i32::MAX as i64) as i32
            };
            let rc = unsafe { WSAPoll(&mut pfd, 1, timeout_arg) };
            if rc < 0 {
                return -1;
            }
            if rc == 0 {
                return 0;
            }
            let mut out = 0i64;
            if pfd.revents & POLLRDNORM != 0 {
                out |= WAIT_READ;
            }
            if pfd.revents & POLLWRNORM != 0 {
                out |= WAIT_WRITE;
            }
            if pfd.revents & (POLLERR | POLLHUP) != 0 {
                out |= WAIT_READ;
            }
            out
        }
        #[cfg(not(windows))]
        {
            let _ = (sock, events, timeout_ms);
            -1
        }
    }
}

/// Like [`ar_rt_tcp_wait`], then wakes `waker_id` if ready (see waker_runtime).
///
/// # Safety
/// Valid sock + waker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_wait_wake(
    sock: i64,
    events: i64,
    timeout_ms: i64,
    waker_id: i64,
) -> i64 {
    let rc = unsafe { ar_rt_tcp_wait(sock, events, timeout_ms) };
    if rc > 0 && waker_id >= 0 {
        unsafe {
            crate::waker_runtime::ar_rt_waker_wake(waker_id);
        }
    }
    rc
}

/// Read using io_uring when backend prefers it; else falls back to [`ar_rt_tcp_read`].
///
/// # Safety
/// Same as read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_read_async(sock: i64, buf: *mut u8, len: i64) -> i64 {
    #[cfg(target_os = "linux")]
    {
        if unsafe { crate::reactor_runtime::ar_rt_reactor_backend() }
            == crate::reactor_runtime::BACKEND_IO_URING
            && let Some(n) = unsafe { read_io_uring(sock, buf, len) }
        {
            return n;
        }
    }
    unsafe { ar_rt_tcp_read(sock, buf, len) }
}

/// Write using io_uring when available; else [`ar_rt_tcp_write`].
///
/// # Safety
/// Same as write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_tcp_write_async(sock: i64, buf: *const u8, len: i64) -> i64 {
    #[cfg(target_os = "linux")]
    {
        if unsafe { crate::reactor_runtime::ar_rt_reactor_backend() }
            == crate::reactor_runtime::BACKEND_IO_URING
            && let Some(n) = unsafe { write_io_uring(sock, buf, len) }
        {
            return n;
        }
    }
    unsafe { ar_rt_tcp_write(sock, buf, len) }
}

#[cfg(target_os = "linux")]
unsafe fn read_io_uring(sock: i64, buf: *mut u8, len: i64) -> Option<i64> {
    use io_uring::{IoUring, opcode, types};
    if sock < 0 || buf.is_null() || len <= 0 {
        return Some(-1);
    }
    let fd = {
        let g = lock();
        let slot = g.get(sock as usize).and_then(|s| s.as_ref())?;
        raw_fd_of(&slot.kind)
    };
    let mut ring = IoUring::new(IO_URING_DEFAULT_ENTRIES).ok()?;
    let entry = opcode::Read::new(types::Fd(fd), buf, len as u32)
        .build()
        .user_data(IO_URING_USER_DATA_READ);
    unsafe {
        ring.submission().push(&entry).ok()?;
    }
    ring.submit_and_wait(1).ok()?;
    let cqe = ring.completion().next()?;
    let res = cqe.result();
    if res < 0 {
        // Fall back to std read on transient errors.
        return None;
    }
    Some(res as i64)
}

#[cfg(target_os = "linux")]
unsafe fn write_io_uring(sock: i64, buf: *const u8, len: i64) -> Option<i64> {
    use io_uring::{IoUring, opcode, types};
    if sock < 0 || buf.is_null() || len < 0 {
        return Some(-1);
    }
    if len == 0 {
        return Some(0);
    }
    let fd = {
        let g = lock();
        let slot = g.get(sock as usize).and_then(|s| s.as_ref())?;
        raw_fd_of(&slot.kind)
    };
    let mut ring = IoUring::new(IO_URING_DEFAULT_ENTRIES).ok()?;
    let entry = opcode::Write::new(types::Fd(fd), buf, len as u32)
        .build()
        .user_data(IO_URING_USER_DATA_WRITE);
    unsafe {
        ring.submission().push(&entry).ok()?;
    }
    ring.submit_and_wait(1).ok()?;
    let cqe = ring.completion().next()?;
    let res = cqe.result();
    if res < 0 {
        return None;
    }
    Some(res as i64)
}

static UDP_SOCKS: Mutex<Vec<Option<UdpSocket>>> = Mutex::new(Vec::new());

fn udp_lock() -> std::sync::MutexGuard<'static, Vec<Option<UdpSocket>>> {
    UDP_SOCKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Binds a UDP socket on `host:port`.
///
/// # Safety
/// `host_ptr` and `host_len` must define a valid memory range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_bind(host_ptr: *const u8, host_len: usize, port: i64) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if host_ptr.is_null() || !(0..=65535).contains(&port) {
            return -1;
        }
        let host = std::str::from_utf8(unsafe { std::slice::from_raw_parts(host_ptr, host_len) }).ok()?;
        let addr = format!("{host}:{port}");
        let socket = UdpSocket::bind(&addr).ok()?;
        let mut g = udp_lock();
        if let Some(idx) = g.iter().position(|s| s.is_none()) {
            g[idx] = Some(socket);
            idx as i64
        } else {
            let id = g.len() as i64;
            g.push(Some(socket));
            id
        }
    }))
    .unwrap_or(-1)
}

/// Sends data via UDP socket to `host:port`.
///
/// # Safety
/// Pointers must be valid memory ranges.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_send_to(
    id: i64,
    buf: *const u8,
    len: usize,
    host_ptr: *const u8,
    host_len: usize,
    port: i64,
) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if buf.is_null() || host_ptr.is_null() || !(0..=65535).contains(&port) {
            return -1;
        }
        let host = std::str::from_utf8(unsafe { std::slice::from_raw_parts(host_ptr, host_len) }).ok()?;
        let target = format!("{host}:{port}");
        let payload = unsafe { std::slice::from_raw_parts(buf, len) };
        let g = udp_lock();
        let socket = g.get(id as usize)?.as_ref()?;
        socket.send_to(payload, &target).map(|sent| sent as i64).unwrap_or(-1)
    }))
    .unwrap_or(-1)
}

/// Receives data from UDP socket into `buf`.
///
/// # Safety
/// `buf` must be writable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_recv_from(
    id: i64,
    buf: *mut u8,
    len: usize,
    port_out: *mut i64,
) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if buf.is_null() || len == 0 {
            return -1;
        }
        let dest = unsafe { std::slice::from_raw_parts_mut(buf, len) };
        let g = udp_lock();
        let socket = g.get(id as usize)?.as_ref()?;
        match socket.recv_from(dest) {
            Ok((n, addr)) => {
                if !port_out.is_null() {
                    unsafe { *port_out = addr.port() as i64 };
                }
                n as i64
            }
            Err(_) => -1,
        }
    }))
    .unwrap_or(-1)
}

/// Closes a UDP socket handle.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_close(id: i64) {
    if id >= 0 {
        let mut g = udp_lock();
        if let Some(slot) = g.get_mut(id as usize) {
            *slot = None;
        }
    }
}

/// Resolves hostname via system DNS.
///
/// # Safety
/// `ip_buf` must be writable for `ip_cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_dns_resolve(
    name_ptr: *const u8,
    name_len: usize,
    ip_buf: *mut u8,
    ip_cap: usize,
) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if name_ptr.is_null() || ip_buf.is_null() || ip_cap == 0 {
            return -1;
        }
        let host = std::str::from_utf8(unsafe { std::slice::from_raw_parts(name_ptr, name_len) }).ok()?;
        use std::net::ToSocketAddrs;
        let query = format!("{host}:0");
        if let Ok(mut addrs) = query.to_socket_addrs() {
            if let Some(addr) = addrs.next() {
                let ip_str = addr.ip().to_string();
                let ip_bytes = ip_str.as_bytes();
                if ip_bytes.len() <= ip_cap {
                    unsafe {
                        std::ptr::copy_nonoverlapping(ip_bytes.as_ptr(), ip_buf, ip_bytes.len());
                    }
                    return ip_bytes.len() as i64;
                }
            }
        }
        -1
    }))
    .unwrap_or(-1)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn connected_pair() -> (i64, i64, i64) {
        // SAFETY: handles are created here, checked before use and transferred
        // to the caller for cleanup; these calls do not borrow raw buffers.
        unsafe {
            let listener = ar_rt_tcp_listen(0);
            assert!(
                listener >= 0,
                "loopback listener must be available for socket tests"
            );
            let port = {
                let guard = lock();
                let SockKind::Listener(socket) = &guard[listener as usize].as_ref().unwrap().kind
                else {
                    panic!("expected listener");
                };
                socket.local_addr().unwrap().port()
            };
            let client = ar_rt_tcp_connect(i64::from(port));
            assert!(client >= 0, "connect to the allocated loopback port");
            let server = ar_rt_tcp_accept(listener);
            assert!(server >= 0, "accept the loopback client");
            (listener, client, server)
        }
    }

    #[test]
    fn listen_connect_write_read() {
        unsafe {
            let (lis, client, server) = connected_pair();
            let msg = b"hi";
            assert_eq!(ar_rt_tcp_write(client, msg.as_ptr(), 2), 2);
            let mut buf = [0u8; 8];
            assert_eq!(ar_rt_tcp_read(server, buf.as_mut_ptr(), 8), 2);
            assert_eq!(&buf[..2], b"hi");
            ar_rt_tcp_close(client);
            ar_rt_tcp_close(server);
            ar_rt_tcp_close(lis);
        }
    }

    #[test]
    fn nonblocking_wait_read() {
        unsafe {
            let (lis, client, server) = connected_pair();
            assert_eq!(ar_rt_tcp_set_nonblocking(server, 1), 0);
            // No data yet — wait should timeout.
            let t0 = ar_rt_tcp_wait(server, WAIT_READ, 10);
            assert_eq!(t0, 0);
            let msg = b"xy";
            assert_eq!(ar_rt_tcp_write(client, msg.as_ptr(), 2), 2);
            let ready = ar_rt_tcp_wait(server, WAIT_READ, 200);
            assert!(ready & WAIT_READ != 0);
            let mut buf = [0u8; 4];
            assert_eq!(ar_rt_tcp_read(server, buf.as_mut_ptr(), 4), 2);
            ar_rt_tcp_close(client);
            ar_rt_tcp_close(server);
            ar_rt_tcp_close(lis);
        }
    }

    #[test]
    fn async_read_write_path() {
        unsafe {
            let (lis, client, server) = connected_pair();
            let msg = b"ok";
            assert!(ar_rt_tcp_write_async(client, msg.as_ptr(), 2) >= 2);
            let mut buf = [0u8; 4];
            assert!(ar_rt_tcp_read_async(server, buf.as_mut_ptr(), 4) >= 2);
            ar_rt_tcp_close(client);
            ar_rt_tcp_close(server);
            ar_rt_tcp_close(lis);
        }
    }
}
