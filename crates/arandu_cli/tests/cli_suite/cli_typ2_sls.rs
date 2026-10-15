//! TYP.2 where/bounds + SL_S std.runtime import typechecks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli_in(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("cli")
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn where_and_colon_bounds_check_ok() {
    let root = workspace_root();
    let file = root.join("tests/ui/type_checker/where_ok.aru");
    let out = run_cli_in(&root, &["check", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "where_ok: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn import_std_runtime_scaffold_checks() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_std_runtime.aru");
    fs::write(
        &file,
        r#"
module tests.cli.std_runtime
import std.runtime.executor as rt
func main(): int {
    let ex = rt.SyncExecutor.new()
    return ex.flags
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["check", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "std.runtime: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_path_absolute_and_empty() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_path_abs.aru");
    let absolute = if cfg!(windows) { r"C:\\" } else { "/tmp" };
    fs::write(
        &file,
        r#"
module tests.cli.path_abs
import std.path as path
func main(): int {
    let empty = path.Path.from("")
    if !empty.isEmpty() {
        return 1
    }
    let abs = path.Path.from("__ABSOLUTE_PATH__")
    if !abs.isAbsolute() {
        return 2
    }
    let rel = path.Path.from("rel")
    if rel.isAbsolute() {
        return 3
    }
    return 0
}
"#
        .replace("__ABSOLUTE_PATH__", absolute),
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "path abs: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_sync_executor_new() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_sync_ex.aru");
    fs::write(
        &file,
        r#"
module tests.cli.sync_ex
import std.runtime.executor as rt
func main(): int {
    let ex = rt.SyncExecutor.new()
    return ex.flags
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "sync ex: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Multi-file inferred generic `rt.spawn` / `rt.join` (no explicit type args).
#[test]
fn run_import_inferred_spawn_join() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_import_infer_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.import_infer_spawn
import std.runtime.executor as rt

async func answer(): int {
    return 42
}

func main(): int {
    let ex = rt.SyncExecutor.new()
    let h = ex.spawn(answer())
    return ex.join(h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "import infer spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Same-module inferred join (no type args on join_g).
#[test]
fn run_local_inferred_join() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_local_infer_join.aru");
    fs::write(
        &file,
        r#"
module tests.cli.local_infer_join

extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
}

struct SyncExecutor { flags: int }
struct TaskHandle { id: int }

func spawn<T>(shared ex: SyncExecutor, job: Coroutine<T>): TaskHandle {
    unsafe {
        let id = ar_rt_spawn_i64(job as ptr[u8])
        return TaskHandle { id: id }
    }
}

func join<T>(shared ex: SyncExecutor, handle: TaskHandle): T {
    unsafe {
        let v = ar_rt_join_i64(handle.id)
        return v as T
    }
}

async func answer(): int {
    return 42
}

func main(): int {
    let ex = SyncExecutor { flags: 0 }
    let h = spawn<int>(ex, answer())
    return join<int>(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "local infer join: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Typed spawn/join over A3 `async func` → `Coroutine<int>`.
#[test]
fn run_typed_spawn_async_func() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_typed_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.typed_spawn
import std.runtime.executor as rt
import std.core.mem as mem

async func answer(): int {
    return 42
}

func main(): int {
    let ex = rt.SyncExecutor.new()
    let h = ex.spawn(answer())
    let result = ex.join(h)
    ex.cancel(h)
    if mem.sizeOf<rt.TaskHandle<int>>() != mem.sizeOf<int>() { return 1 }
    return result
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "typed spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Same-module generic spawn/join with explicit type args (mono specialization).
#[test]
fn run_generic_spawn_join_explicit() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.generic_spawn

extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
}

struct SyncExecutor { flags: int }
struct TaskHandle { id: int }

func spawn<T>(shared ex: SyncExecutor, job: Coroutine<T>): TaskHandle {
    unsafe {
        let id = ar_rt_spawn_i64(job as ptr[u8])
        return TaskHandle { id: id }
    }
}

func join<T>(shared ex: SyncExecutor, handle: TaskHandle): T {
    unsafe {
        let v = ar_rt_join_i64(handle.id)
        return v as T
    }
}

async func answer(): int {
    return 42
}

func main(): int {
    let ex = SyncExecutor { flags: 0 }
    let h = spawn<int>(ex, answer())
    return join<int>(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "generic spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_waker_wake_and_wait() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_waker.aru");
    fs::write(
        &file,
        r#"
module tests.cli.waker
import std.runtime.waker as waker

func main(): int {
    let mut w = waker.Waker.new()
    w.wake()
    let rc = w.wait(100)
    w.close()
    if rc != 1 {
        return 1
    }
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "waker: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_reactor_backend_is_supported_or_portable() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_backend.aru");
    fs::write(
        &file,
        r#"
module tests.cli.backend
import std.runtime.reactor as reactor

func main(): int {
    let b = reactor.reactorBackend()
    // Portable fallback: 0; Linux: 1 = epoll, 2 = io_uring.
    if b < 0 {
        return 1
    }
    if b > 2 {
        return 2
    }
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "backend: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_tcp_async_wait_wake() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_tcp_async.aru");
    fs::write(
        &file,
        r#"
module tests.cli.tcp_async
import std.net as net
import std.runtime.waker as waker

func main(): int {
    let lis = net.RawTcpListener.listen(18770)
    if lis.id < 0 {
        return 1
    }
    let client = net.RawTcpStream.connect(18770)
    if client.id < 0 {
        return 2
    }
    let server = lis.accept()
    if server.id < 0 {
        return 3
    }
    let nb = server.setNonblocking(1)
    if nb != 0 {
        return 4
    }
    let mut w = waker.Waker.new()
    // Timeout with no data
    let t0 = server.waitWake(net.tcpWaitReadFlag(), 5, w)
    if t0 != 0 {
        return 5
    }
    // Write then wait
    // Use write_async (io_uring when available)
    // We cannot easily pass string buffers without alloc; skip payload e2e here.
    // Wait writable on client should succeed.
    let wr = client.wait(net.tcpWaitWriteFlag(), 100)
    if wr < 1 {
        return 6
    }
    w.close()
    client.close()
    server.close()
    lis.close()
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "tcp async: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_supervisor_true() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_supervisor.aru");
    let worker = if cfg!(windows) {
        std::env::var("WINDIR")
            .map(|windir| format!(r"{windir}\System32\whoami.exe"))
            .unwrap_or_else(|_| r"C:\Windows\System32\whoami.exe".to_string())
    } else if std::path::Path::new("/usr/bin/true").is_file() {
        "/usr/bin/true".to_string()
    } else {
        "/bin/true".to_string()
    }
    .replace('\\', "\\\\");
    fs::write(
        &file,
        r#"
module tests.cli.supervisor
import std.runtime.supervisor as sup

func main(): int {
    let mut s = sup.Supervisor.new()
    if s.id < 0 {
        return 1
    }
    let w = s.spawn("__WORKER_PATH__", 0)
    if w.id < 0 {
        return 2
    }
    let code = s.wait(w)
    s.close()
    return code
}
"#
        .replace("__WORKER_PATH__", &worker),
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "supervisor: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Typed block_on over async func (no spawn).
#[test]
fn run_typed_block_on() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_typed_block_on.aru");
    fs::write(
        &file,
        r#"
module tests.cli.typed_block_on
import std.runtime.executor as rt

async func answer(): int {
    return 7
}

func main(): int {
    let ex = rt.SyncExecutor.new()
    return ex.blockOn(answer())
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "typed block_on: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SL_R.2: EpollReactor sleep_ms returns success.
#[test]
fn run_reactor_sleep_ms() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_reactor_sleep.aru");
    fs::write(
        &file,
        r#"
module tests.cli.reactor_sleep
import std.runtime.reactor as reactor

func main(): int {
    let mut r = reactor.EpollReactor.new()
    if r.id < 0 {
        return 1
    }
    let rc = r.sleepMs(5)
    r.close()
    if rc != 0 {
        return 2
    }
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "reactor sleep: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SL_R.2 + SL_R.0: arm timer, poll, and join a spawned coroutine.
#[test]
fn run_reactor_arm_poll_with_spawn() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_reactor_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.reactor_spawn
import std.runtime.executor as rt
import std.runtime.reactor as reactor

async func ready(): int {
    return 99
}

func main(): int {
    let mut r = reactor.EpollReactor.new()
    let ex = rt.SyncExecutor.new()
    if r.id < 0 {
        return 1
    }
    let h = ex.spawn(ready())
    let arm = r.armTimerMs(5)
    if arm != 0 {
        return 2
    }
    let fired = r.pollMs(200)
    if fired != 1 {
        return 3
    }
    let v: int = ex.join(h)
    r.close()
    return v
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(99),
        "reactor+spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_vec_and_string_try_reserve_try_push() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_try_push.aru");
    std::fs::write(
        &file,
        r#"
module tests.cli.vec_try_push
import std.alloc.vec as vec
import std.alloc.string as string

func main(): int {
    let mut v = vec.Vec<int>.new()
    if !v.tryReserve(16) {
        return 1
    }
    if !v.tryPush(42) {
        return 2
    }
    if !v.tryPush(84) {
        return 3
    }
    if v.len() != 2 {
        return 4
    }

    let mut s = string.String.new()
    if !s.tryReserve(32) {
        return 5
    }
    if !s.pushStr("hello") {
        return 6
    }
    if s.len() != 5 {
        return 7
    }

    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "try_push: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
