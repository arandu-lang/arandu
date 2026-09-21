//! Sequential prerequisite for structured jobs: statically dispatched generic
//! work with explicit context and a result larger than the i64 executor ABI.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

mod common;

#[test]
fn static_job_preserves_aggregate_result_with_and_without_optimization() {
    let directory = common::temp_dir("arandu-static-job").unwrap();
    let source = directory.join("main.aru");
    fs::write(
        &source,
        r#"
struct Stats { code: int, comment: int, blank: int }
interface Job {
    func run(shared self): Stats
}
struct CountJob { amount: int }
func CountJob.run(shared self): Stats {
    return Stats { code: self.amount, comment: 2, blank: 3 }
}
func execute<J: Job>(job: J): Stats {
    return job.run()
}
func main(): int {
    let stats = execute<CountJob>(CountJob { amount: 37 })
    if stats.code != 37 { return 1 }
    if stats.comment != 2 { return 2 }
    if stats.blank != 3 { return 3 }
    return 0
}
"#,
    )
    .unwrap();

    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&source);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().expect("run statically dispatched job");
        assert_eq!(
            output.status.code(),
            Some(0),
            "optimized={optimized}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn owned_job_context_and_generic_result_are_destroyed_once() {
    let directory = common::temp_dir("arandu-owned-job").unwrap();
    let source = directory.join("main.aru");
    fs::write(&source, include_str!("fixtures/owned_job_lifecycle.aru")).unwrap();
    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&source);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "optimized={optimized}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            "10\n30\n20\n",
            "optimized={optimized}"
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn public_parallel_fold_crosses_cutoff_on_the_jit_backend() {
    let directory = common::temp_dir("arandu-parallel-fold-jit").unwrap();
    let source = directory.join("main.aru");
    fs::write(
        &source,
        r#"
import std.alloc.vec as vec
import std.parallel as parallel

struct Sum { value: int }
struct Item { a: int, b: int, c: int }
struct SumJob { enabled: bool }
func SumJob.run(self: ref SumJob, item: ref Item, state: mut ref Sum): void {
    if self.enabled {
        state.value = state.value + item.a
    }
}
struct SumCombine {}
func SumCombine.combine(self: ref SumCombine, dest: mut ref Sum, partial: ref Sum): void {
    dest.value = dest.value + partial.value
}

func check(items: []Item, workers: uint): int {
    let identity = Sum { value: 0 }
    match parallel.parallelFold<Item, Sum, SumJob, SumCombine>(
        items,
        Sum { value: 7 },
        ref identity,
        SumJob { enabled: true },
        SumCombine {},
        workers
    ) {
        Ok(actual) => { return actual.value }
        Err(_) => { return -1 }
    }
}

func checkWithGrain(items: []Item, workers: uint, grain: uint, maxChunks: uint): int {
    let identity = Sum { value: 0 }
    match parallel.parallelFoldWithGrain<Item, Sum, SumJob, SumCombine>(
        items,
        Sum { value: 7 },
        ref identity,
        SumJob { enabled: true },
        SumCombine {},
        workers,
        grain,
        maxChunks
    ) {
        Ok(actual) => { return actual.value }
        Err(_) => { return -1 }
    }
}

func main(): int {
    let mut values = vec.new<Item>()
    let mut i: int = 0
    while i < 1025 {
        vec.push<Item>(values, Item { a: 1, b: 2, c: 3 })
        i = i + 1
    }
    let items = vec.asSlice<Item>(values)
    let one = check(items, 1)
    let four = check(items, 4)
    let fineOne = checkWithGrain(items, 1, 1, 2048)
    let fineFour = checkWithGrain(items, 4, 1, 2048)
    let invalidPolicy = checkWithGrain(items, 4, 0, 2048)
    vec.destroy<Item>(values)
    if one != 1032 { return 1 }
    if four != one { return 2 }
    if fineOne != one { return 3 }
    if fineFour != fineOne { return 4 }
    if invalidPolicy != -1 { return 5 }
    return 0
}
"#,
    )
    .unwrap();

    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&source);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().expect("run public parallel fold");
        assert_eq!(
            output.status.code(),
            Some(0),
            "optimized={optimized}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
