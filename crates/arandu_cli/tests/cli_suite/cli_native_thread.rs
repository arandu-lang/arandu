//! Source-to-native ownership and Send-bound contracts for std.thread.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::{
    fs,
    io::Read,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

#[test]
fn native_thread_moves_owned_argument_result_and_joins_on_drop() {
    run_native_contract(include_str!(
        "../../../../tests/corpus/native_thread_owned.aru"
    ));
}

#[test]
fn native_shared_arc_supports_simultaneous_readers_and_a_later_writer() {
    run_native_contract(include_str!(
        "../../../../tests/corpus/native_shared_arc.aru"
    ));
}

#[test]
fn arc_uniqueness_excludes_live_weak_and_strong_handles() {
    run_native_contract(include_str!("../../../../tests/corpus/arc_uniqueness.aru"));
}

#[test]
fn native_thread_park_timeout_token_saturation_and_cross_thread_wake() {
    run_native_contract(include_str!(
        "../../../../tests/corpus/native_thread_park_timeout.aru"
    ));
}

fn run_native_contract(contract: &str) {
    let root = common::temp_dir("arandu-native-thread").unwrap();
    let source = root.join("thread.aru");
    fs::write(&source, contract).unwrap();
    for (mode, optimized) in [("run", false), ("run", true), ("emit-c", false)] {
        let mut command = common::cli_command();
        command.arg(mode).arg(&source);
        if optimized {
            command.arg("--release");
        }
        let output = run_with_deadline(&mut command);
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "emit-c"
            && Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c = root.join("thread.c");
            fs::write(&c, &output.stdout).unwrap();
            let executable = root.join(if cfg!(windows) {
                "thread.exe"
            } else {
                "thread"
            });
            for optimization in ["-O0", "-O2"] {
                let mut compiler = Command::new("clang");
                compiler.args([optimization, "-fwrapv"]);
                if cfg!(unix) {
                    compiler.args([
                        "-pthread",
                        "-fsanitize=undefined",
                        "-fno-sanitize-recover=all",
                    ]);
                }
                let compiled = run_with_deadline(compiler.arg(&c).arg("-o").arg(&executable));
                assert!(
                    compiled.status.success(),
                    "{}",
                    String::from_utf8_lossy(&compiled.stderr)
                );
                let run = run_with_deadline(&mut Command::new(&executable));
                assert_eq!(
                    run.status.code(),
                    Some(0),
                    "{}",
                    String::from_utf8_lossy(&run.stderr)
                );
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

fn run_with_deadline(command: &mut Command) -> Output {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the child changes only its resource limit, without locks
        // or allocation between fork and exec. Expected aborts leave no core.
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // Drain both pipes while waiting: generated C can exceed pipe capacity.
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: output.join().unwrap(),
        stderr: errors.join().unwrap(),
    };
    assert!(
        !timed_out,
        "native thread contract timed out: {command:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn native_thread_rejects_borrowed_argument_at_send_boundary() {
    let root = common::temp_dir("arandu-native-thread-send").unwrap();
    let source = root.join("thread.aru");
    fs::write(
        &source,
        r#"
module borrowedThread
import std.thread as thread
struct Job {}
public func Job.run(self: own Job, argument: str): int { return 1 }
public func main(): int {
    let result = thread.spawnWith<str, int, Job>("borrowed", Job {})
    return 0
}


"#,
    )
    .unwrap();
    let output = common::cli_command()
        .arg("check")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "borrowed str crossed native-thread Send boundary"
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("Send"), "{diagnostics}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_arc_transfer_requires_both_send_and_sync_for_its_payload() {
    let root = common::temp_dir("arandu-arc-transfer").unwrap();
    let path = root.join("arc.aru");
    for (storage, capability, accepted) in [
        ("arc.Arc<int>", "Send", true),
        ("arc.Arc<int>", "Sync", true),
        ("arc.Arc<int>", "Copy", false),
        ("arc.ArcWeak<int>", "Send", true),
        ("arc.ArcWeak<int>", "Sync", true),
        ("arc.ArcWeak<int>", "Copy", false),
        ("arc.Arc<cell.UnsafeCell<int>>", "Send", false),
        ("arc.ArcWeak<cell.UnsafeCell<int>>", "Sync", false),
        ("arc.Arc<rwlock.RwLock<int>>", "Send", true),
        (
            "arc.Arc<rwlock.RwLock<cell.UnsafeCell<int>>>",
            "Send",
            false,
        ),
        ("arc.Arc<mutex.Mutex<cell.UnsafeCell<int>>>", "Send", true),
        ("arc.Arc<ptr[u8]>", "Send", false),
        ("arc.ArcWeak<ptr[u8]>", "Sync", false),
    ] {
        fs::write(&path, format!(
            "import std.alloc.arc as arc\nimport std.core.marker as marker\nimport std.core.cell as cell\nimport std.sync.rwlock as rwlock\nimport std.sync.mutex as mutex\nfunc require<T:marker.{capability}>(value:T):void{{}}\nfunc inspect(value:{storage}):void{{require(value)}}\nfunc main():int{{return 0}}"
        )).unwrap();
        let result = run_with_deadline(common::cli_command().arg("check").arg(&path));
        assert_eq!(
            result.status.success(),
            accepted,
            "{storage}:{capability}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        if !accepted {
            assert!(String::from_utf8_lossy(&result.stderr).contains("T025"));
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_arc_reference_counts_abort_before_overflow_or_reserved_weak_lock() {
    let root = common::temp_dir("arandu-arc-overflow").unwrap();
    let path = root.join("arc.aru");
    for (name, setup, strong, weak, operation) in [
        (
            "strong_clone",
            "",
            "2147483646",
            "1",
            "let copy=owner.clone()",
        ),
        (
            "weak_downgrade",
            "",
            "1",
            "2147483646",
            "let weak=owner.downgrade()",
        ),
        (
            "weak_clone",
            "let weak=owner.downgrade()",
            "1",
            "2147483646",
            "let copy=weak.clone()",
        ),
        (
            "weak_upgrade",
            "let weak=owner.downgrade()",
            "2147483646",
            "2",
            "let copy=weak.upgrade()",
        ),
    ] {
        // White-box fault injection avoids allocating billions of handles.
        // Arc's first pointer addresses strong/weak AtomicUsize fields, whose
        // underlying scalar layouts are independently exercised by atomics.
        let source = format!(
            r#"
module arcOverflow
import std.alloc.arc as arc
import std.core.mem as mem
import std.core.intrinsics as intrinsics
public func main():int {{
    let mut owner=arc.Arc<int>.new(42)
    {setup}
    unsafe {{
        let ownerAddress=intrinsics.addressOf<arc.Arc<int>>(mut ref owner) as ptr[ptr[usize]]
        let counts=mem.ptrRead<ptr[usize]>(ownerAddress)
        mem.ptrWrite<usize>(counts,{strong} as usize)
        mem.ptrWrite<usize>(mem.ptrOffset<usize>(counts,1 as isize),{weak} as usize)
    }}
    if owner.strongCount()!=({strong} as usize) {{ return 99 }}
    {operation}
    return 0
}}
"#
        );
        fs::write(&path, source).unwrap();
        let checked = run_with_deadline(common::cli_command().arg("check").arg(&path));
        assert!(
            checked.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let run = run_with_deadline(common::cli_command().arg("run").arg(&path));
        assert_runtime_abort(&run, name);
        let emitted = run_with_deadline(common::cli_command().arg("emit-c").arg(&path));
        assert!(
            emitted.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&emitted.stderr)
        );
        if Command::new("clang")
            .arg("--version")
            .output()
            .is_ok_and(|result| result.status.success())
        {
            let c = root.join("arc.c");
            let executable = root.join(if cfg!(windows) { "arc.exe" } else { "arc" });
            fs::write(&c, emitted.stdout).unwrap();
            for optimization in ["-O0", "-O2"] {
                let result = run_with_deadline(
                    Command::new("clang")
                        .args([optimization, "-fwrapv"])
                        .arg(&c)
                        .arg("-o")
                        .arg(&executable),
                );
                assert!(
                    result.status.success(),
                    "{name}: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_runtime_abort(&run_with_deadline(&mut Command::new(&executable)), name);
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

fn assert_runtime_abort(output: &Output, name: &str) {
    assert!(
        !output.status.success(),
        "{name}: counter increment unexpectedly succeeded"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            matches!(
                output.status.signal(),
                Some(libc::SIGILL | libc::SIGTRAP | libc::SIGABRT)
            ),
            "{name}: expected an explicit abort/trap, got {:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
