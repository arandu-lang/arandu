//! CLI command dispatching and execution routing.

pub mod archive;
pub mod bench;
pub mod build;
pub mod doc;
pub mod doctor;
pub mod fmt;
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

/// Canonical commands used by dispatch validation and shell completions.
pub const COMMAND_NAMES: &[&str] = &[
    "run",
    "build",
    "check",
    "test",
    "bench",
    "doc",
    "new",
    "init",
    "watch",
    "clean",
    "tree",
    "audit",
    "vendor",
    "verify",
    "update",
    "fmt",
    "doctor",
    "cache",
    "lex",
    "parse",
    "hir",
    "amir",
    "graph",
    "emit-c",
    "emit-wasm",
    "emit-component",
    "hash-file",
    "completions",
    "archive",
];

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
    if matches!(inv.args.get(1).map(String::as_str), Some("-h" | "--help")) {
        println!("{}", args::global_help());
        return Ok(CliSuccess::Done);
    }

    let command = inv.args[1].as_str();
    if args::command_help(command).is_some()
        && inv.args[2..]
            .iter()
            .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        println!("{}", args::command_help(command).unwrap_or_default());
        return Ok(CliSuccess::Done);
    }
    if !inv.program_args.is_empty() && command != "run" {
        fail_usage("arguments after `--` are supported only by `arandu run`");
    }
    if inv.watch && command != "check" {
        fail_usage(
            "--watch is currently supported by 'arandu check'; use 'arandu watch' for the package watch command",
        );
    }
    if inv.watch
        && inv
            .args
            .get(2)
            .is_some_and(|path| !is_project_target(Some(path)))
    {
        fail_usage(
            "'arandu check --watch' requires a package directory or Arandu.toml, not a single .aru file",
        );
    }
    if inv.project_flags.accept_lock && command != "update" {
        fail_usage("--accept is valid only with 'arandu update'");
    }

    // ── Project / environment commands (no mandatory .aru path) ──────────
    match command {
        "completions" => {
            if inv.args.len() != 3 {
                fail_usage("usage: arandu completions <bash|zsh|fish|powershell>");
            }
            print_completions(&inv.args[2]);
            return Ok(CliSuccess::Done);
        }
        "fmt" => {
            let mut check_only = false;
            let mut paths = Vec::new();
            for arg in &inv.args[2..] {
                if arg == "--check" {
                    check_only = true;
                } else if arg.starts_with('-') {
                    fail_usage(format!("unknown option for fmt: {arg}"));
                } else {
                    paths.push(PathBuf::from(arg));
                }
            }
            if paths.is_empty() {
                paths.push(PathBuf::from("."));
            }
            return fmt::cmd_format_paths(&paths, check_only, inv.quiet);
        }
        "archive" => return archive::cmd_archive(&inv.args),
        "doc" => return doc::cmd_doc(&inv.args, &inv.project_flags, inv.data_layout),
        "new" => return project::cmd_new(&inv.args, inv.quiet),
        "init" => return project::cmd_init(&inv.args, inv.quiet),
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
            return project::cmd_clean(&start, inv.quiet);
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
                inv.genref_report,
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
                target: inv.project_flags.target.clone(),
                backend: Some(
                    // Test and benchmark children currently execute through the dev JIT.
                    crate::project::BackendChoice::CraneliftDev
                        .label()
                        .to_string(),
                ),
                doc_tests: false,
            };
            let mut arguments = inv.args[2..].iter();
            while let Some(argument) = arguments.next() {
                if argument == "--list" {
                    list = true;
                } else if argument == "--doc" {
                    doc_tests = true;
                    runner.doc_tests = true;
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
                target: inv.project_flags.target.clone(),
                backend: Some(
                    // Test and benchmark children currently execute through the dev JIT.
                    crate::project::BackendChoice::CraneliftDev
                        .label()
                        .to_string(),
                ),
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
                            "--max-regression requires a non-negative percentage",
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
                if inv.watch {
                    return crate::watch::cmd_watch(&start, &inv.project_flags, inv.data_layout);
                }
                return run::cmd_project_check(
                    &start,
                    &inv.project_flags,
                    inv.opt,
                    inv.debug,
                    inv.parallel,
                    inv.genref_report,
                    inv.data_layout,
                );
            } else {
                if inv.parallel {
                    fail_usage("--parallel is supported only for 'arandu check'");
                }
                return run::cmd_project_run(
                    &start,
                    &inv.project_flags,
                    inv.opt,
                    inv.debug,
                    inv.genref_report,
                    inv.data_layout,
                    &inv.program_args,
                );
            }
        }
        _ => {}
    }

    if !COMMAND_NAMES.contains(&command) {
        let suggestion = COMMAND_NAMES
            .iter()
            .map(|candidate| (*candidate, edit_distance(command, candidate)))
            .filter(|(_, distance)| *distance <= 2)
            .min_by_key(|(_, distance)| *distance)
            .map(|(candidate, _)| candidate);
        match suggestion {
            Some(candidate) => fail_usage(format!(
                "error: unknown command '{command}'. Did you mean '{candidate}'?"
            )),
            None => fail_usage(format!("error: unknown command '{command}'")),
        }
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
    ) {
        usage_and_exit();
    }

    let target_path = Path::new(&inv.args[2]);
    run::cmd_single_file_dispatch(command, target_path, &inv)
}

fn print_completions(shell: &str) {
    let commands = COMMAND_NAMES.join(" ");
    match shell {
        "bash" => println!(
            "_arandu() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  COMPREPLY=( $(compgen -W \"{commands} --help --version --release --target --layout --quiet --verbose\" -- \"$cur\") )\n}}\ncomplete -F _arandu arandu arandu_cli"
        ),
        "zsh" => println!(
            "#compdef arandu arandu_cli\n_arguments '1:command:({commands})' '*:argument: '"
        ),
        "fish" => {
            for binary in ["arandu", "arandu_cli"] {
                for command in COMMAND_NAMES {
                    println!("complete -c {binary} -n '__fish_use_subcommand' -a '{command}'");
                }
            }
            for binary in ["arandu", "arandu_cli"] {
                println!("complete -c {binary} -l help -s h -d 'Show help'");
                println!("complete -c {binary} -l version -s V -d 'Show version'");
                println!("complete -c {binary} -l quiet -s q -d 'Suppress status output'");
            }
        }
        "powershell" => {
            let mut output = String::from(
                "Register-ArgumentCompleter -CommandName arandu,arandu_cli -ScriptBlock {",
            );
            output.push('\n');
            output.push_str(
                "  param($commandName, $parameterName, $wordToComplete, $commandAst, $fakeBoundParameters)",
            );
            output.push('\n');
            output.push_str("  $commands = @(");
            for command in COMMAND_NAMES {
                output.push('\'');
                output.push_str(command);
                output.push_str("',");
            }
            output.push_str(
                "'--help','--version','--release','--target','--layout','--quiet','--verbose')",
            );
            output.push('\n');
            output.push_str("  $commands | Where-Object { $_ -like ($wordToComplete + '*') } | ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }");
            output.push('\n');
            output.push('}');
            println!("{output}");
        }
        other => fail_usage(format!(
            "unsupported shell '{other}'; expected bash, zsh, fish, or powershell"
        )),
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.chars().count()).collect();
    for (row, left_char) in left.chars().enumerate() {
        let mut current = vec![row + 1; previous.len()];
        for (column, right_char) in right.chars().enumerate() {
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + usize::from(left_char != right_char));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or_default()
}
