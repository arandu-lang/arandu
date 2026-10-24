//! Integration tests for Rc, Arc, Slab, and VecDeque across CLI compilation modes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::cli_native_thread::{assert_runtime_abort, run_with_deadline};
use crate::{common, wasm_host};
use std::{fs, process::Command};

const CONTRACT: &str = include_str!("../../../../tests/corpus/stdlib_alloc_structures.aru");

#[test]
fn alloc_structures_rc_arc_slab_and_deque() {
    run_contract(CONTRACT, None);
}

#[test]
fn rc_arc_unwrap_and_last_drop_do_not_resurrect_payloads_through_weak() {
    run_contract(
        include_str!("../../../../tests/corpus/rc_lifecycle.aru"),
        Some("11\n22\n33\n44\n55\n66\n"),
    );
}

#[test]
fn deque_drops_each_noncopy_element_exactly_once() {
    run_contract(
        include_str!("../../../../tests/corpus/deque_drops.aru"),
        Some("1\n2\n3\n11\n0\n4\n5\n6\n7\n8\n9\n30\n19\n20\n21\n99\n"),
    );
}

#[test]
fn loop_reassigned_owner_keeps_its_backing_until_dropped() {
    // Regression: Wasm reclaimed a call site's returned backing on re-entry
    // while a loop-carried local still owned it (double free, then a hang).
    run_contract(
        include_str!("../../../../tests/corpus/loop_reassigned_owner.aru"),
        Some("0\n1\n2\n3\n4\n"),
    );
}

#[test]
#[cfg(target_pointer_width = "64")]
fn deque_rejects_capacity_that_cannot_be_rounded_without_overflow() {
    let root = common::temp_dir("arandu-deque-capacity-overflow").expect("temporary directory");
    let source = root.join("deque_capacity_overflow.aru");
    fs::write(
        &source,
        r#"
import std.alloc.deque as deque

func main(): int {
    let _q = deque.VecDeque<u8>.withCapacity((18446744073709551615 as u64) as usize)
    return 0
}
"#,
    )
    .expect("write overflow contract");

    let output = common::cli_command()
        .arg("run")
        .arg(&source)
        .output()
        .expect("run CLI");
    assert!(
        !output.status.success(),
        "unrepresentable deque capacity must abort instead of wrapping or hanging"
    );
}

fn run_contract(contract: &str, expected_stdout: Option<&str>) {
    let root = common::temp_dir("arandu-alloc-contract").expect("temporary directory");
    let source = root.join("alloc_structures.aru");
    fs::write(&source, contract).expect("write contract");

    for mode in ["run", "emit-c", "emit-wasm"] {
        let output = common::cli_command()
            .arg(mode)
            .arg(&source)
            .output()
            .expect("run CLI");
        assert!(
            output.status.success(),
            "{mode}: {:?}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "run"
            && let Some(expected) = expected_stdout
        {
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
                expected
            );
        }
        if mode == "emit-wasm" {
            let run = wasm_host::run_main(&output.stdout);
            run.assert_returns(0, "alloc contract in Wasmtime");
            if let Some(expected) = expected_stdout {
                assert_eq!(run.stdout, expected, "Wasmtime drop transcript");
            }
        }
        if mode == "emit-c"
            && Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c = root.join("alloc_structures.c");
            fs::write(&c, &output.stdout).expect("write C");
            for optimization in ["-O0", "-O2"] {
                let executable = root.join(if cfg!(windows) {
                    "alloc_structures.exe"
                } else {
                    "alloc_structures"
                });
                let mut command = Command::new("clang");
                command.args([optimization, "-fwrapv"]);
                if cfg!(unix) {
                    command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
                }
                let compile = command
                    .arg(&c)
                    .arg("-o")
                    .arg(&executable)
                    .output()
                    .expect("compile C");
                assert!(
                    compile.status.success(),
                    "clang {optimization}: {:?}\n{}",
                    compile.status.code(),
                    String::from_utf8_lossy(&compile.stderr)
                );
                let run = Command::new(&executable).output().expect("run compiled C");
                assert_eq!(
                    run.status.code(),
                    Some(0),
                    "compiled alloc contract failed with exit code {:?}\n{}",
                    run.status.code(),
                    String::from_utf8_lossy(&run.stderr)
                );
                if let Some(expected) = expected_stdout {
                    assert_eq!(
                        String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n"),
                        expected
                    );
                }
            }
        }
    }
}
fn clang_available() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|version| version.status.success())
}

/// Compile generated C with optional extra flags for the host target.
fn compile_c(
    c: &std::path::Path,
    executable: &std::path::Path,
    optimization: &str,
    extra: &[&str],
) {
    let compiled = run_with_deadline(
        Command::new("clang")
            .args([optimization, "-fwrapv"])
            .args(extra)
            .arg(c)
            .arg("-o")
            .arg(executable)
            .args(arandu_middle::TargetSpec::for_host().cc_link_args()),
    );
    assert!(
        compiled.status.success(),
        "clang {optimization}: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
}

/// Count-overflow contract shared by `Rc`/`Weak`: one increment past
/// `countLimit()` aborts before writing, and the increment that reaches the
/// limit still succeeds. Mirrors the white-box Arc regression in
/// `cli_native_thread.rs`; `RcBox` starts with `strong`/`weak: usize`.
#[test]
fn rc_reference_counts_abort_before_overflow() {
    const LIMIT: u64 = 2_147_483_646;
    let root = common::temp_dir("arandu-rc-overflow").unwrap();
    let path = root.join("rc.aru");
    for (name, setup, strong_at_limit, operation, check) in [
        (
            "strong_clone",
            "",
            true,
            "let copy=owner.clone()",
            "owner.strongCount()",
        ),
        (
            "weak_downgrade",
            "",
            false,
            "let weak=owner.downgrade()",
            "owner.weakCount()+(1 as usize)",
        ),
        (
            "weak_clone",
            "let weak=owner.downgrade()",
            false,
            "let copy=weak.clone()",
            "owner.weakCount()+(1 as usize)",
        ),
        (
            "weak_upgrade",
            "let weak=owner.downgrade()",
            true,
            "let copy=weak.upgrade()",
            "owner.strongCount()",
        ),
    ] {
        for at_limit in [true, false] {
            let counter = if at_limit { LIMIT } else { LIMIT - 1 };
            let (strong, weak) = match (strong_at_limit, setup.is_empty()) {
                (true, true) => (counter, 1),
                (true, false) => (counter, 2),
                (false, _) => (1, counter),
            };
            let case = format!("{name} from {counter}");
            let source = format!(
                r#"
module rcOverflow
import std.alloc.rc as rc
import std.core.mem as mem
import std.core.intrinsics as intrinsics
public func main():int {{
    let mut owner=rc.Rc<int>.new(42)
    {setup}
    unsafe {{
        let ownerAddress=intrinsics.addressOf<rc.Rc<int>>(mut ref owner) as ptr[ptr[usize]]
        let counts=mem.ptrRead<ptr[usize]>(ownerAddress)
        mem.ptrWrite<usize>(counts,{strong} as usize)
        mem.ptrWrite<usize>(mem.ptrOffset<usize>(counts,1 as isize),{weak} as usize)
    }}
    if owner.strongCount()!=({strong} as usize) || owner.weakCount()!=({weak} as usize)-(1 as usize) {{ return 99 }}
    {operation}
    if {check}!=({LIMIT} as usize) {{ return 98 }}
    return 0
}}
"#
            );
            fs::write(&path, source).unwrap();
            let expect = |output: &std::process::Output, backend: &str| {
                if at_limit {
                    assert_runtime_abort(output, &format!("{case} ({backend})"));
                } else {
                    assert_eq!(
                        output.status.code(),
                        Some(0),
                        "{case} ({backend}) must reach the limit: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            };
            expect(
                &run_with_deadline(common::cli_command().arg("run").arg(&path)),
                "JIT",
            );
            let wasm = run_with_deadline(common::cli_command().arg("emit-wasm").arg(&path));
            assert!(wasm.status.success(), "{case}: emit-wasm failed");
            let run = wasm_host::run_main(&wasm.stdout);
            if at_limit {
                run.assert_traps(&case);
            } else {
                run.assert_returns(0, &case);
            }
            if !clang_available() {
                continue;
            }
            let emitted = run_with_deadline(common::cli_command().arg("emit-c").arg(&path));
            assert!(emitted.status.success(), "{case}: emit-c failed");
            let c = root.join("rc.c");
            let executable = root.join(if cfg!(windows) { "rc.exe" } else { "rc" });
            fs::write(&c, emitted.stdout).unwrap();
            for optimization in ["-O0", "-O2"] {
                compile_c(&c, &executable, optimization, &[]);
                expect(
                    &run_with_deadline(&mut Command::new(&executable)),
                    optimization,
                );
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

/// Allocation-failure contract of Box/Rc/Arc in generated C: `tryNew` returns
/// `Err` and destroys the rejected payload exactly once; `new` aborts.
///
/// `ar_rt_alloc_aligned` is a `static` runtime function, so the seam is the
/// libc `malloc` it calls. A 328-byte payload puts every control-block request
/// in a size window no other allocation of these programs uses; the unwrapped
/// build is the control proving that window is the one exercised. The JIT
/// allocates through Rust's global allocator and has no injection seam.
#[test]
#[cfg(target_os = "linux")]
fn box_rc_arc_report_or_abort_on_allocation_failure_in_generated_c() {
    if !clang_available() {
        return;
    }
    const WRAP: &str = "\nvoid *__real_malloc(size_t size);\nvoid *__wrap_malloc(size_t size) { if (size >= 328 && size < 400) return NULL; return __real_malloc(size); }\n";
    let fields = (0..40)
        .map(|index| format!("w{index}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let values = (0..40)
        .map(|index| format!("w{index}: {index} as i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let root = common::temp_dir("arandu-alloc-failure").unwrap();
    let path = root.join("failure.aru");
    let c = root.join("failure.c");
    let executable = root.join("failure");
    for (module, owner) in [
        ("std.alloc.boxed", "Box"),
        ("std.alloc.rc", "Rc"),
        ("std.alloc.arc", "Arc"),
    ] {
        for constructor in ["tryNew", "new"] {
            let body = if constructor == "tryNew" {
                format!(
                    "match owner.{owner}<Probe>.tryNew(probe(7)) {{ Result.Ok(_) => {{ return 10 }} Result.Err(_) => {{}} }}\n    io.println(8)\n    return 0"
                )
            } else {
                format!("let held = owner.{owner}<Probe>.new(probe(7))\n    return 10")
            };
            fs::write(
                &path,
                format!(
                    "module allocFailure\nimport {module} as owner\nimport std.core.io as io\nstruct Probe {{ tag: int, {fields} }}\n@Destructor\nfunc Probe.destroy(self: own Probe): void {{ io.println(self.tag) }}\nfunc probe(tag: int): Probe {{ return Probe {{ tag: tag, {values} }} }}\npublic func main(): int {{\n    {body}\n}}\n"
                ),
            )
            .unwrap();
            let emitted = run_with_deadline(common::cli_command().arg("emit-c").arg(&path));
            assert!(
                emitted.status.success(),
                "{owner}.{constructor}: {}",
                String::from_utf8_lossy(&emitted.stderr)
            );
            let mut wrapped = String::from_utf8(emitted.stdout).unwrap();
            fs::write(&c, &wrapped).unwrap();
            compile_c(&c, &executable, "-O0", &[]);
            let control = run_with_deadline(&mut Command::new(&executable));
            assert_eq!(
                (control.status.code(), control.stdout.as_slice()),
                (Some(10), b"7\n".as_slice()),
                "{owner}.{constructor}: control run must allocate"
            );
            wrapped.push_str(WRAP);
            fs::write(&c, &wrapped).unwrap();
            for optimization in ["-O0", "-O2"] {
                compile_c(
                    &c,
                    &executable,
                    optimization,
                    &[
                        "-fsanitize=undefined",
                        "-fno-sanitize-recover=all",
                        "-Wl,--wrap=malloc",
                    ],
                );
                let failed = run_with_deadline(&mut Command::new(&executable));
                let case = format!("{owner}.{constructor} {optimization}");
                if constructor == "tryNew" {
                    assert_eq!(
                        (
                            failed.status.code(),
                            String::from_utf8_lossy(&failed.stdout)
                        ),
                        (Some(0), "7\n8\n".into()),
                        "{case}: {}",
                        String::from_utf8_lossy(&failed.stderr)
                    );
                } else {
                    assert_runtime_abort(&failed, &case);
                    // `new` delegates to `tryNew`, which destroys the rejected
                    // payload before the abort; buffered stdout may be lost
                    // with the abort, but the payload never drops twice.
                    let printed = String::from_utf8_lossy(&failed.stdout);
                    assert!(
                        printed.is_empty() || printed == "7\n",
                        "{case}: unexpected drop transcript {printed:?}"
                    );
                }
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}
