//! CLI command dispatching and execution routing.

pub mod bench;
pub mod build;
pub mod doc;
pub mod doctor;
pub mod hash;
pub mod project;
pub mod run;
pub mod test;

use std::env;
use std::path::{Path, PathBuf};

use crate::args::{self, parse_benchmark_percentage, parse_benchmark_seconds, usage_and_exit};
use crate::cli_error::{CliResult, CliSuccess};
use crate::pipeline::{fail_usage, is_project_target};
use crate::test_runner;

pub fn run(raw_args: Vec<String>) -> CliResult {
    let inv = args::parse_invocation(raw_args);

    // Initialise global perf flags (written once, read-only afterwards).
    arandu_base::init_z_flags(&inv.z_flags);

    // Initialise the tracing subscriber from -Zdebug-* / -Zself-profile flags.
    let tracing_cfg = arandu_base::build_tracing_config();
    arandu_base::tracing_bridge::init_tracing(tracing_cfg);

    if inv.args.len() == 2 && matches!(inv.args[1].as_str(), "--version" | "-V") {
        println!("arandu {}", env!("CARGO_PKG_VERSION"));
        return Ok(CliSuccess::Done);
    }

    if inv.args.len() < 2 {
        usage_and_exit();
    }

    let command = inv.args[1].as_str();
    if !inv.program_args.is_empty() && command != "run" {
        fail_usage("arguments after `--` are supported only by `arandu run`");
    }
    if inv.project_flags.accept_lock && command != "update" {
        fail_usage("--accept is valid only with 'arandu update'");
    }

    // ── Project / environment commands (no mandatory .aru path) ──────────
    match command {
        "doc" => return doc::cmd_doc(&inv.args, &inv.project_flags, inv.data_layout),
        "new" => return project::cmd_new(&inv.args),
        "init" => return project::cmd_init(&inv.args),
        "doctor" => {
            if inv.args.len() != 2 {
                fail_usage("usage: arandu_cli doctor [--stdlib-path=<dir>] [-v]");
            }
            return doctor::cmd_doctor(&inv.project_flags);
        }
        "cache" => return project::cmd_cache(&inv.args, &inv.project_flags),
        "hash-file" => {
            if inv.args.len() != 3 {
                fail_usage("usage: arandu_cli hash-file <path>");
            }
            return hash::cmd_hash_file(Path::new(&inv.args[2]));
        }
        "watch" => {
            let start = if inv.args.len() >= 3 {
                PathBuf::from(&inv.args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            return project::cmd_watch(&start, &inv.project_flags, inv.data_layout);
        }
        "clean" => {
            let start = if inv.args.len() >= 3 {
                PathBuf::from(&inv.args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            return project::cmd_clean(&start);
        }
        "tree" | "verify" | "audit" | "vendor" | "update" => {
            let start = if inv.args.len() >= 3 {
                PathBuf::from(&inv.args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            if command == "update" {
                return project::cmd_update(&start, &inv.project_flags);
            } else if command == "vendor" {
                return project::cmd_vendor(&start, &inv.project_flags);
            } else if command == "audit" {
                return project::cmd_audit(&start, &inv.project_flags);
            } else {
                return project::cmd_inspect(&start, &inv.project_flags, command == "verify");
            }
        }
        "build" => {
            let start = if inv.args.len() >= 3 {
                PathBuf::from(&inv.args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            return build::cmd_project_build(
                &start,
                &inv.project_flags,
                inv.opt,
                inv.debug,
                inv.data_layout,
            );
        }
        "test" => {
            test_runner::install_ctrlc_handler();
            let mut start = None;
            let mut list = false;
            let mut exact = None;
            let mut filter = None;
            let mut harness_child = false;
            let mut doc_tests = false;
            let mut runner = test_runner::RunnerOptions {
                jobs: 1,
                timeout: std::time::Duration::from_secs(300),
                fail_fast: false,
                seed: 0,
                format: test_runner::TestOutputFormat::Human,
                output: None,
                target: None,
                backend: None,
            };
            let mut arguments = inv.args[2..].iter();
            while let Some(argument) = arguments.next() {
                if argument == "--list" {
                    list = true;
                } else if argument == "--doc" {
                    doc_tests = true;
                } else if argument == "--harness-child" {
                    harness_child = true;
                } else if argument == "--exact" {
                    exact = arguments.next().cloned();
                    if exact.is_none() {
                        fail_usage("usage: arandu_cli test [package-path] --list [--exact <id>]");
                    }
                } else if argument == "--fail-fast" {
                    runner.fail_fast = true;
                } else if argument == "--jobs" {
                    runner.jobs = arguments
                        .next()
                        .and_then(|value| value.parse().ok())
                        .filter(|jobs| *jobs > 0)
                        .unwrap_or_else(|| {
                            fail_usage("--jobs requires an integer greater than zero")
                        });
                } else if argument == "--timeout" {
                    let seconds = arguments
                        .next()
                        .and_then(|value| value.parse::<u64>().ok())
                        .filter(|seconds| *seconds > 0)
                        .unwrap_or_else(|| {
                            fail_usage("--timeout requires seconds greater than zero")
                        });
                    runner.timeout = std::time::Duration::from_secs(seconds);
                } else if argument == "--seed" {
                    runner.seed = arguments
                        .next()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_else(|| fail_usage("--seed requires an unsigned integer"));
                } else if argument == "--format" {
                    runner.format = match arguments.next().map(String::as_str) {
                        Some("json") => test_runner::TestOutputFormat::Json,
                        Some("human") => test_runner::TestOutputFormat::Human,
                        Some("junit") => test_runner::TestOutputFormat::Junit,
                        _ => fail_usage("--format requires 'human', 'json' or 'junit'"),
                    };
                } else if argument == "--filter" {
                    filter = arguments.next().cloned();
                    if filter.is_none() {
                        fail_usage("--filter requires a literal substring");
                    }
                } else if argument == "--output" {
                    runner.output = arguments.next().map(PathBuf::from);
                    if runner.output.is_none() {
                        fail_usage("--output requires a file path");
                    }
                } else if argument.starts_with('-') || start.is_some() {
                    fail_usage("usage: arandu_cli test [package-path] --list [--exact <id>]");
                } else {
                    start = Some(PathBuf::from(argument));
                }
            }
            let start =
                start.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            return test::cmd_project_test_list(
                &start,
                &inv.project_flags,
                list,
                exact.as_deref(),
                filter.as_deref(),
                harness_child,
                &runner,
                inv.data_layout,
                doc_tests,
            );
        }
        "bench" => {
            let mut start = None;
            let mut list = false;
            let mut exact = None;
            let mut filter = None;
            let mut harness_child = false;
            let mut runner = test_runner::BenchmarkRunnerOptions {
                timeout: std::time::Duration::from_secs(300),
                config: arandu_codegen::testing::BenchmarkConfigV1 {
                    warmup_ns: 500_000_000,
                    measurement_ns: 3_000_000_000,
                    samples: 30,
                },
                format_json: false,
                output: None,
                target: None,
                backend: None,
                baseline: None,
            };
            let mut save_baseline = None;
            let mut compare_baseline = None;
            let mut strict_baseline = false;
            let mut dry_run = false;
            let mut max_regression_percent = 5.0;
            let mut noise_threshold_percent = 1.0;
            let mut comparison_policy_set = false;
            let mut arguments = inv.args[2..].iter();
            while let Some(argument) = arguments.next() {
                match argument.as_str() {
                    "--list" => list = true,
                    "--harness-child" => harness_child = true,
                    "--exact" => {
                        exact = arguments.next().cloned();
                        if exact.is_none() {
                            fail_usage("--exact requires a canonical benchmark id");
                        }
                    }
                    "--filter" => {
                        filter = arguments.next().cloned();
                        if filter.is_none() {
                            fail_usage("--filter requires a literal substring");
                        }
                    }
                    "--warmup" => {
                        runner.config.warmup_ns = parse_benchmark_seconds(
                            arguments.next(),
                            "--warmup requires positive seconds",
                        );
                    }
                    "--measurement-time" => {
                        runner.config.measurement_ns = parse_benchmark_seconds(
                            arguments.next(),
                            "--measurement-time requires positive seconds",
                        );
                    }
                    "--samples" => {
                        runner.config.samples = arguments
                            .next()
                            .and_then(|value| value.parse::<u32>().ok())
                            .filter(|samples| (10..=10_000).contains(samples))
                            .unwrap_or_else(|| {
                                fail_usage("--samples requires an integer from 10 to 10000")
                            });
                    }
                    "--timeout" => {
                        let seconds = arguments
                            .next()
                            .and_then(|value| value.parse::<u64>().ok())
                            .filter(|seconds| *seconds > 0)
                            .unwrap_or_else(|| fail_usage("--timeout requires positive seconds"));
                        runner.timeout = std::time::Duration::from_secs(seconds);
                    }
                    "--format" => {
                        runner.format_json = match arguments.next().map(String::as_str) {
                            Some("json") => true,
                            Some("human") => false,
                            _ => fail_usage("--format requires 'human' or 'json'"),
                        };
                    }
                    "--output" => {
                        runner.output = arguments.next().map(PathBuf::from);
                        if runner.output.is_none() {
                            fail_usage("--output requires a file path");
                        }
                    }
                    "--save-baseline" => {
                        save_baseline = arguments.next().cloned();
                        if save_baseline.is_none() {
                            fail_usage("--save-baseline requires a baseline name");
                        }
                    }
                    "--compare" | "--baseline" => {
                        compare_baseline = arguments.next().cloned();
                        if compare_baseline.is_none() {
                            fail_usage("--compare requires a baseline name");
                        }
                    }
                    "--strict" => strict_baseline = true,
                    "--dry-run" => dry_run = true,
                    "--max-regression" => {
                        comparison_policy_set = true;
                        max_regression_percent = parse_benchmark_percentage(
                            arguments.next(),
                            "--max-regression requires a percentage from 0 to 100",
                        );
                    }
                    "--noise-threshold" => {
                        comparison_policy_set = true;
                        noise_threshold_percent = parse_benchmark_percentage(
                            arguments.next(),
                            "--noise-threshold requires a percentage from 0 to 100",
                        );
                    }
                    _ if argument.starts_with('-') || start.is_some() => {
                        fail_usage("usage: arandu_cli bench [package-path] [flags]");
                    }
                    _ => start = Some(PathBuf::from(argument)),
                }
            }
            if save_baseline.is_some() && compare_baseline.is_some() {
                fail_usage("--save-baseline and --compare are mutually exclusive");
            }
            if (strict_baseline || dry_run || comparison_policy_set) && compare_baseline.is_none() {
                fail_usage(
                    "--strict, --dry-run, --max-regression and --noise-threshold require --compare",
                );
            }
            runner.baseline = if let Some(name) = save_baseline {
                Some(test_runner::BenchmarkBaselineMode::Save { name })
            } else {
                compare_baseline.map(|name| test_runner::BenchmarkBaselineMode::Compare {
                    name,
                    strict: strict_baseline,
                    dry_run,
                    max_regression_percent,
                    noise_threshold_percent,
                })
            };
            let start =
                start.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            return bench::cmd_project_bench(
                &start,
                &inv.project_flags,
                list,
                exact.as_deref(),
                filter.as_deref(),
                harness_child,
                &runner,
                inv.data_layout,
            );
        }
        "check" | "run"
            if inv.args.len() == 2 || is_project_target(inv.args.get(2).map(String::as_str)) =>
        {
            let start = if inv.args.len() >= 3 {
                PathBuf::from(&inv.args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            if command == "check" {
                return run::cmd_project_check(
                    &start,
                    &inv.project_flags,
                    inv.opt,
                    inv.debug,
                    inv.data_layout,
                );
            } else {
                return run::cmd_project_run(
                    &start,
                    &inv.project_flags,
                    inv.opt,
                    inv.debug,
                    inv.data_layout,
                    &inv.program_args,
                );
            }
        }
        _ => {}
    }

    // ── Legacy single-path commands ──────────────────────────────────────
    if inv.args.len() != 3 {
        usage_and_exit();
    }

    if !matches!(
        command,
        "lex"
            | "parse"
            | "check"
            | "hir"
            | "amir"
            | "run"
            | "emit-c"
            | "emit-wasm"
            | "emit-component"
            | "graph"
            | "fmt"
    ) {
        usage_and_exit();
    }

    let target_path = Path::new(&inv.args[2]);
    run::cmd_single_file_dispatch(command, target_path, &inv)
}
