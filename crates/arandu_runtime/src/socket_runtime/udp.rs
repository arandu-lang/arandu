use std::net::UdpSocket;
use std::sync::{Arc, Mutex};

pub(super) type SharedUdpSocket = Arc<Mutex<UdpSocket>>;

/// Low 16 bits are the slot. The next 15 bits are the generation. Generation 0
/// keeps the first handle equal to the index. The sign bit stays clear.
struct UdpEntry {
    generation: u16,
    socket: Option<SharedUdpSocket>,
}

const UDP_INDEX_MASK: u32 = 0xFFFF;
const UDP_GENERATION_MASK: u16 = 0x7FFF;
const UDP_MAX_SLOTS: usize = 65536;

static UDP_SOCKS: Mutex<Vec<UdpEntry>> = Mutex::new(Vec::new());

fn udp_lock() -> std::sync::MutexGuard<'static, Vec<UdpEntry>> {
    UDP_SOCKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn pack_udp_handle(index: usize, generation: u16) -> i32 {
    let packed = (u32::from(generation) << 16) | (index as u32 & UDP_INDEX_MASK);
    packed as i32
}

fn unpack_udp_handle(id: i32) -> Option<(usize, u16)> {
    if id < 0 {
        return None;
    }
    let raw = id as u32;
    Some((
        (raw & UDP_INDEX_MASK) as usize,
        ((raw >> 16) as u16) & UDP_GENERATION_MASK,
    ))
}

fn bump_udp_generation(generation: u16) -> u16 {
    let next = generation.wrapping_add(1) & UDP_GENERATION_MASK;
    if next == 0 { 1 } else { next }
}

fn format_udp_endpoint(host: &str, port: i32) -> String {
    if host.starts_with('[') || !host.contains(':') {
        format!("{host}:{port}")
    } else {
        format!("[{host}]:{port}")
    }
}

pub(super) fn get_udp_socket(id: i32) -> Option<SharedUdpSocket> {
    let (index, generation) = unpack_udp_handle(id)?;
    let table = udp_lock();
    let entry = table.get(index)?;
    if entry.generation != generation {
        return None;
    }
    entry.socket.clone()
}

fn publish_udp(socket: SharedUdpSocket) -> Option<i32> {
    let mut table = udp_lock();
    if let Some(index) = table.iter().position(|entry| entry.socket.is_none()) {
        let generation = table[index].generation;
        table[index].socket = Some(socket);
        return Some(pack_udp_handle(index, generation));
    }
    if table.len() >= UDP_MAX_SLOTS {
        return None;
    }
    let index = table.len();
    table.push(UdpEntry {
        generation: 0,
        socket: Some(socket),
    });
    Some(pack_udp_handle(index, 0))
}

/// Binds a UDP socket on `host:port`.
///
/// # Safety
/// `host_ptr` and `host_len` must define a valid memory range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_bind(host_ptr: *const u8, host_len: usize, port: i32) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<i32> {
        if host_ptr.is_null() || !(0..=65535).contains(&port) {
            return None;
        }
        let host =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(host_ptr, host_len) }).ok()?;
        let addr = format_udp_endpoint(host, port);
        let socket = UdpSocket::bind(&addr).ok()?;
        publish_udp(Arc::new(Mutex::new(socket)))
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Sends data via UDP socket to `host:port`.
///
/// # Safety
/// Pointers must be valid memory ranges.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_send_to(
    id: i32,
    buf: *const u8,
    len: usize,
    host_ptr: *const u8,
    host_len: usize,
    port: i32,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<i32> {
        if buf.is_null() || host_ptr.is_null() || !(0..=65535).contains(&port) {
            return None;
        }
        let host =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(host_ptr, host_len) }).ok()?;
        let target = format_udp_endpoint(host, port);
        let payload = unsafe { std::slice::from_raw_parts(buf, len) };
        let socket = get_udp_socket(id)?;
        let socket = socket.lock().unwrap_or_else(|e| e.into_inner());
        Some(
            socket
                .send_to(payload, &target)
                .map_or(-1, |sent| sent as i32),
        )
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Receives data from UDP socket into `buf`.
///
/// # Safety
/// `buf` must be writable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_recv_from(
    id: i32,
    buf: *mut u8,
    len: usize,
    port_out: *mut i32,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<i32> {
        if buf.is_null() || len == 0 {
            return None;
        }
        let dest = unsafe { std::slice::from_raw_parts_mut(buf, len) };
        let socket = get_udp_socket(id)?;
        let socket = socket.lock().unwrap_or_else(|e| e.into_inner());
        match socket.recv_from(dest) {
            Ok((n, addr)) => {
                if !port_out.is_null() {
                    unsafe { *port_out = addr.port() as i32 };
                }
                Some(n as i32)
            }
            Err(_) => Some(-1),
        }
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}

/// Closes a UDP socket handle.
///
/// # Safety
/// Always safe to call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_net_udp_close(id: i32) {
    let Some((index, generation)) = unpack_udp_handle(id) else {
        return;
    };
    let removed = {
        let mut table = udp_lock();
        let Some(entry) = table.get_mut(index) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        let removed = entry.socket.take();
        if removed.is_some() {
            entry.generation = bump_udp_generation(entry.generation);
        }
        removed
    };
    drop(removed);
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
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<i32> {
        if name_ptr.is_null() || ip_buf.is_null() || ip_cap == 0 {
            return None;
        }
        let host =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(name_ptr, name_len) }).ok()?;
        use std::net::ToSocketAddrs;
        let query = format!("{host}:0");
        if let Ok(mut addrs) = query.to_socket_addrs()
            && let Some(addr) = addrs.next()
        {
            let ip_str = addr.ip().to_string();
            let ip_bytes = ip_str.as_bytes();
            if ip_bytes.len() <= ip_cap {
                unsafe {
                    std::ptr::copy_nonoverlapping(ip_bytes.as_ptr(), ip_buf, ip_bytes.len());
                }
                return Some(ip_bytes.len() as i32);
            }
        }
        None
    }))
    .ok()
    .flatten()
    .unwrap_or(-1)
}
