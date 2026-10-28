//! RFC 0011 incremental AOT benchmark and determinism oracle.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::performance::{cpu_name, percentile_index, toolchain_name};

const USAGE: &str = "usage: bench-incremental [--corpus small|medium|large|all] [--samples N] [--warmup N] [--release] [--daemon] [--verify-determinism]";

/// Base of the constants that mutations edit; see [`Workload`].
const SALT: usize = 1_000_000;

/// Clean builds rebuild the whole target directory, so they are sampled less.
const MAX_CLEAN_SAMPLES: usize = 5;

#[derive(Clone, Copy)]
struct Corpus {
    name: &'static str,
    modules: usize,
    functions: usize,
}

const CORPORA: [Corpus; 3] = [
    Corpus {
        name: "small",
        modules: 4,
        functions: 8,
    },
    Corpus {
        name: "medium",
        modules: 16,
        functions: 32,
    },
    Corpus {
        name: "large",
        modules: 64,
        functions: 64,
    },
];

struct Options {
    corpora: Vec<Corpus>,
    samples: usize,
    warmup: usize,
    release: bool,
    /// Measure builds through the project daemon, end to end from the client
    /// (the HOT.6 gate), instead of a new compiler process per build.
    daemon: bool,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            corpora: vec![CORPORA[0]],
            samples: 5,
            warmup: 1,
            release: false,
            daemon: false,
        };
        while let Some(argument) = args.next() {
            match argument.as_str() {
                // Determinism is always verified; the flag is accepted for CI.
                "--verify-determinism" => {}
                "--release" => options.release = true,
                "--daemon" => options.daemon = true,
                "--corpus" => {
                    let value = args.next().ok_or("--corpus requires a value")?;
                    options.corpora = if value == "all" {
                        CORPORA.to_vec()
                    } else {
                        let corpus = CORPORA
                            .iter()
                            .find(|corpus| corpus.name == value)
                            .ok_or_else(|| format!("unknown corpus `{value}`"))?;
                        vec![*corpus]
                    };
                }
                "--samples" => options.samples = count(&mut args, "--samples", 1)?,
                "--warmup" => options.warmup = count(&mut args, "--warmup", 0)?,
                other => return Err(format!("unknown bench-incremental option: {other}")),
            }
        }
        Ok(options)
    }
}

fn count(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
    minimum: usize,
) -> Result<usize, String> {
    let value = args
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))?;
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{flag} expects an integer, got `{value}`"))?;
    if parsed < minimum {
        return Err(format!("{flag} must be at least {minimum}"));
    }
    Ok(parsed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mutation {
    NoopTouch,
    Documentation,
    PrivateBody,
    PrivateBodyGrowth,
    PublicSignature,
    NewItem,
    Clean,
}

impl Mutation {
    const ALL: [Self; 7] = [
        Self::NoopTouch,
        Self::Documentation,
        Self::PrivateBody,
        Self::PrivateBodyGrowth,
        Self::PublicSignature,
        Self::NewItem,
        Self::Clean,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::NoopTouch => "noop-touch",
            Self::Documentation => "documentation",
            Self::PrivateBody => "private-body",
            Self::PrivateBodyGrowth => "private-body-growth",
            Self::PublicSignature => "public-signature",
            Self::NewItem => "new-item",
            Self::Clean => "clean",
        }
    }
}

/// Generated multi-module program. Module `k` imports module `k - 1` and the
/// entry calls the last module, so every module is linked. Mutations edit the
/// first helper of `target`, in the middle of the import chain.
///
/// Every sample yields source never compiled before, so content-addressed
/// object reuse cannot hide codegen. Edited constants start at `SALT` and stay
/// above the 8-bit immediate range: a body edit keeps its machine-code size,
/// while the growth class changes it on purpose.
struct Workload {
    corpus: Corpus,
    target: usize,
    revision: usize,
    body_salt: usize,
    api_salt: usize,
    growth: usize,
    wide_signature: bool,
    added: usize,
}

impl Workload {
    const fn new(corpus: Corpus) -> Self {
        Self {
            corpus,
            target: corpus.modules / 2,
            revision: 0,
            body_salt: 0,
            api_salt: 0,
            growth: 0,
            wide_signature: false,
            added: 0,
        }
    }

    /// Applies sample `index` of `mutation` and returns the modules whose text
    /// must be rewritten. Each sample produces text no earlier sample had,
    /// except for the no-op touch, which rewrites identical bytes.
    fn mutate(&mut self, mutation: Mutation, index: usize) -> Vec<usize> {
        match mutation {
            Mutation::NoopTouch => {}
            Mutation::Documentation => self.revision = index + 1,
            Mutation::PrivateBody => self.body_salt = index + 1,
            Mutation::PrivateBodyGrowth => self.growth = index + 1,
            Mutation::PublicSignature => {
                self.wide_signature = !self.wide_signature;
                self.api_salt = index + 1;
                return vec![self.target, self.target + 1];
            }
            Mutation::NewItem => self.added = index + 1,
            Mutation::Clean => return Vec::new(),
        }
        vec![self.target]
    }

    fn main_source(&self) -> String {
        let last = self.corpus.modules - 1;
        format!(
            "module workload\nimport workload.m{last} as last\n\nfunc main(): int {{\n    return last.api(1)\n}}\n"
        )
    }

    fn module_source(&self, module: usize) -> String {
        let edited = module == self.target;
        let mut source = String::new();
        if module > 0 {
            source.push_str(&format!("import workload.m{} as prev\n\n", module - 1));
        }
        for function in 0..self.corpus.functions {
            let hot = edited && function == 0;
            let revision = if hot && self.revision > 0 {
                format!(" Revision {}.", self.revision)
            } else {
                String::new()
            };
            let step = if hot { SALT + self.body_salt } else { function };
            source.push_str(&format!(
                "/// Helper {function} of module {module}.{revision}\nfunc helper_{function}(value: int): int {{\n    let mut acc = value\n    let mut i = 0\n    while i < 3 {{\n        acc = acc + i + {step}\n        i = i + 1\n    }}\n"
            ));
            if hot {
                for extra in 1..=self.growth {
                    source.push_str(&format!("    acc = acc + {}\n", SALT + extra));
                }
            }
            source.push_str("    return acc\n}\n\n");
        }
        if edited {
            for item in 1..=self.added {
                source.push_str(&format!(
                    "public func added_{item}(): int {{\n    return {item}\n}}\n\n"
                ));
            }
        }
        let wide = edited && self.wide_signature;
        let parameters = if wide {
            "value: int, extra: int"
        } else {
            "value: int"
        };
        let mut terms = (0..self.corpus.functions)
            .map(|function| format!("helper_{function}(value)"))
            .collect::<Vec<_>>();
        if module > 0 {
            let previous = module - 1;
            let arguments = if previous == self.target && self.wide_signature {
                "value, 0"
            } else {
                "value"
            };
            terms.push(format!("prev.api({arguments})"));
        }
        if edited {
            terms.extend((1..=self.added).map(|item| format!("added_{item}()")));
            terms.push((SALT + self.api_salt).to_string());
        }
        if wide {
            terms.push("extra".to_owned());
        }
        source.push_str(&format!(
            "public func api({parameters}): int {{\n    return {}\n}}\n",
            terms.join(" + ")
        ));
        source
    }

    fn write_module(&self, project: &Path, module: usize) -> Result<(), String> {
        let path = project.join(format!("src/m{module}.aru"));
        fs::write(&path, self.module_source(module))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))
    }

    fn write_all(&self, project: &Path) -> Result<(), String> {
        let main = project.join("src/main.aru");
        fs::write(&main, self.main_source())
            .map_err(|error| format!("cannot write {}: {error}", main.display()))?;
        (0..self.corpus.modules).try_for_each(|module| self.write_module(project, module))
    }
}

pub fn run(root: &Path, args: impl Iterator<Item = String>) -> i32 {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error}\n{USAGE}");
            return 2;
        }
    };
    match execute(root, &options) {
        Ok(deterministic) => i32::from(!deterministic),
        Err(error) => {
            eprintln!("bench-incremental: {error}");
            1
        }
    }
}

fn execute(root: &Path, options: &Options) -> Result<bool, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let profile = if options.release { "release" } else { "debug" };
    let release_flag = options.release.then_some("--release");
    let build = Command::new(&cargo)
        .current_dir(root)
        .args(["build", "--locked", "-p", "arandu_cli"])
        .args(release_flag)
        .output()
        .map_err(|error| format!("cannot build benchmark compiler: {error}"))?;
    ensure_success("build benchmark compiler", &build)?;
    let runtime_build = Command::new(&cargo)
        .current_dir(root)
        .args(["rustc", "--locked", "-p", "arandu_runtime"])
        .args(release_flag)
        .args(["--crate-type", "staticlib"])
        .output()
        .map_err(|error| format!("cannot build benchmark runtime: {error}"))?;
    ensure_success("build benchmark runtime", &runtime_build)?;

    let output_dir = root.join("target").join(profile);
    let cli = output_dir.join(if cfg!(windows) {
        "arandu_cli.exe"
    } else {
        "arandu_cli"
    });
    if !cli.is_file() {
        return Err(format!(
            "compiler executable not found at {}",
            cli.display()
        ));
    }
    let runtime = output_dir.join(if cfg!(windows) {
        "arandu_runtime.lib"
    } else {
        "libarandu_runtime.a"
    });
    if !runtime.is_file() {
        return Err(format!("AOT runtime not found at {}", runtime.display()));
    }

    let work = unique_work_dir(&root.join("target/bench-incremental"))?;
    let sockets = work.join("run");
    let daemon = if options.daemon {
        private_dir(&sockets)?;
        Some(sockets.as_path())
    } else {
        None
    };
    let clean_samples = options.samples.min(MAX_CLEAN_SAMPLES);
    let mut results = Vec::new();
    for corpus in &options.corpora {
        let parent = work.join(corpus.name);
        fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
        run_cli(
            &cli,
            &runtime,
            &parent,
            &["new", "workload", "--bin", "--vcs=none"],
            None,
        )?;
        let project = parent.join("workload");
        for mutation in Mutation::ALL {
            let (warmup, wanted) = if mutation == Mutation::Clean {
                (0, clean_samples)
            } else {
                (options.warmup, options.samples)
            };
            let mut workload = Workload::new(*corpus);
            workload.write_all(&project)?;
            run_cli_in(&cli, &runtime, &project, &["build", "-v"], None, daemon)?;
            let mut samples = Vec::with_capacity(wanted);
            for index in 0..warmup + wanted {
                if mutation == Mutation::Clean {
                    let target = project.join("target");
                    fs::remove_dir_all(&target)
                        .map_err(|error| format!("cannot remove {}: {error}", target.display()))?;
                }
                for module in workload.mutate(mutation, index) {
                    workload.write_module(&project, module)?;
                }
                let sample = measure(&cli, &runtime, &project, mutation, daemon)?;
                if index >= warmup {
                    samples.push(sample);
                }
            }
            let summary = summarize(&samples);
            println!(
                "bench-incremental: {}/{}: p50 {:.1} ms, p95 {:.1} ms",
                corpus.name,
                mutation.name(),
                summary["wall_ms"]["p50"].as_f64().unwrap_or_default(),
                summary["wall_ms"]["p95"].as_f64().unwrap_or_default(),
            );
            results.push(json!({
                "corpus": corpus.name,
                "modules": corpus.modules,
                "functions_per_module": corpus.functions,
                "mutation": mutation.name(),
                "summary": summary,
                "samples": samples.iter().map(Sample::to_json).collect::<Vec<_>>(),
            }));
        }
        if daemon.is_some() {
            run_cli_in(&cli, &runtime, &project, &["__daemon-stop"], None, daemon)?;
        }
    }

    let determinism = verify_determinism(&cli, &runtime, &work)?;
    let report_dir = root.join("target/benchmarks");
    fs::create_dir_all(&report_dir)
        .map_err(|error| format!("cannot create report directory: {error}"))?;
    let report = json!({
        "schema": 3,
        "workload": "rfc-0011-generated-corpus",
        "configuration": {
            "compiler_profile": profile,
            "samples": options.samples,
            "warmup": options.warmup,
            "clean_samples": clean_samples,
            "time_passes": !options.daemon,
            "daemon": options.daemon,
            "edited_module": "middle of the import chain",
        },
        "host": host(root, &work),
        "results": results,
        "determinism": determinism,
    });
    let mut json_bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("cannot serialize JSON report: {error}"))?;
    json_bytes.push(b'\n');
    fs::write(report_dir.join("incremental.json"), json_bytes)
        .map_err(|error| format!("cannot write JSON report: {error}"))?;
    fs::write(report_dir.join("incremental.md"), markdown_report(&report))
        .map_err(|error| format!("cannot write Markdown report: {error}"))?;

    println!(
        "bench-incremental: reports written to {}",
        report_dir.display()
    );
    println!(
        "bench-incremental: deterministic across paths and RAYON_NUM_THREADS=1/16: {}",
        determinism["equal"].as_bool().unwrap_or(false)
    );
    let deterministic = determinism["equal"].as_bool().unwrap_or(false);
    fs::remove_dir_all(&work)
        .map_err(|error| format!("cannot remove benchmark work directory: {error}"))?;
    Ok(deterministic)
}

struct Sample {
    wall_ms: f64,
    usage: Option<Usage>,
    path: &'static str,
    patch_fallback: bool,
    queries: u64,
    cgu_cached: u64,
    cgu_recompiled: u64,
    artifact_bytes: u64,
    phase_ms: BTreeMap<String, f64>,
    sha256: String,
}

impl Sample {
    fn to_json(&self) -> Value {
        json!({
            "wall_ms": self.wall_ms,
            "cpu_ms": self.usage.as_ref().map(|usage| usage.cpu_ms),
            "max_rss_kib": self.usage.as_ref().map(|usage| usage.max_rss_kib),
            "path": self.path,
            "patch_fallback": self.patch_fallback,
            "queries_reexecuted": self.queries,
            "cgu_cached": self.cgu_cached,
            "cgu_recompiled": self.cgu_recompiled,
            "artifact_bytes": self.artifact_bytes,
            "phase_ms": self.phase_ms,
            "sha256": self.sha256,
        })
    }
}

fn measure(
    cli: &Path,
    runtime: &Path,
    project: &Path,
    mutation: Mutation,
    daemon: Option<&Path>,
) -> Result<Sample, String> {
    // `-Z` switches are process-wide and `-v` records every query event, so
    // a daemon build is timed as a user runs it; `require` fails rather than
    // timing a batch build.
    let args: &[&str] = if daemon.is_some() {
        &["build"]
    } else {
        &["build", "-v", "-Ztime-passes"]
    };
    let measured = run_measured(cli_command(cli, runtime, project, args, None, daemon))
        .map_err(|error| format!("cannot run {}: {error}", cli.display()))?;
    ensure_success(
        &format!("{} {}", cli.display(), args.join(" ")),
        &measured.output,
    )?;
    let stdout = String::from_utf8_lossy(&measured.output.stdout);
    let stderr = String::from_utf8_lossy(&measured.output.stderr);
    let (cgu_cached, cgu_recompiled) = parse_cgu(&stderr).unwrap_or((0, 0));
    let phase_ms = parse_phase_timings(&stderr);
    if daemon.is_none() {
        for required in ["project-load", "incremental-cutoff"] {
            if !phase_ms.contains_key(required) {
                return Err(format!(
                    "{} measurement did not emit required `{required}` phase timing",
                    mutation.name()
                ));
            }
        }
    }
    let artifact = current_artifact(project)?;
    let artifact_bytes = fs::metadata(&artifact)
        .map_err(|error| format!("cannot stat artifact {}: {error}", artifact.display()))?
        .len();
    Ok(Sample {
        wall_ms: measured.wall_ms,
        usage: measured.usage,
        path: publication_path(&stdout),
        patch_fallback: patch_fell_back(&stderr),
        queries: parse_queries(&stderr).unwrap_or(0),
        cgu_cached,
        cgu_recompiled,
        artifact_bytes,
        phase_ms,
        sha256: sha256_file(&artifact)?,
    })
}

/// How the build produced its artifact: unchanged inputs, a verified reuse of
/// the linked image, an in-place ELF patch, or a full link.
fn publication_path(stdout: &str) -> &'static str {
    if stdout.contains("incremental: up-to-date") {
        return "up-to-date";
    }
    match parse_backend(stdout) {
        Some("incremental-reuse") => "reuse",
        Some("in-process-elf") => "patch",
        _ => "full-link",
    }
}

fn patch_fell_back(stderr: &str) -> bool {
    stderr.contains("[in-process-elf] safety/determinism precondition not met")
        || stderr.contains("[in-process-elf] patch failed safely")
}

fn summarize(samples: &[Sample]) -> Value {
    let wall = samples
        .iter()
        .map(|sample| sample.wall_ms)
        .collect::<Vec<_>>();
    let cpu = samples
        .iter()
        .filter_map(|sample| sample.usage.as_ref().map(|usage| usage.cpu_ms))
        .collect::<Vec<_>>();
    let max_rss_kib = samples
        .iter()
        .filter_map(|sample| sample.usage.as_ref().map(|usage| usage.max_rss_kib))
        .max();
    let mut paths = BTreeMap::<&str, usize>::new();
    let mut phases = BTreeMap::<&str, Vec<f64>>::new();
    for sample in samples {
        *paths.entry(sample.path).or_default() += 1;
        for (phase, milliseconds) in &sample.phase_ms {
            phases.entry(phase).or_default().push(*milliseconds);
        }
    }
    let patched = paths.get("patch").copied().unwrap_or_default();
    let linked = patched + paths.get("full-link").copied().unwrap_or_default();
    let patch_rate = (linked > 0).then(|| patched as f64 / linked as f64);
    let phase_p50 = phases
        .into_iter()
        .map(|(phase, values)| (phase, distribution(&values)["p50"].clone()))
        .collect::<BTreeMap<_, _>>();
    json!({
        "samples": samples.len(),
        "wall_ms": distribution(&wall),
        "cpu_ms": (!cpu.is_empty()).then(|| distribution(&cpu)),
        "max_rss_kib": max_rss_kib,
        "paths": paths,
        "patch_rate": patch_rate,
        "patch_fallbacks": samples.iter().filter(|sample| sample.patch_fallback).count(),
        "cgu_recompiled_max": samples.iter().map(|sample| sample.cgu_recompiled).max(),
        "artifact_bytes_max": samples.iter().map(|sample| sample.artifact_bytes).max(),
        "phase_ms_p50": phase_p50,
    })
}

/// Nearest-rank percentiles, the same method as `check-project-performance`.
fn distribution(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |percentile| {
        sorted
            .get(percentile_index(sorted.len(), percentile))
            .copied()
            .unwrap_or_default()
    };
    let mean = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    json!({
        "p50": at(50),
        "p95": at(95),
        "max": sorted.last().copied().unwrap_or_default(),
        "mean": mean,
    })
}

fn host(root: &Path, work: &Path) -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "kernel": fs::read_to_string("/proc/sys/kernel/osrelease")
            .map_or_else(|_| "unreported".to_owned(), |release| release.trim().to_owned()),
        "cpu": cpu_model(),
        "logical_cpus": std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        "filesystem": filesystem_type(work),
        "rust_toolchain": toolchain_name(root),
        "commit": commit(root),
    })
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == "model name").then(|| value.trim().to_owned())
            })
        })
        .unwrap_or_else(cpu_name)
}

/// Filesystem type of the longest mount point containing `path` (Linux).
fn filesystem_type(path: &Path) -> String {
    let (Ok(path), Ok(mounts)) = (path.canonicalize(), fs::read_to_string("/proc/self/mounts"))
    else {
        return "unreported".to_owned();
    };
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mount = fields.next()?.replace("\\040", " ");
            Some((mount, fields.next()?))
        })
        .filter(|(mount, _)| path.starts_with(mount))
        .max_by_key(|(mount, _)| mount.len())
        .map_or_else(|| "unreported".to_owned(), |(_, kind)| kind.to_owned())
}

fn commit(root: &Path) -> String {
    if let Ok(sha) = std::env::var("GITHUB_SHA") {
        return sha;
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(head) = git(&["rev-parse", "HEAD"]) else {
        return "unreported".to_owned();
    };
    match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(status) if status.is_empty() => head,
        _ => format!("{head}-dirty"),
    }
}

fn verify_determinism(cli: &Path, runtime: &Path, work: &Path) -> Result<Value, String> {
    let first_parent = work.join("determinism-a");
    let second_parent = work.join("a-much-longer-determinism-location");
    fs::create_dir_all(&first_parent).map_err(|error| error.to_string())?;
    fs::create_dir_all(&second_parent).map_err(|error| error.to_string())?;
    for parent in [&first_parent, &second_parent] {
        run_cli(
            cli,
            runtime,
            parent,
            &["new", "deterministic", "--bin", "--vcs=none"],
            None,
        )?;
        fs::write(
            parent.join("deterministic/src/main.aru"),
            "func helper(): int { return 7; }\nfunc main(): int { return helper(); }\n",
        )
        .map_err(|error| error.to_string())?;
    }
    let first = first_parent.join("deterministic");
    let second = second_parent.join("deterministic");
    run_cli(cli, runtime, &first, &["build", "-v"], Some("1"))?;
    run_cli(cli, runtime, &second, &["build", "-v"], Some("16"))?;
    let first_digest = sha256_file(&current_artifact(&first)?)?;
    let second_digest = sha256_file(&current_artifact(&second)?)?;
    let equal = first_digest == second_digest;
    Ok(json!({
        "threads_1_sha256": first_digest,
        "threads_16_sha256": second_digest,
        "equal": equal,
    }))
}

/// `daemon` is the private runtime directory of the daemon measurement;
/// without it every build is a new compiler process.
fn cli_command(
    cli: &Path,
    runtime: &Path,
    directory: &Path,
    args: &[&str],
    rayon_threads: Option<&str>,
    daemon: Option<&Path>,
) -> Command {
    let mut command = Command::new(cli);
    command
        .current_dir(directory)
        .env("ARANDU_RUNTIME_LIB", runtime)
        .args(args);
    match daemon {
        Some(sockets) => {
            command
                .env("ARANDU_DAEMON", "require")
                .env("XDG_RUNTIME_DIR", sockets)
                .env_remove("CI");
        }
        None => {
            command.env("ARANDU_DAEMON", "0");
        }
    }
    if let Some(threads) = rayon_threads {
        command.env("RAYON_NUM_THREADS", threads);
    }
    command
}

/// The build daemon runs on Unix only; its sockets need a directory only
/// this user can enter.
#[cfg(unix)]
fn private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn private_dir(_path: &Path) -> Result<(), String> {
    Err("--daemon measures the Unix build daemon".to_owned())
}

fn run_cli(
    cli: &Path,
    runtime: &Path,
    directory: &Path,
    args: &[&str],
    rayon_threads: Option<&str>,
) -> Result<Output, String> {
    run_cli_in(cli, runtime, directory, args, rayon_threads, None)
}

fn run_cli_in(
    cli: &Path,
    runtime: &Path,
    directory: &Path,
    args: &[&str],
    rayon_threads: Option<&str>,
    daemon: Option<&Path>,
) -> Result<Output, String> {
    let output = cli_command(cli, runtime, directory, args, rayon_threads, daemon)
        .output()
        .map_err(|error| format!("cannot run {}: {error}", cli.display()))?;
    ensure_success(&format!("{} {}", cli.display(), args.join(" ")), &output)?;
    Ok(output)
}

struct Usage {
    cpu_ms: f64,
    max_rss_kib: u64,
}

struct Measured {
    output: Output,
    wall_ms: f64,
    usage: Option<Usage>,
}

/// Runs `command` to completion, reporting wall time and, on Unix, the CPU
/// time and peak RSS of the compiler process tree it reaped.
fn run_measured(mut command: Command) -> Result<Measured, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let (status, usage) = wait_with_usage(child)?;
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    Ok(Measured {
        output: Output {
            status,
            stdout: collect(stdout)?,
            stderr: collect(stderr)?,
        },
        wall_ms,
        usage,
    })
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes)?;
        }
        Ok(bytes)
    })
}

fn collect(reader: JoinHandle<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| "compiler output reader panicked".to_owned())?
        .map_err(|error| format!("cannot read compiler output: {error}"))
}

#[cfg(unix)]
fn wait_with_usage(child: Child) -> Result<(ExitStatus, Option<Usage>), String> {
    use std::os::unix::process::ExitStatusExt as _;

    let pid = libc::pid_t::try_from(child.id())
        .map_err(|error| format!("compiler pid out of range: {error}"))?;
    let mut status = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    loop {
        // SAFETY: `pid` names a child of this process that nothing else reaps
        // (`child` is never waited on), and both out-pointers reference live,
        // writable values of the types `wait4` expects.
        let reaped = unsafe { libc::wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
        if reaped == pid {
            break;
        }
        let error = std::io::Error::last_os_error();
        if reaped == -1 && error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(format!("cannot wait for compiler process: {error}"));
    }
    // SAFETY: the buffer was zero-initialized, which is a valid `rusage`, and
    // the successful `wait4` above filled it.
    let usage = unsafe { usage.assume_init() };
    let milliseconds =
        |time: libc::timeval| time.tv_sec as f64 * 1_000.0 + time.tv_usec as f64 / 1_000.0;
    let max_rss = u64::try_from(usage.ru_maxrss).unwrap_or_default();
    Ok((
        ExitStatus::from_raw(status),
        Some(Usage {
            cpu_ms: milliseconds(usage.ru_utime) + milliseconds(usage.ru_stime),
            // Linux reports KiB; macOS reports bytes.
            max_rss_kib: if cfg!(target_os = "macos") {
                max_rss / 1024
            } else {
                max_rss
            },
        }),
    ))
}

#[cfg(not(unix))]
fn wait_with_usage(mut child: Child) -> Result<(ExitStatus, Option<Usage>), String> {
    child
        .wait()
        .map(|status| (status, None))
        .map_err(|error| format!("cannot wait for compiler process: {error}"))
}

fn ensure_success(operation: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{operation} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn current_artifact(project: &Path) -> Result<PathBuf, String> {
    let state_path = find_named(&project.join("target/dev"), OsStr::new("build-state.json"))?
        .ok_or_else(|| "build-state.json not found".to_owned())?;
    let state: Value = serde_json::from_slice(
        &fs::read(&state_path).map_err(|error| format!("cannot read build state: {error}"))?,
    )
    .map_err(|error| format!("cannot parse build state: {error}"))?;
    let relative = state["artifact"]
        .as_str()
        .ok_or_else(|| "build state has no artifact path".to_owned())?;
    Ok(state_path.parent().unwrap_or(project).join(relative))
}

fn find_named(root: &Path, name: &OsStr) -> Result<Option<PathBuf>, String> {
    if !root.is_dir() {
        return Ok(None);
    }
    let mut pending = vec![root.to_path_buf()];
    let mut found = None;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("cannot scan {}: {error}", directory.display()))?
        {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name() == Some(name) {
                if found.is_some() {
                    return Err(format!("multiple {} files found", name.to_string_lossy()));
                }
                found = Some(path);
            }
        }
    }
    Ok(found)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot hash artifact {}: {error}", path.display()))?;
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    Ok(encoded)
}

fn parse_queries(stderr: &str) -> Option<u64> {
    let line = stderr.lines().find(|line| line.starts_with("[rebuilt: "))?;
    line.strip_prefix("[rebuilt: ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn parse_cgu(stderr: &str) -> Option<(u64, u64)> {
    let line = stderr.lines().find(|line| line.starts_with("[cgu] "))?;
    let (_, stats) = line.split_once("units: ")?;
    let mut parts = stats.split(',');
    let cached = parts.next()?.split_whitespace().next()?.parse().ok()?;
    let recompiled = parts.next()?.split_whitespace().next()?.parse().ok()?;
    Some((cached, recompiled))
}

fn parse_backend(stdout: &str) -> Option<&str> {
    let (_, rest) = stdout.split_once("backend=")?;
    rest.split([',', ')']).next()
}

fn parse_phase_timings(stderr: &str) -> BTreeMap<String, f64> {
    let mut timings = BTreeMap::new();
    for line in stderr.lines() {
        let Some((_, payload)) = line.split_once("[arandu][perf]") else {
            continue;
        };
        let mut fields = payload.split_whitespace();
        let (Some(phase), Some(duration)) = (fields.next(), fields.next()) else {
            continue;
        };
        if fields.next().is_some() {
            continue;
        }
        let Some(milliseconds) = duration.strip_suffix("ms") else {
            continue;
        };
        let Ok(milliseconds) = milliseconds.parse::<f64>() else {
            continue;
        };
        if !milliseconds.is_finite() || milliseconds < 0.0 {
            continue;
        }
        *timings.entry(phase.to_owned()).or_insert(0.0) += milliseconds;
    }
    timings
}

fn markdown_report(report: &Value) -> String {
    let configuration = &report["configuration"];
    let host = &report["host"];
    let text = |value: &Value| value.as_str().unwrap_or("?").to_owned();
    let mut output = format!(
        "# RFC 0011 incremental benchmark\n\nCompiler profile `{}`; {} samples per class ({} for clean) after {} warm-up build(s); `-Ztime-passes` enabled.\n\nHost: {}/{}, kernel {}, {} ({} logical CPUs), filesystem {}, toolchain {}, commit `{}`.\n\n",
        text(&configuration["compiler_profile"]),
        configuration["samples"],
        configuration["clean_samples"],
        configuration["warmup"],
        text(&host["os"]),
        text(&host["arch"]),
        text(&host["kernel"]),
        text(&host["cpu"]),
        host["logical_cpus"],
        text(&host["filesystem"]),
        text(&host["rust_toolchain"]),
        text(&host["commit"]),
    );
    output.push_str("| Corpus | Mutation | n | p50 ms | p95 ms | max ms | CPU p50 ms | RSS max MiB | CGU rebuilt max | Paths | Patch fallbacks |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: |\n");
    let results = report["results"].as_array().map_or(&[][..], Vec::as_slice);
    for result in results {
        let summary = &result["summary"];
        let paths = summary["paths"]
            .as_object()
            .map_or_else(String::new, |paths| {
                paths
                    .iter()
                    .map(|(path, count)| format!("{path}: {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        let cpu = summary["cpu_ms"]["p50"]
            .as_f64()
            .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.1}"));
        let rss = summary["max_rss_kib"].as_u64().map_or_else(
            || "n/a".to_owned(),
            |kib| format!("{:.1}", kib as f64 / 1024.0),
        );
        output.push_str(&format!(
            "| {} | {} | {} | {:.1} | {:.1} | {:.1} | {cpu} | {rss} | {} | {paths} | {} |\n",
            text(&result["corpus"]),
            text(&result["mutation"]),
            summary["samples"],
            summary["wall_ms"]["p50"].as_f64().unwrap_or_default(),
            summary["wall_ms"]["p95"].as_f64().unwrap_or_default(),
            summary["wall_ms"]["max"].as_f64().unwrap_or_default(),
            summary["cgu_recompiled_max"],
            summary["patch_fallbacks"],
        ));
    }
    output.push_str(&format!(
        "\nDeterministic across different paths and `RAYON_NUM_THREADS=1/16`: **{}**.\n",
        report["determinism"]["equal"].as_bool().unwrap_or(false)
    ));
    output.push_str(
        "\n## Phase timings (p50)\n\n| Corpus | Mutation | Phase | ms |\n| --- | --- | --- | ---: |\n",
    );
    for result in results {
        if let Some(phases) = result["summary"]["phase_ms_p50"].as_object() {
            for (phase, milliseconds) in phases {
                output.push_str(&format!(
                    "| {} | {} | {phase} | {:.3} |\n",
                    text(&result["corpus"]),
                    text(&result["mutation"]),
                    milliseconds.as_f64().unwrap_or_default(),
                ));
            }
        }
    }
    output
}

fn unique_work_dir(parent: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_nanos();
    let directory = parent.join(format!("work-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).map_err(|error| {
        format!(
            "cannot reserve benchmark directory {}: {error}",
            directory.display()
        )
    })?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_and_accumulates_non_colored_phase_timings() {
        let stderr = "\
[12:34:56] [arandu][perf] project-load                        4.250ms\n\
unrelated output\n\
[12:34:56] [arandu][perf] lower-amir                         10.500ms\n\
[12:34:57] [arandu][perf] project-load                        0.750ms\n";

        let timings = parse_phase_timings(stderr);
        assert_eq!(timings.len(), 2);
        assert_eq!(timings.get("project-load"), Some(&5.0));
        assert_eq!(timings.get("lower-amir"), Some(&10.5));
    }

    #[test]
    fn ignores_malformed_or_non_finite_phase_timings() {
        let stderr = "\
[12:34:56] [arandu][perf] project-load nope\n\
[12:34:56] [arandu][perf] project-load NaNms\n\
[12:34:56] [arandu][perf] too many fields 1.000ms\n";

        assert!(parse_phase_timings(stderr).is_empty());
    }

    #[test]
    fn options_default_to_a_cheap_small_run_and_reject_bad_counts() {
        let options = Options::parse(arguments(&["--verify-determinism"])).unwrap();
        assert_eq!(options.corpora.len(), 1);
        assert_eq!(options.corpora[0].name, "small");
        assert_eq!((options.samples, options.warmup), (5, 1));
        assert!(!options.release);

        let options = Options::parse(arguments(&[
            "--corpus",
            "all",
            "--samples",
            "50",
            "--warmup",
            "0",
            "--release",
        ]))
        .unwrap();
        assert_eq!(options.corpora.len(), 3);
        assert_eq!((options.samples, options.warmup), (50, 0));
        assert!(options.release);

        assert!(Options::parse(arguments(&["--samples", "0"])).is_err());
        assert!(Options::parse(arguments(&["--corpus", "huge"])).is_err());
        assert!(Options::parse(arguments(&["--samples"])).is_err());
    }

    #[test]
    fn every_mutation_sample_produces_unseen_source() {
        for mutation in Mutation::ALL {
            if matches!(mutation, Mutation::NoopTouch | Mutation::Clean) {
                continue;
            }
            let mut workload = Workload::new(CORPORA[0]);
            let mut seen = std::collections::BTreeSet::new();
            seen.insert(workload.module_source(workload.target));
            for index in 0..8 {
                let changed = workload.mutate(mutation, index);
                assert!(changed.contains(&workload.target));
                assert!(
                    seen.insert(workload.module_source(workload.target)),
                    "{} sample {index} revisits an earlier source",
                    mutation.name()
                );
            }
        }
    }

    #[test]
    fn body_edit_keeps_constant_width_and_new_items_are_called() {
        let mut workload = Workload::new(CORPORA[0]);
        workload.mutate(Mutation::PrivateBody, 0);
        let target = workload.target;
        assert!(workload
            .module_source(target)
            .contains(&format!("acc + i + {}", SALT + 1)));

        let mut workload = Workload::new(CORPORA[0]);
        workload.mutate(Mutation::NewItem, 1);
        let source = workload.module_source(target);
        // One definition plus one call from the module API.
        assert_eq!(source.matches(&"added_2()").count(), 2);
    }

    #[test]
    fn signature_toggle_rewrites_the_only_caller() {
        let mut workload = Workload::new(CORPORA[0]);
        let target = workload.target;
        assert_eq!(
            workload.mutate(Mutation::PublicSignature, 0),
            [target, target + 1]
        );
        assert!(workload
            .module_source(target)
            .contains("api(value: int, extra: int)"));
        assert!(workload
            .module_source(target + 1)
            .contains("prev.api(value, 0)"));
        workload.mutate(Mutation::PublicSignature, 1);
        assert!(workload
            .module_source(target + 1)
            .contains("prev.api(value)"));
    }

    #[test]
    fn classifies_publication_paths() {
        assert_eq!(
            publication_path(
                "built w (backend=cranelift-dev, artifact=x, incremental: up-to-date)"
            ),
            "up-to-date"
        );
        assert_eq!(
            publication_path("built w (backend=in-process-elf, entry=x)"),
            "patch"
        );
        assert_eq!(
            publication_path("built w (backend=incremental-reuse, entry=x)"),
            "reuse"
        );
        assert_eq!(
            publication_path("built w (backend=cranelift-dev, entry=x)"),
            "full-link"
        );
    }

    #[test]
    fn distribution_uses_nearest_rank_percentiles() {
        let values = (1..=20).rev().map(f64::from).collect::<Vec<_>>();
        let summary = distribution(&values);
        assert_eq!(summary["p50"], 10.0);
        assert_eq!(summary["p95"], 19.0);
        assert_eq!(summary["max"], 20.0);
        assert_eq!(summary["mean"], 10.5);
    }
}
