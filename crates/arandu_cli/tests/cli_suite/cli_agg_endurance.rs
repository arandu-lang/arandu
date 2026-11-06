//! AGG.1 endurance: `tests/corpus/agg1_endurance.aru` creates and drops every
//! owned value of each fallback shape the AGG.1 probe covered once per round.
//! The result checks correctness, AddressSanitizer/LeakSanitizer check
//! cleanup, and the resident set of ten times the rounds must stay where it
//! was: a value that is never released shows as RSS growing with the rounds.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common;

const CORPUS: &str = include_str!("../../../../tests/corpus/agg1_endurance.aru");
const POINTER_RESULTS: &str = include_str!("../../../../tests/corpus/agg1_pointer_results.aru");
const SMALL: u32 = 20_000;
const LARGE: u32 = 200_000;
/// Allocator and page noise between the two runs, far below what a single
/// leaked byte per round would add over `LARGE - SMALL` rounds.
const RSS_TOLERANCE_KIB: i64 = 4 * 1024;

fn program(directory: &Path, rounds: u32) -> PathBuf {
    program_of(CORPUS, directory, rounds)
}

fn program_of(corpus: &str, directory: &Path, rounds: u32) -> PathBuf {
    let path = directory.join(format!("agg1_endurance_{rounds}.aru"));
    assert!(
        corpus.contains("const ROUNDS = 2000\n"),
        "the corpus declares its round count"
    );
    let source = corpus.replace(
        "const ROUNDS = 2000\n",
        &format!("const ROUNDS = {rounds}\n"),
    );
    fs::write(&path, source).unwrap();
    path
}

/// Exit code and peak resident set of `command`, measured for that child
/// alone with `wait4`, which also reaps it.
#[allow(clippy::zombie_processes)]
fn measure(mut command: Command) -> (Option<i32>, i64) {
    let child = command.spawn().expect("spawn");
    let mut status = 0;
    // SAFETY: `wait4` writes the status and the zeroed `rusage` it is given
    // for the child this test spawned and has not reaped.
    let usage = unsafe {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        assert_eq!(libc::wait4(pid, &mut status, 0, usage.as_mut_ptr()), pid);
        usage.assume_init()
    };
    let code = libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status));
    // Linux reports kilobytes, macOS bytes.
    let rss = if cfg!(target_os = "macos") {
        usage.ru_maxrss / 1024
    } else {
        usage.ru_maxrss
    };
    (code, rss)
}

fn clang() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn emit_c(directory: &Path, rounds: u32, flags: &[&str]) -> PathBuf {
    let source = program(directory, rounds);
    let output = common::cli_command()
        .arg("emit-c")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = directory.join(format!("agg1_{rounds}.c"));
    fs::write(&c, &output.stdout).unwrap();
    let binary = directory.join(format!("agg1_{rounds}_{}", flags.len()));
    let compile = Command::new("clang")
        .args(flags)
        .arg(&c)
        .arg("-o")
        .arg(&binary)
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    binary
}

#[test]
fn every_fallback_shape_is_released_in_c() {
    if !clang() {
        return;
    }
    let directory = common::temp_dir("arandu-agg1-c").unwrap();
    let sanitized = emit_c(
        &directory,
        2_000,
        &["-fsanitize=address", "-fno-omit-frame-pointer"],
    );
    let mut command = Command::new(&sanitized);
    command.env(
        "ASAN_OPTIONS",
        if cfg!(target_os = "linux") {
            "detect_leaks=1"
        } else {
            "detect_leaks=0"
        },
    );
    let run = command.output().unwrap();
    assert!(
        run.status.success(),
        "ASan/LSan: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let (small_code, small) = measure(Command::new(emit_c(&directory, SMALL, &["-O1"])));
    let (large_code, large) = measure(Command::new(emit_c(&directory, LARGE, &["-O1"])));
    assert_eq!((small_code, large_code), (Some(0), Some(0)));
    assert!(
        large - small <= RSS_TOLERANCE_KIB,
        "C RSS grows with the rounds: {small} KiB -> {large} KiB"
    );
    let _ = fs::remove_dir_all(directory);
}

/// The JIT keeps them on the invocation as C keeps them by value: a Copy
/// aggregate carrying a view (`Option<ref T>`) once fell outside invocation
/// scratch into `malloc` nothing freed, ~225 bytes per `GenArena.getRef`.
#[test]
fn every_fallback_shape_is_released_on_the_jit() {
    assert_jit_rss_is_bounded(CORPUS, "arandu-agg1-jit");
}

/// A function whose result may be an address (`ptr`, an unproven `ref`) was
/// kept out of invocation scratch, so its aggregate temporaries leaked per
/// call. It is promoted when it makes no address of its own storage.
#[test]
fn pointer_results_release_their_temporaries_on_the_jit() {
    assert_jit_rss_is_bounded(POINTER_RESULTS, "arandu-agg1-pointer");
}

fn assert_jit_rss_is_bounded(corpus: &str, label: &str) {
    let directory = common::temp_dir(label).unwrap();
    let run = |rounds| {
        let mut command = common::cli_command();
        command
            .arg("run")
            .arg(program_of(corpus, &directory, rounds));
        measure(command)
    };
    let (small_code, small) = run(SMALL);
    let (large_code, large) = run(LARGE);
    assert_eq!((small_code, large_code), (Some(0), Some(0)));
    assert!(
        large - small <= RSS_TOLERANCE_KIB,
        "JIT RSS grows with the rounds: {small} KiB -> {large} KiB"
    );
    let _ = fs::remove_dir_all(directory);
}
