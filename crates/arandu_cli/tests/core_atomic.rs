//! Execute the atomic API and reject invalid ABI/ordering before memory access.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::{fs, process::Command};

fn command_without_core_dump() -> Command {
    let command = common::cli_command();
    #[cfg(unix)]
    let command = {
        let mut command = command;
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec changes only this child process's resource limit;
        // setrlimit is called without allocating or taking application locks.
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
        command
    };
    command
}

#[test]
fn atomic_operations_cover_widths_wrapping_cas_and_pointers_in_jit_and_c() {
    let root = common::temp_dir("arandu-atomic-contract").unwrap();
    let path = root.join("atomic.aru");
    fs::write(
        &path,
        include_str!("../../../tests/corpus/stdlib_atomic.aru"),
    )
    .unwrap();
    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&path);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "JIT: {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let emitted = String::from_utf8(output.stdout).unwrap();
    assert!(
        emitted.contains("__atomic_fetch_add") && emitted.contains("__atomic_compare_exchange_n")
    );
    let source = root.join("atomic.c");
    fs::write(&source, emitted).unwrap();
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        for optimization in ["-O0", "-O2"] {
            let executable = root.join("atomic.exe");
            let mut command = Command::new("clang");
            command.args([optimization, "-fwrapv"]);
            if cfg!(unix) {
                command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
            }
            let output = command
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = Command::new(executable).output().unwrap();
            assert!(
                output.status.success(),
                "C {optimization}: {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_invalid_orders_trap_before_memory_access() {
    let root = common::temp_dir("arandu-atomic-order").unwrap();
    let path = root.join("invalid.aru");
    for expression in [
        "value.load(atomic.MemoryOrder.Release)",
        "value.load(atomic.MemoryOrder.AcqRel)",
        "value.store(1, atomic.MemoryOrder.Acquire)",
        "value.store(1, atomic.MemoryOrder.AcqRel)",
        "value.compareExchange(1, 2, atomic.MemoryOrder.SeqCst, atomic.MemoryOrder.Release)",
        "value.compareExchange(1, 2, atomic.MemoryOrder.Relaxed, atomic.MemoryOrder.Acquire)",
        "atomic.fence(atomic.MemoryOrder.Relaxed)",
        "atomic.compilerFence(atomic.MemoryOrder.Relaxed)",
    ] {
        fs::write(&path, format!("import std.core.atomic as atomic\nfunc main(): int {{ let mut value = atomic.AtomicInt.new(1); {expression}; return 0 }}")).unwrap();
        let output = command_without_core_dump()
            .arg("run")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {expression}");
        assert!(
            output.stderr.is_empty(),
            "expected execution trap for {expression}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_storage_and_intrinsic_signatures_cannot_be_forged() {
    let root = common::temp_dir("arandu-atomic-invalid").unwrap();
    let path = root.join("invalid.aru");
    for declaration in [
        "func atomicFetchAdd32(value: ref u32, operand: u32): u32",
        "func atomicLoad64(value: ref u32): u64",
        "func atomicCompareExchange32(value: mut ref u32, expected: u32): u32",
        "func atomicStoreUsize(value: mut ref usize, replacement: usize): usize",
        "func atomicFence(order: int): void",
        "func atomicFetchAddPtr(value: mut ref ptr[u8], operand: ptr[u8]): ptr[u8]",
    ] {
        fs::write(&path, format!("module std.core.invalidAtomic\nextern \"arandu-intrinsic\" {{ {declaration} }}\nfunc main(): int {{ return 0 }}")).unwrap();
        let output = common::cli_command()
            .arg("check")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {declaration}");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains("U001") && diagnostics.contains("unsupported signature"),
            "{diagnostics}"
        );
    }
    fs::write(&path, "import std.core.atomic as atomic\nfunc main(): int { let value = atomic.AtomicInt { val: 0 as u32 }; return 0 }").unwrap();
    let output = common::cli_command()
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("T041"));
    fs::remove_dir_all(root).unwrap();
}
