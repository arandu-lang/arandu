//! UDP socket lifecycle and DNS resolution exercised through the public CLI.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::fs;

#[test]
fn udp_socket_bind_send_recv_and_dns_in_jit() {
    let root = common::temp_dir("arandu-udp-contract").expect("temp dir");
    let source = r#"import std.net as net
import std.core.slice as slice
import std.core.intrinsics as intrinsics
import std.alloc.vec as vec

public func main(): int {
    // 1. DNS resolution test
    match net.resolveHost("localhost") {
        Result.Ok(addr) => {
            if addr.port != 0 { return 1 }
        }
        Result.Err(_) => { return 2 }
    }

    // 2. UDP socket bind and exchange
    let serverAddr = net.SocketAddr.new("127.0.0.1", 48291)
    let clientAddr = net.SocketAddr.new("127.0.0.1", 48292)

    match net.UdpSocket.bind(serverAddr) {
        Result.Ok(srv) => {
            let mut server = srv
            match net.UdpSocket.bind(clientAddr) {
                Result.Ok(cli) => {
                    let mut client = cli
                    let msg = "ping"
                    let msgBytes = intrinsics.strBytes(msg)
                    let sent = client.sendTo(msgBytes, serverAddr)
                    match sent {
                        Result.Ok(n) => {
                            if n != 4 { return 3 }
                        }
                        Result.Err(_) => { return 4 }
                    }

                    let mut recvBuf = vec.Vec<u8>.withCapacity(16)
                    let mut i: usize = 0
                    while i < 16 {
                        recvBuf.push(0 as u8)
                        i = i + 1
                    }
                    let packet = server.recvPacket(recvBuf.asSlice())
                    match packet {
                        Result.Ok(pkt) => {
                            if pkt.bytes_received != (4 as usize) { return 5 }
                            if pkt.peer_port != 48292 { return 6 }
                        }
                        Result.Err(_) => { return 7 }
                    }

                    client.close()
                    server.close()
                }
                Result.Err(_) => {
                    server.close()
                    return 8
                }
            }
        }
        Result.Err(_) => { return 9 }
    }

    return 0
}
"#;

    let path = root.join("udp.aru");
    fs::write(&path, source).expect("write udp test");

    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .expect("run CLI");

    assert!(
        output.status.success(),
        "JIT UDP failed (code {:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let emitted = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .expect("emit-c");
    assert!(
        emitted.status.success(),
        "emit-c: {}",
        String::from_utf8_lossy(&emitted.stderr)
    );

    if std::process::Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|v| v.status.success())
    {
        let c_path = root.join("udp.c");
        fs::write(&c_path, emitted.stdout).unwrap();
        let bin_path = root.join("udp");
        let compile = std::process::Command::new("clang")
            .args([
                "-O2",
                "-fwrapv",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
            ])
            .arg(&c_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "clang compile: {}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = std::process::Command::new(&bin_path).output().unwrap();
        assert!(
            run.status.success(),
            "clang run: code={:?} stderr={}",
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    fs::remove_dir_all(root).unwrap();
}
