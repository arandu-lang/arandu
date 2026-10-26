use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

pub(super) struct SockSlot {
    pub(super) kind: SockKind,
    nonblocking: bool,
}

pub(super) enum SockKind {
    Listener(TcpListener),
    Stream(TcpStream),
}

pub(super) type SharedSockSlot = Arc<Mutex<SockSlot>>;

/// A published handle keeps its generation. Close bumps it, so a stale id cannot
/// address the socket that later occupies the same index. Generation 0 is the
/// first publication and makes the handle equal the index.
struct TcpEntry {
    generation: u32,
    slot: Option<SharedSockSlot>,
}

const TCP_INDEX_MASK: u64 = 0xFFFF_FFFF;
const TCP_GENERATION_MASK: u32 = 0x7FFF_FFFF;

static SOCKS: Mutex<Vec<TcpEntry>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<TcpEntry>> {
    SOCKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn pack_tcp_handle(index: usize, generation: u32) -> i64 {
    let packed = ((u64::from(generation) & u64::from(TCP_GENERATION_MASK)) << 32)
        | (index as u64 & TCP_INDEX_MASK);
    packed as i64
}

fn unpack_tcp_handle(sock: i64) -> Option<(usize, u32)> {
    if sock < 0 {
        return None;
    }
    let raw = sock as u64;
    let index = (raw & TCP_INDEX_MASK) as usize;
    let generation = ((raw >> 32) & u64::from(TCP_GENERATION_MASK)) as u32;
    Some((index, generation))
}

fn bump_tcp_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1) & TCP_GENERATION_MASK;
    if next == 0 { 1 } else { next }
}

pub(super) fn get_slot(sock: i64) -> Option<SharedSockSlot> {
    let (index, generation) = unpack_tcp_handle(sock)?;
    let table = lock();
    let entry = table.get(index)?;
    if entry.generation != generation {
        return None;
    }
    entry.slot.clone()
}

fn insert(kind: SockKind) -> i64 {
    let mut table = lock();
    let slot = Arc::new(Mutex::new(SockSlot {
        kind,
        nonblocking: false,
    }));
    if let Some(index) = table.iter().position(|entry| entry.slot.is_none()) {
        let generation = table[index].generation;
        table[index].slot = Some(slot);
        return pack_tcp_handle(index, generation);
    }
    if table.len() > u32::MAX as usize {
        return -1;
    }
    let index = table.len();
    table.push(TcpEntry {
        generation: 0,
        slot: Some(slot),
    });
    pack_tcp_handle(index, 0)
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
    let slot = get_slot(sock_id)?;
    let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    Some(raw_fd_of(&slot.kind))
}

/// Raw `SOCKET` of a live TCP handle. Constant for the slot's lifetime, so the
/// reactor captures it once at registration (without holding the slot lock
/// across a wait) and revalidates the generation-tagged id on every poll.
#[cfg(windows)]
pub fn get_socket_raw(sock_id: i64) -> Option<windows_sys::Win32::Networking::WinSock::SOCKET> {
    let slot = get_slot(sock_id)?;
    let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    Some(raw_socket_of(&slot.kind))
}

/// Keeps a live socket's OS handle open while held: `ar_rt_tcp_close` only
/// unpublishes the id, and `closesocket` runs when the last lease drops. The
/// reactor holds one across `WSAPoll`, so the kernel cannot hand the same
/// `SOCKET` value to a new socket mid-wait. It does not take the slot mutex,
/// so a blocking read/write on another thread never stalls the reactor.
#[cfg(windows)]
pub(crate) struct SocketLease {
    _keep_open: SharedSockSlot,
}

/// Leases `sock_id` if its generation is still current, else `None`.
#[cfg(windows)]
pub(crate) fn lease_socket(sock_id: i64) -> Option<SocketLease> {
    get_slot(sock_id).map(|slot| SocketLease { _keep_open: slot })
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
    let Some(slot) = get_slot(listener) else {
        return -1;
    };
    let cloned_listener = {
        let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
    let Some(slot) = get_slot(sock) else {
        return -1;
    };
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
    let Some(slot) = get_slot(sock) else {
        return -1;
    };
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
    let Some((index, generation)) = unpack_tcp_handle(sock) else {
        return;
    };
    let removed = {
        let mut table = lock();
        let Some(entry) = table.get_mut(index) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        let removed = entry.slot.take();
        if removed.is_some() {
            entry.generation = bump_tcp_generation(entry.generation);
        }
        removed
    };
    drop(removed);
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
    let Some(slot) = get_slot(sock) else {
        return -1;
    };
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
        let Some(slot) = get_slot(sock) else {
            return -1;
        };
        let fd = {
            let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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

            let Some(slot) = get_slot(sock) else {
                return -1;
            };
            let fd = {
                let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
    let slot = get_slot(sock)?;
    let fd = {
        let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
    let slot = get_slot(sock)?;
    let fd = {
        let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
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
