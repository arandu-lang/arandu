//! Native lock source API, guard lifetimes, canonical capabilities and C parity.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::{fs, process::Command};

fn check(body: &str) -> std::process::Output {
    let directory = common::temp_dir("arandu-sync-check").unwrap();
    let path = directory.join("check.aru");
    fs::write(&path, body).unwrap();
    let result = common::cli_command()
        .arg("check")
        .arg(path)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    result
}

#[test]
fn native_sync_guards_allow_shared_readers_and_reacquire_after_condvar_timeout() {
    let directory = common::temp_dir("arandu-native-sync").unwrap();
    let path = directory.join("sync.aru");
    fs::write(
        &path,
        include_str!("../../../tests/corpus/stdlib_native_sync.aru"),
    )
    .unwrap();
    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&path);
        if optimized {
            command.arg("--opt");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "JIT {optimized}: {:?}: {}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let emitted = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let source = directory.join("sync.c");
    fs::write(&source, &emitted.stdout).unwrap();
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    for optimization in ["-O0", "-O2"] {
        let executable = directory.join("sync.exe");
        let mut command = Command::new(&compiler);
        command.args([optimization, "-fwrapv"]);
        if cfg!(unix) {
            command.args([
                "-pthread",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
            ]);
        }
        let result = command
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "C {optimization}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let result = Command::new(executable).output().unwrap();
        assert!(
            result.status.success(),
            "C {optimization}: {:?}: {}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn lock_guards_and_borrows_cannot_escape_be_copied_or_outlive_their_owner() {
    for body in [
        "let lock = locks.RwLock<int>.new(1); let guard = lock.read(); let moved = lock; guard.get(); moved.intoInner()",
        "let lock = locks.RwLock<int>.new(1); let guard = lock.read(); let first = guard; guard.get(); first.get()",
        "let lock = locks.RwLock<Value>.new(Value {number:1}); let mut guard = lock.write(); let value = guard.getMut(); let moved = guard; value.number = 2; moved.get()",
        "let lock = locks.RwLock<Value>.new(Value {number:1}); let guard = lock.read(); let value = guard.get(); let moved = guard; value.number; moved.get()",
        "let lock = locks.RwLock<int>.new(1); let first = lock; let second = lock; first.intoInner(); second.intoInner()",
        "let lock = locks.RwLock<int>.new(1); let value = lock.value",
        "let cell = cells.UnsafeCell.new(1); let raw = cell.get()",
        "let lock = locks.RwLock<int>.new(1); let raw = lock.rawData()",
        "let lock = locks.RwLock<int>.new(1); let forged = locks.RwLockReadGuard.fromLocked(ref lock); forged.get()",
    ] {
        let result = check(&format!(
            "import std.sync.rwlock as locks\nimport std.core.cell as cells\nstruct Value {{number:int}}\nfunc main():int {{ {body}; return 0 }}"
        ));
        assert!(
            !result.status.success(),
            "invalid guard/borrow was accepted: {body}"
        );
        let diagnostics = String::from_utf8_lossy(&result.stderr);
        assert!(
            diagnostics.contains("O0") || diagnostics.contains("T041"),
            "unexpected diagnostics: {diagnostics}"
        );
    }
    for receiver in ["read", "write"] {
        let result = check(&format!(
            "import std.sync.rwlock as locks\nfunc escape():ref int {{ let lock=locks.RwLock<int>.new(1); let guard=lock.{receiver}(); return guard.get() }}\nfunc main():int {{return 0}}"
        ));
        assert!(
            !result.status.success(),
            "reference escaped local {receiver} guard"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("O0"));
    }
}

#[test]
fn projected_guard_through_caller_reference_stays_scoped_without_fallback() {
    let source = r#"
import std.sync.mutex as mutex
struct Value { number: int }
struct Outer { lock: mutex.Mutex<Value> }
func inspect(owner: ref Outer): int {
    let guard = owner.lock.lock()
    return guard.get().number
}
func main(): int {
    let owner = Outer { lock: mutex.Mutex<Value>.new(Value {number: 1}) }
    return inspect(ref owner) - 1
}
"#;
    let directory = common::temp_dir("arandu-sync-ref-projection").unwrap();
    let path = directory.join("scoped.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("check")
        .arg("--no-generational-fallback")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invalid = source
        .replace("func main(): int {", "func escape(): ref Value {")
        .replace(
            "    return inspect(ref owner) - 1",
            "    let guard = owner.lock.lock()\n    return guard.get()",
        );
    let output = check(&format!("{invalid}\nfunc main():int {{return 0}}"));
    assert!(
        !output.status.success(),
        "reference escaped the original stack owner through a projected guard"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("O010"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn synchronized_owners_require_the_correct_canonical_send_and_sync_proofs() {
    for (storage, capability, accepted) in [
        ("cells.UnsafeCell<int>", "Send", true),
        ("cells.UnsafeCell<int>", "Sync", false),
        ("locks.RwLock<int>", "Send", true),
        ("locks.RwLock<int>", "Sync", true),
        ("locks.RwLock<cells.UnsafeCell<int>>", "Send", true),
        ("locks.RwLock<cells.UnsafeCell<int>>", "Sync", false),
        ("mutex.Mutex<cells.UnsafeCell<int>>", "Sync", true),
        ("locks.RwLock<ptr[u8]>", "Send", false),
        ("mutex.Mutex<ptr[u8]>", "Sync", false),
        ("locks.RwLockReadGuard<int>", "Send", false),
        ("locks.RwLockWriteGuard<int>", "Sync", false),
        ("mutex.MutexGuard<int>", "Send", false),
        ("condvar.Condvar", "Sync", true),
        ("condvar.Condvar", "Copy", false),
        ("Mutex<ptr[u8]>", "Sync", false),
    ] {
        let source = format!(
            "import std.core.marker as marker\nimport std.core.cell as cells\nimport std.sync.rwlock as locks\nimport std.sync.mutex as mutex\nimport std.sync.condvar as condvar\nstruct Mutex<T>{{value:T}}\nfunc require<T:marker.{capability}>(value:T):void{{}}\nfunc inspect(value:{storage}):void{{require(value)}}\nfunc main():int{{return 0}}"
        );
        let result = check(&source);
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
}
