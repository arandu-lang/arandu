//! SL_R — host TCP sockets (blocking + nonblocking + reactor wait + io_uring I/O).
//!
//! - Blocking: [`ar_rt_tcp_read`] / [`ar_rt_tcp_write`] (default).
//! - Async surface: [`ar_rt_tcp_set_nonblocking`] + [`ar_rt_tcp_wait`] (poll/epoll)
//!   then read/write; optional waker via [`ar_rt_tcp_wait_wake`].
//! - SL_R.3: when reactor backend is io_uring, read/write use io_uring submit.

mod tcp;
mod udp;

#[cfg(unix)]
pub use tcp::get_socket_fd;
#[cfg(windows)]
pub use tcp::get_socket_raw;
#[cfg(windows)]
pub(crate) use tcp::{SocketLease, lease_socket};
pub use tcp::{
    WAIT_READ, WAIT_WRITE, ar_rt_tcp_accept, ar_rt_tcp_close, ar_rt_tcp_connect, ar_rt_tcp_listen,
    ar_rt_tcp_read, ar_rt_tcp_read_async, ar_rt_tcp_set_nonblocking, ar_rt_tcp_wait,
    ar_rt_tcp_wait_wake, ar_rt_tcp_write, ar_rt_tcp_write_async,
};
pub use udp::{
    ar_net_dns_resolve, ar_net_udp_bind, ar_net_udp_close, ar_net_udp_recv_from, ar_net_udp_send_to,
};

#[cfg(test)]
pub(crate) mod tests {
    use std::net::UdpSocket;
    use std::time::Duration;

    use super::tcp::{SockKind, get_slot};
    use super::udp::get_udp_socket;
    use super::*;

    pub(crate) fn listener_port(listener: i64) -> u16 {
        let slot = get_slot(listener).expect("listener handle stays registered");
        let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        let SockKind::Listener(socket) = &slot.kind else {
            panic!("expected listener");
        };
        socket.local_addr().expect("listener address").port()
    }

    pub(crate) fn connected_pair() -> (i64, i64, i64) {
        // SAFETY: handles are created here, checked before use and transferred
        // to the caller for cleanup; these calls do not borrow raw buffers.
        unsafe {
            let listener = ar_rt_tcp_listen(0);
            assert!(
                listener >= 0,
                "loopback listener must be available for socket tests"
            );
            let port = listener_port(listener);
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

    #[test]
    fn udp_registry_supports_send_and_receive() {
        let host = b"127.0.0.1";
        // SAFETY: host and payload buffers remain alive and valid for each call.
        unsafe {
            let id = ar_net_udp_bind(host.as_ptr(), host.len(), 0);
            assert!(id >= 0, "loopback UDP bind must succeed");
            let socket = get_udp_socket(id).expect("bound UDP handle remains registered");
            let address = socket
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .local_addr()
                .expect("bound UDP socket has a local address");

            let sender = UdpSocket::bind("127.0.0.1:0").expect("bind UDP test sender");
            sender
                .send_to(b"in", address)
                .expect("send datagram to runtime socket");
            let mut received = [0u8; 8];
            let mut peer_port = 0;
            assert_eq!(
                ar_net_udp_recv_from(id, received.as_mut_ptr(), received.len(), &mut peer_port),
                2
            );
            assert_eq!(&received[..2], b"in");
            assert_eq!(peer_port as u16, sender.local_addr().unwrap().port());

            let target = b"127.0.0.1";
            assert_eq!(
                ar_net_udp_send_to(
                    id,
                    b"out".as_ptr(),
                    3,
                    target.as_ptr(),
                    target.len(),
                    i32::from(sender.local_addr().unwrap().port()),
                ),
                3
            );
            let mut sent = [0u8; 8];
            sender
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("set bounded UDP receive timeout");
            let (count, _) = sender
                .recv_from(&mut sent)
                .expect("receive runtime datagram");
            assert_eq!(&sent[..count], b"out");
            ar_net_udp_close(id);
        }
    }

    #[test]
    fn closed_tcp_handle_does_not_alias_a_reused_slot() {
        unsafe {
            let first = ar_rt_tcp_listen(0);
            assert!(first >= 0);
            ar_rt_tcp_close(first);
            let second = ar_rt_tcp_listen(0);
            assert!(second >= 0);
            assert_ne!(first, second);
            let mut buf = [0u8; 1];
            assert_eq!(ar_rt_tcp_read(first, buf.as_mut_ptr(), 1), -1);
            ar_rt_tcp_close(second);
        }
    }

    #[test]
    fn blocked_accept_does_not_stall_another_socket() {
        unsafe {
            let blocked = ar_rt_tcp_listen(0);
            assert!(blocked >= 0);
            let port = {
                let slot = get_slot(blocked).expect("listener remains registered");
                let slot = slot.lock().unwrap_or_else(|error| error.into_inner());
                let SockKind::Listener(socket) = &slot.kind else {
                    panic!("expected listener");
                };
                socket.local_addr().expect("listener address").port()
            };
            let acceptor = std::thread::spawn(move || ar_rt_tcp_accept(blocked));
            std::thread::sleep(Duration::from_millis(50));
            let started = std::time::Instant::now();
            let other = ar_rt_tcp_listen(0);
            assert!(other >= 0);
            ar_rt_tcp_close(other);
            let again = ar_rt_tcp_listen(0);
            assert!(again >= 0);
            assert!(started.elapsed() < Duration::from_millis(500));
            let client = ar_rt_tcp_connect(i64::from(port));
            assert!(client >= 0);
            let accepted = acceptor.join().expect("accept thread");
            assert!(accepted >= 0);
            ar_rt_tcp_close(client);
            ar_rt_tcp_close(accepted);
            ar_rt_tcp_close(blocked);
            ar_rt_tcp_close(again);
        }
    }

    #[test]
    fn udp_rebind_localhost_ipv6_and_many_sockets() {
        let loopback = b"127.0.0.1";
        unsafe {
            let v6 = b"::1";
            let v6_id = ar_net_udp_bind(v6.as_ptr(), v6.len(), 0);
            assert!(v6_id >= 0, "IPv6 loopback UDP bind");
            ar_net_udp_close(v6_id);

            let host = b"localhost";
            let local = ar_net_udp_bind(host.as_ptr(), host.len(), 0);
            assert!(local >= 0, "localhost UDP bind");
            ar_net_udp_close(local);

            let mut ids = Vec::with_capacity(70);
            for _ in 0..70 {
                let id = ar_net_udp_bind(loopback.as_ptr(), loopback.len(), 0);
                assert!(id >= 0, "bind 70 loopback UDP sockets");
                ids.push(id);
            }
            let mut unique = ids.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), ids.len());
            for id in ids {
                ar_net_udp_close(id);
            }

            let first = ar_net_udp_bind(loopback.as_ptr(), loopback.len(), 0);
            assert!(first >= 0);
            ar_net_udp_close(first);
            let second = ar_net_udp_bind(loopback.as_ptr(), loopback.len(), 0);
            assert!(second >= 0);
            assert_ne!(first, second);
            assert_eq!(
                ar_net_udp_send_to(
                    first,
                    b"x".as_ptr(),
                    1,
                    loopback.as_ptr(),
                    loopback.len(),
                    9
                ),
                -1
            );
            ar_net_udp_close(second);
        }
    }
}
