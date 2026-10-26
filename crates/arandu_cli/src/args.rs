//! CLI argument parsing, flags, layout extraction, and usage documentation.

use arandu_middle::layout::DataLayout;

use crate::cli_error::CliFailure;
use crate::pipeline::{fail_usage, finish};
use crate::project::{self, ProjectFlags};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    #[must_use]
    pub fn should_color_stream(self, is_terminal: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                let no_color = std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty());
                if no_color {
                    return false;
                }
                is_terminal
            }
        }
    }

    #[must_use]
    pub fn should_color_stderr(self) -> bool {
        use std::io::IsTerminal;
        self.should_color_stream(std::io::stderr().is_terminal())
    }

    #[must_use]
    pub fn should_color_stdout(self) -> bool {
        use std::io::IsTerminal;
        self.should_color_stream(std::io::stdout().is_terminal())
    }
}

/// Detects CLI color preferences early before the diagnostic hook is initialized.
#[must_use]
pub fn detect_color_choice(raw_args: &[String]) -> ColorChoice {
    let mut choice = ColorChoice::Auto;
    let mut i = 0;
    while i < raw_args.len() {
        let arg = &raw_args[i];
        if arg == "--" {
            break;
        }
        if arg == "--no-color" {
            choice = ColorChoice::Never;
        } else if let Some(val) = arg.strip_prefix("--color=") {
            match val {
                "always" => choice = ColorChoice::Always,
                "never" => choice = ColorChoice::Never,
                "auto" => choice = ColorChoice::Auto,
                _ => {}
            }
        } else if arg == "--color" && i + 1 < raw_args.len() {
            match raw_args[i + 1].as_str() {
                "always" => {
                    choice = ColorChoice::Always;
                    i += 1;
                }
                "never" => {
                    choice = ColorChoice::Never;
                    i += 1;
                }
                "auto" => {
                    choice = ColorChoice::Auto;
                    i += 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    choice
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunBackend {
    Cranelift,
    Llvm,
}

impl RunBackend {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "cranelift" | "jit" => Some(Self::Cranelift),
            "llvm" => Some(Self::Llvm),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CliInvocation {
    pub debug: bool,
    pub opt: bool,
    pub backend: RunBackend,
    pub parallel: bool,
    pub genref_report: bool,
    pub cfg: bool,
    pub ascii: bool,
    pub quiet: bool,
    pub watch: bool,
    pub args: Vec<String>,
    /// Arguments following `--`, forwarded verbatim to an executed program.
    pub program_args: Vec<String>,
    pub z_flags: Vec<String>,
    pub data_layout: DataLayout,
    pub project_flags: ProjectFlags,
}

pub fn parse_invocation(raw_args: impl IntoIterator<Item = String>) -> CliInvocation {
    let raw_args_vec: Vec<String> = raw_args.into_iter().collect();
    let mut debug = false;
    let mut opt = false;
    let mut backend = RunBackend::Cranelift;
    let mut parallel = false;
    let mut genref_report = false;
    let mut cfg = false;
    let mut ascii = false;
    let mut quiet = false;
    let mut watch = false;
    let mut color = ColorChoice::Auto;
    let mut args = Vec::new();
    let mut program_args = Vec::new();
    let mut z_flags: Vec<String> = Vec::new();
    let mut ctfe_budget = arandu_query::ctfe::CtfeLimits::default().budget();
    let mut layout_flags: Vec<String> = Vec::new();
    let mut raw_project_flags: Vec<String> = Vec::new();

    let mut after_separator = false;
    let mut i = 0;
    while i < raw_args_vec.len() {
        let arg = &raw_args_vec[i];
        if after_separator {
            program_args.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            after_separator = true;
            i += 1;
            continue;
        }
        match arg.as_str() {
            s if s == "--ctfe-fuel"
                || s.starts_with("--ctfe-fuel=")
                || s == "--ctfe-frames"
                || s.starts_with("--ctfe-frames=")
                || s == "--ctfe-values"
                || s.starts_with("--ctfe-values=") =>
            {
                let (name, value) = if let Some(parts) = s.split_once('=') {
                    parts
                } else {
                    i += 1;
                    let value = raw_args_vec
                        .get(i)
                        .unwrap_or_else(|| fail_usage(format!("{s} requires a positive integer")));
                    (s, value.as_str())
                };
                let value = value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or_else(|| fail_usage(format!("{name} requires a positive integer")));
                match name {
                    "--ctfe-fuel" => ctfe_budget.fuel = value,
                    "--ctfe-values" => ctfe_budget.values = value,
                    "--ctfe-frames" => {
                        ctfe_budget.frames = u32::try_from(value).unwrap_or_else(|_| {
                            fail_usage("--ctfe-frames exceeds the supported integer range")
                        })
                    }
                    _ => fail_usage("unknown CTFE limit option"),
                }
            }
            "--debug" => debug = true,
            "--opt" => opt = true,
            "-b" | "--backend" => {
                i += 1;
                let value = raw_args_vec
                    .get(i)
                    .unwrap_or_else(|| fail_usage("--backend requires a value"));
                backend = RunBackend::parse(value).unwrap_or_else(|| {
                    fail_usage(format!(
                        "unknown backend '{value}'; expected cranelift or llvm"
                    ))
                });
            }
            s if s.starts_with("--backend=") => {
                let value = &s["--backend=".len()..];
                backend = RunBackend::parse(value).unwrap_or_else(|| {
                    fail_usage(format!(
                        "unknown backend '{value}'; expected cranelift or llvm"
                    ))
                });
            }
            "--parallel" => parallel = true,
            "--genref-report" => genref_report = true,
            "--cfg" => cfg = true,
            "--ascii" => ascii = true,
            "-q" | "--quiet" => quiet = true,
            "--watch" => watch = true,
            "--no-color" => {
                color = ColorChoice::Never;
                raw_project_flags.push(arg.clone());
            }
            "--color=always" => {
                color = ColorChoice::Always;
                raw_project_flags.push(arg.clone());
            }
            "--color=never" => {
                color = ColorChoice::Never;
                raw_project_flags.push(arg.clone());
            }
            "--color=auto" => {
                color = ColorChoice::Auto;
                raw_project_flags.push(arg.clone());
            }
            "--color" => {
                i += 1;
                if i < raw_args_vec.len() {
                    match raw_args_vec[i].as_str() {
                        "always" => {
                            color = ColorChoice::Always;
                            raw_project_flags.push(format!("--color={}", raw_args_vec[i]));
                        }
                        "never" => {
                            color = ColorChoice::Never;
                            raw_project_flags.push(format!("--color={}", raw_args_vec[i]));
                        }
                        "auto" => {
                            color = ColorChoice::Auto;
                            raw_project_flags.push(format!("--color={}", raw_args_vec[i]));
                        }
                        other => fail_usage(format!(
                            "unknown --color option: '{other}' (use auto|always|never)"
                        )),
                    }
                } else {
                    fail_usage("--color requires an argument (use auto|always|never)");
                }
            }
            s if s.starts_with("--color=") => {
                fail_usage(format!(
                    "unknown --color option: '{}' (use auto|always|never)",
                    &s["--color=".len()..]
                ));
            }
            // G2: long form of -Zno-generational-fallback (same atomic).
            "--no-generational-fallback" => {
                z_flags.push("-Zno-generational-fallback".into());
            }
            s if s.starts_with("-Z") => z_flags.push(arg.clone()),
            "--layout" => {
                i += 1;
                if i < raw_args_vec.len() {
                    layout_flags.push(format!("--layout={}", raw_args_vec[i]));
                } else {
                    fail_usage("--layout requires host, ptr4, ptr8, or i686");
                }
            }
            s if s.starts_with("--layout=") => layout_flags.push(arg.clone()),
            "--stdlib-path" | "--cache-dir" => {
                raw_project_flags.push(arg.clone());
                i += 1;
                if let Some(value) = raw_args_vec.get(i).filter(|value| !value.starts_with('-')) {
                    raw_project_flags.push(value.clone());
                } else {
                    fail_usage(format!("{arg} requires a path argument"));
                }
            }
            "-o" | "--emit" | "--crate-type" | "--linker-script" => {
                raw_project_flags.push(arg.clone());
                i += 1;
                if let Some(value) = raw_args_vec.get(i).filter(|value| !value.starts_with('-')) {
                    raw_project_flags.push(value.clone());
                } else {
                    fail_usage(format!("{arg} requires a value"));
                }
            }
            s if s.starts_with("--emit=")
                || s.starts_with("--crate-type=")
                || s.starts_with("--linker-script=")
                || s.starts_with("-o=") =>
            {
                raw_project_flags.push(arg.clone());
            }
            // Collect project flags even before we know the subcommand.
            s if s.starts_with("--stdlib-path")
                || s.starts_with("--cache-dir")
                || s.starts_with("--target")
                || s == "--release"
                || s == "--size"
                || s == "-Oz"
                || s == "-v"
                || s == "--verbose"
                || s == "--locked"
                || s == "--offline"
                || s == "--frozen"
                || s == "--accept" =>
            {
                if s == "--target" {
                    raw_project_flags.push(arg.clone());
                    i += 1;
                    if let Some(value) = raw_args_vec.get(i).filter(|value| !value.starts_with('-'))
                    {
                        raw_project_flags.push(value.clone());
                    } else {
                        fail_usage("--target requires a target triple argument");
                    }
                } else {
                    raw_project_flags.push(arg.clone());
                }
            }
            _ => args.push(arg.clone()),
        }
        i += 1;
    }
    let mut data_layout = parse_data_layout(&layout_flags);
    let args = normalize_equals_flags(args);
    let (mut project_flags, extra_positional) = project::parse_project_flags(&raw_project_flags)
        .unwrap_or_else(|message| fail_usage(format!("error: {message}")));
    let _ = extra_positional;
    project_flags.ctfe_limits = arandu_query::ctfe::CtfeLimits::new(
        ctfe_budget.fuel,
        ctfe_budget.frames,
        ctfe_budget.values,
    )
    .unwrap_or_else(|message| fail_usage(message));
    project_flags.color = color;
    project_flags.quiet = quiet;
    if layout_flags.is_empty()
        && project_flags
            .target
            .as_deref()
            .is_some_and(|t| t.starts_with("wasm32"))
    {
        data_layout = DataLayout::ptr_width(4);
    }
    if let Some(triple) = project_flags.target.as_deref()
        && arandu_middle::embedded_profile(triple).is_some()
    {
        let explicit = layout_flags
            .iter()
            .find_map(|flag| flag.strip_prefix("--layout="));
        match explicit {
            None | Some("ptr4" | "32") => data_layout = DataLayout::ptr_width(4),
            Some(_) => fail_usage("bare-metal profiles use a 32-bit data layout"),
        }
    }

    CliInvocation {
        debug,
        opt,
        backend,
        parallel,
        genref_report,
        cfg,
        ascii,
        quiet,
        watch,
        args,
        program_args,
        z_flags,
        data_layout,
        project_flags,
    }
}

fn normalize_equals_flags(args: Vec<String>) -> Vec<String> {
    const VALUE_FLAGS: &[&str] = &[
        "--format",
        "--filter",
        "--exact",
        "--jobs",
        "--timeout",
        "--seed",
        "--output",
        "--warmup",
        "--measurement-time",
        "--samples",
        "--save-baseline",
        "--compare",
        "--baseline",
        "--max-regression",
        "--noise-threshold",
        "--out-dir",
    ];
    let mut normalized = Vec::with_capacity(args.len());
    for arg in args {
        if let Some((name, value)) = arg.split_once('=')
            && VALUE_FLAGS.contains(&name)
        {
            normalized.push(name.to_owned());
            normalized.push(value.to_owned());
        } else {
            normalized.push(arg);
        }
    }
    normalized
}

pub fn parse_data_layout(flags: &[String]) -> DataLayout {
    for f in flags {
        if let Some(rest) = f.strip_prefix("--layout=") {
            return match rest {
                "host" => DataLayout::host(),
                "ptr4" | "32" => DataLayout::ptr_width(4),
                "i686" | "i686-sysv" => DataLayout::i686_sysv(),
                "ptr8" | "64" => DataLayout::ptr_width(8),
                other => {
                    fail_usage(format!(
                        "unknown --layout={other} (use host|ptr4|ptr8|i686)"
                    ));
                }
            };
        }
    }
    DataLayout::host()
}

pub fn parse_benchmark_seconds(value: Option<&String>, usage: &str) -> u64 {
    let seconds = value
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 3600.0)
        .unwrap_or_else(|| fail_usage(usage));
    let nanos = std::time::Duration::from_secs_f64(seconds).as_nanos();
    u64::try_from(nanos).unwrap_or_else(|_| fail_usage(usage))
}

pub fn parse_benchmark_percentage(value: Option<&String>, usage: &str) -> f64 {
    value
        .and_then(|v| v.trim_end_matches('%').parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or_else(|| fail_usage(usage))
}

pub fn usage_and_exit() -> ! {
    finish(Err(CliFailure::usage(global_help())))
}

#[must_use]
pub fn global_help() -> &'static str {
    concat!(
        "The Arandu Programming Language Compiler\n\n",
        "usage:\n",
        "  arandu <command> [options] [package-path | file]\n",
        "  arandu_cli <command> [options] [package-path | file]\n\n",
        "Commands:\n",
        "  run        Compile and execute a package or file via Cranelift JIT\n",
        "  build      Compile a package to a native executable or library\n",
        "  check      Type-check and validate a package or source file\n",
        "  test       Execute unit and integration test suites\n",
        "  bench      Run benchmarks and compare baseline metrics\n",
        "  doc        Generate package documentation\n\n",
        "Project & Dependency Management:\n",
        "  new        Create a new Arandu project\n",
        "  init       Initialize an Arandu package in the current directory\n",
        "  tree       Display the resolved dependency graph\n",
        "  audit      Audit locked provenance and security policies\n",
        "  vendor     Create a verified offline source snapshot\n",
        "  verify     Verify offline cache integrity\n",
        "  update     Review and publish a dependency graph update\n",
        "  watch      Watch files and check the package incrementally\n",
        "  clean      Remove project build artifacts and scratch cache\n\n",
        "Developer Tools:\n",
        "  fmt        Format one or more source files or directories\n",
        "  completions Generate shell completion definitions\n",
        "  doctor     Inspect compiler toolchain and environment\n",
        "  cache      Inspect, prune, and verify compiler caches\n\n",
        "Compiler Inspection (advanced):\n",
        "  lex, parse, hir, amir, graph, emit-c, emit-llvm, emit-wasm, emit-component\n",
        "  hash-file  Compute a BLAKE3 checksum for packaging\n",
        "  archive    Validate a package archive\n\n",
        "Target & Toolchain Options:\n",
        "  --release                  Build with speed optimizations (Cranelift + AMIR O2)\n",
        "  --size, -Oz                Build for size (AMIR Os, stripped native output)\n",
        "  --stdlib-path <dir>        Override path to standard library\n",
        "  --cache-dir <dir>          Override compiler cache directory\n",
        "  --ctfe-fuel N / --ctfe-frames N / --ctfe-values N\n",
        "                             compile-time evaluation ceilings (positive integers)\n",
        "  --layout=host|ptr4|ptr8|i686  (default: host)\n",
        "                             layout model only; cross compiler/sysroot are external\n",
        "  --color=auto|always|never  Control ANSI color output (default: auto)\n",
        "  --no-color                 Disable ANSI color output (respects https://no-color.org)\n",
        "  --vcs=auto|git|none        VCS initialization mode for new projects\n",
        "  -v, --verbose              Enable detailed progress and timing logs\n",
        "  -q, --quiet                Suppress non-error status and progress output\n",
        "      --watch                Repeat package checks after source changes (check only)\n",
        "  -V, --version              Print compiler version and exit\n",
        "  -h, --help                 Print this help message (or command-specific help)\n\n",
        "Generational Memory Safety (GenRef):\n",
        "  --no-generational-fallback Reject runtime generational promotion (promote O004 to error)\n",
        "  --genref-report            Print per-module/function promotion and check counts on stderr\n\n",
        "Developer & Unstable Debug Flags (-Z):\n",
        "  -Ztime-passes              Display execution timings for compilation passes\n",
        "  -Zprofile-queries          Profile Salsa incremental semantic query costs\n",
        "  -Zprint-alloc-stats        Print scratch arena allocation statistics\n",
        "  -Zdump-mir                 Dump intermediate AMIR between optimization passes\n",
        "  -Zdebug-parser             Trace Rowan CST parsing steps\n",
        "  -Zdebug-typeck             Trace bidirectional type inference & constraints\n",
        "  -Zdebug-ossa               Trace ownership SSA generation and joins\n",
        "  -Zdebug-layout             Trace memory layout computation\n",
        "  -Zdebug-backend            Trace backend machine code generation\n",
        "  -Zdebug-all                Enable all compiler debug traces\n",
        "  -Zself-profile=<path>      Record detailed execution profile\n",
        "  -Zexplain-rebuild          Explain reason for Salsa incremental rebuild\n",
        "  -Zno-generational-fallback Synonym for --no-generational-fallback\n\n",
        "Environment & Defaults:\n",
        "  backend: build → Cranelift baseline; build --release → Cranelift speed + AMIR O2; --size → speed_and_size + AMIR Os\n",
        "  stdlib:  --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)\n",
        "  cache:   --cache-dir > ARANDU_CACHE_DIR > platform-native user cache"
    )
}

/// Help text for commands with distinct option surfaces.
#[must_use]
pub fn command_help(command: &str) -> Option<&'static str> {
    match command {
        "run" => Some(
            "Usage: arandu run [path] [-- program-args...]\n\nCompile and execute a package or .aru file. Cranelift JIT is the default; LLVM\nIR execution is experimental and requires `lli` or `clang` on PATH.\n\nOptions:\n  --opt             Optimize AMIR before execution\n  -b, --backend B   Select cranelift (default) or llvm\n  --stdlib-path DIR Override the standard library path\n  -v, --verbose     Show progress and incremental status\n  -q, --quiet       Suppress non-error status output\n\nExamples:\n  arandu run\n  arandu run -b llvm src/main.aru -- hello\n",
        ),
        "build" => Some(
            "Usage: arandu build [path] [options]\n\nCompile a package to a native executable, or to a freestanding object.\n\nOptions:\n  --release            Build with speed optimizations\n  -b, --backend B      Select cranelift (default) or llvm (release)\n  --size, -Oz          Optimize for size and strip native symbols\n  --target TRIPLE      Host, wasm32, thumbv6m-none-eabi, thumbv7em-none-eabihf, or riscv32imc-unknown-none-elf\n  --emit obj|llvm-ir|header  Write an object, LLVM IR, or a C header instead of linking\n  --crate-type staticlib  Archive a bare-metal object (`llvm-ar` or `ar`)\n  --linker-script FILE Link an ELF only with this user-supplied script\n  -o FILE              Write the selected artifact to FILE\n  --layout LAYOUT      Select host, ptr4, ptr8, or i686 layout\n  --locked             Require the lockfile to be current\n  --offline            Resolve from the local cache only\n  -v, --verbose        Show detailed build progress\n  -q, --quiet          Suppress non-error status output\n\nBare-metal LLVM builds publish a relocatable object. The compiler does not\ninvent Flash, RAM, or a vector table. A final ELF requires --linker-script.\n",
        ),
        "check" => Some(
            "Usage: arandu check [path] [options]\n\nType-check a package or source file without code generation.\n\nOptions:\n  --parallel       Check source files in parallel\n  --target TRIPLE  Select a compilation target\n  --watch          Repeat checks after source changes\n  -v, --verbose    Show progress and incremental status\n  -q, --quiet      Suppress success and status output\n",
        ),
        "test" => Some(
            "Usage: arandu test [package-path] [options]\n\nOptions:\n  --list                  List tests without running them\n  --doc                   Include documentation tests\n  --filter TEXT           Select tests containing this literal text\n  --exact ID              Select one canonical test id\n  --jobs N                Run up to N tests concurrently\n  --timeout SECONDS       Set the per-test timeout\n  --seed N                Set the deterministic test seed\n  --format FORMAT         human, json, or junit\n  --output PATH           Write structured output to a file\n  --fail-fast             Stop after the first failure\n\nExamples:\n  arandu test\n  arandu test --filter parser\n  arandu test --format=json --output results.json\n",
        ),
        "bench" => Some(
            "Usage: arandu bench [package-path] [options]\n\nOptions:\n  --list                    List benchmarks\n  --filter TEXT             Select benchmarks by literal substring\n  --exact ID                Select a canonical benchmark id\n  --warmup SECONDS          Set warmup duration\n  --measurement-time SEC    Set measurement duration\n  --samples N               Set sample count (10..10000)\n  --format FORMAT           human or json\n  --save-baseline NAME      Save a named baseline\n  --compare NAME            Compare with a named baseline\n  --strict                  Fail on policy regressions (requires --compare)\n",
        ),
        "doc" => Some(
            "Usage: arandu doc [path] [options]\n\nOptions:\n  --format FORMAT  html, json, or md (also accepts --format=FORMAT)\n  --out-dir DIR    Output directory\n  --open           Open the generated HTML documentation\n\nExamples:\n  arandu doc\n  arandu doc stdlib --format json --out-dir docs\n",
        ),
        "fmt" => Some(
            "Usage: arandu fmt [--check] <file-or-directory>...\n\nFormat one or more .aru files. Directories are searched recursively.\nBy default files are updated in place; --check reports differences without writing.\n\nExamples:\n  arandu fmt src/main.aru\n  arandu fmt src/ tests/\n  arandu fmt --check src/main.aru src/lib.aru\n",
        ),
        "completions" => Some(
            "Usage: arandu completions <bash|zsh|fish|powershell>\n\nPrint shell completion definitions to standard output.\n\nExamples:\n  arandu completions zsh > _arandu\n  arandu completions bash > arandu.bash\n",
        ),
        "new" => Some(
            "Usage: arandu new <name> [--bin|--lib] [--vcs=auto|git|none]\n\nCreate a new Arandu package directory.\n",
        ),
        "init" => Some(
            "Usage: arandu init [path] [--bin|--lib]\n\nInitialize a package in the current directory or at path.\n",
        ),
        "watch" => Some(
            "Usage: arandu watch [path]\n\nWatch package source files and run incremental checks after edits.\n",
        ),
        "clean" => Some(
            "Usage: arandu clean [path]\n\nRemove generated project build artifacts and scratch cache.\n",
        ),
        "tree" => Some(
            "Usage: arandu tree [path] [--locked] [--offline]\n\nDisplay the canonical resolved dependency graph.\n",
        ),
        "audit" => Some(
            "Usage: arandu audit [path]\n\nAudit locked package provenance and security policies.\n",
        ),
        "vendor" => Some(
            "Usage: arandu vendor [path]\n\nCreate a verified offline snapshot of the locked source graph.\n",
        ),
        "verify" => Some(
            "Usage: arandu verify [path]\n\nVerify the local package cache and lockfile integrity.\n",
        ),
        "update" => Some(
            "Usage: arandu update [path] [--accept]\n\nReview dependency graph changes; --accept publishes the reviewed graph.\n",
        ),
        "doctor" => Some(
            "Usage: arandu doctor [--stdlib-path DIR] [-v]\n\nInspect the compiler toolchain, runtime, and standard library.\n",
        ),
        "cache" => Some(
            "Usage: arandu cache <dir|inspect|verify|verify-tree|prune> [options]\n\nInspect or maintain the compiler cache.\n",
        ),
        "lex" => Some(
            "Usage: arandu lex <file.aru>\n\nPrint the source token stream (compiler inspection tool).\n",
        ),
        "parse" => Some(
            "Usage: arandu parse <file.aru>\n\nPrint the syntax tree (compiler inspection tool).\n",
        ),
        "hir" => Some(
            "Usage: arandu hir <file.aru> [--debug]\n\nPrint the lowered High-level IR (compiler inspection tool).\n",
        ),
        "amir" => Some(
            "Usage: arandu amir <file.aru> [--cfg] [--ascii] [--opt | --release]\n\nPrint Arandu Mid-level IR (compiler inspection tool).\n--opt applies AMIR O1 and --release AMIR O2, as `build` does.\n",
        ),
        "graph" => Some(
            "Usage: arandu graph <file.aru>\n\nEmit a Graphviz dependency graph (compiler inspection tool).\n",
        ),
        "emit-c" => Some(
            "Usage: arandu emit-c <file.aru> [--opt | --release | --size]\n\nEmit portable C source (compiler inspection tool).\nThe AMIR level matches `build`: --opt O1, --release O2, --size Os.\n",
        ),
        "emit-llvm" => Some(
            "Usage: arandu emit-llvm <file.aru> [--target TRIPLE]\n\nEmit LLVM IR directly from AMIR (no Clang required to emit).\nLike the LLVM product, the AMIR is always optimized at O2.\nHost layout stays the default. Canonical bare-metal triples use a 32-bit layout.\n",
        ),
        "emit-wasm" => Some(
            "Usage: arandu emit-wasm <file.aru> [--layout ptr4] [--opt | --release | --size]\n\nEmit a WebAssembly module (compiler inspection tool).\nThe AMIR level matches `build`: --opt O1, --release O2, --size Os.\n",
        ),
        "emit-component" => Some(
            "Usage: arandu emit-component <file.aru> [--opt | --release | --size]\n\nEmit a WebAssembly Component (compiler inspection tool).\nThe AMIR level matches `build`: --opt O1, --release O2, --size Os.\n",
        ),
        "hash-file" => {
            Some("Usage: arandu hash-file <path>\n\nCompute a BLAKE3 checksum for packaging.\n")
        }
        "archive" => Some(
            "Usage: arandu archive validate <archive>\n\nValidate an Arandu package archive.\n",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_help_mentions_every_registered_command() {
        let help = global_help();
        for command in crate::commands::COMMAND_NAMES {
            assert!(help.contains(command), "global help omits {command}");
            assert!(
                command_help(command).is_some(),
                "registered command {command} has no command-specific help"
            );
        }
    }

    #[test]
    fn equals_form_value_flags_are_normalized_for_command_parsers() {
        assert_eq!(
            normalize_equals_flags(vec![
                "test".to_string(),
                "--format=json".to_string(),
                "--filter=parser".to_string(),
                "--quiet".to_string(),
                "--watch".to_string(),
            ]),
            [
                "test", "--format", "json", "--filter", "parser", "--quiet", "--watch"
            ]
        );
    }

    #[test]
    fn run_backend_accepts_separated_and_equals_forms() {
        let separated = parse_invocation([
            "arandu".to_string(),
            "run".to_string(),
            "-b".to_string(),
            "llvm".to_string(),
        ]);
        assert_eq!(separated.backend, RunBackend::Llvm);
        let equals = parse_invocation([
            "arandu".to_string(),
            "run".to_string(),
            "--backend=cranelift".to_string(),
        ]);
        assert_eq!(equals.backend, RunBackend::Cranelift);
    }

    #[test]
    fn separated_layout_and_quiet_options_are_global() {
        let invocation = parse_invocation([
            "arandu".to_string(),
            "build".to_string(),
            "--layout".to_string(),
            "ptr4".to_string(),
            "--quiet".to_string(),
        ]);
        assert_eq!(invocation.data_layout.pointer_width(), 4);
        assert!(invocation.quiet);
        assert!(invocation.project_flags.quiet);
    }
}
