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

#[derive(Debug, Clone)]
pub struct CliInvocation {
    pub debug: bool,
    pub opt: bool,
    pub parallel: bool,
    pub genref_report: bool,
    pub cfg: bool,
    pub ascii: bool,
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
    let mut parallel = false;
    let mut genref_report = false;
    let mut cfg = false;
    let mut ascii = false;
    let mut color = ColorChoice::Auto;
    let mut args = Vec::new();
    let mut program_args = Vec::new();
    let mut z_flags: Vec<String> = Vec::new();
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
            "--debug" => debug = true,
            "--opt" => opt = true,
            "--parallel" => parallel = true,
            "--genref-report" => genref_report = true,
            "--cfg" => cfg = true,
            "--ascii" => ascii = true,
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
            s if s.starts_with("--layout=") => layout_flags.push(arg.clone()),
            // Collect project flags even before we know the subcommand.
            s if s.starts_with("--stdlib-path")
                || s.starts_with("--cache-dir")
                || s.starts_with("--target")
                || s == "--release"
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
                    if i < raw_args_vec.len() {
                        raw_project_flags.push(raw_args_vec[i].clone());
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
    let (mut project_flags, extra_positional) = project::parse_project_flags(&raw_project_flags)
        .unwrap_or_else(|message| fail_usage(format!("error: {message}")));
    let _ = extra_positional;
    project_flags.color = color;
    if layout_flags.is_empty()
        && project_flags
            .target
            .as_deref()
            .is_some_and(|t| t.starts_with("wasm32"))
    {
        data_layout = DataLayout::ptr_width(4);
    }

    CliInvocation {
        debug,
        opt,
        parallel,
        genref_report,
        cfg,
        ascii,
        args,
        program_args,
        z_flags,
        data_layout,
        project_flags,
    }
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
    let message = concat!(
        "The Arandu Programming Language Compiler\n\n",
        "usage:\n",
        "  arandu <command> [options] [package-path | file]\n",
        "  arandu_cli <command> [options] [package-path | file]\n\n",
        "Build & Execution Commands:\n",
        "  run        Compile and execute a package or file via Cranelift JIT\n",
        "  build      Compile package to native executable or library\n",
        "  check      Type-check and validate without code generation\n",
        "  test       Execute unit and integration test suites\n",
        "  bench      Run benchmarks and compare baseline metrics\n\n",
        "Project & Package Management:\n",
        "  new        Create a new Arandu project directory [--bin|--lib] [--vcs=auto|git|none]\n",
        "  init       Initialize an Arandu package in current directory [--bin|--lib]\n",
        "  watch      Watch filesystem and re-check package incrementally\n",
        "  clean      Remove project build artifacts and scratch cache\n",
        "  doc        Generate package documentation [--format=html|json|md] [--open]\n\n",
        "Dependencies & Supply-Chain:\n",
        "  tree       Display canonical resolved dependency graph\n",
        "  audit      Audit locked provenance and security policies\n",
        "  vendor     Create verified offline source snapshot\n",
        "  verify     Verify offline cache integrity against lockfile\n",
        "  update     Review and publish remote graph update (--accept)\n\n",
        "Plumbing & Inspection Commands:\n",
        "  lex        Dump concrete syntax tokens\n",
        "  parse      Dump concrete syntax tree (Rowan CST / AST)\n",
        "  hir        Dump High-Level Intermediate Representation\n",
        "  amir       Dump Arandu Mid-Level IR (SSA/OSSA) [--cfg] [--ascii] [--opt]\n",
        "  graph      Emit module dependency graph in Graphviz DOT format\n",
        "  emit-c     Emit portable C source code\n",
        "  emit-wasm  Emit WebAssembly binary (wasm32; use --layout=ptr4) [--opt]\n",
        "  emit-component  Emit WebAssembly Component (WIT-wrapped) [--opt]\n",
        "  fmt        Format source files according to canonical style rules\n",
        "  doctor     Inspect compiler toolchain, environment, and stdlib paths\n",
        "  cache      Inspect, prune, and verify compiler cache <dir|inspect|verify|prune>\n",
        "  hash-file  Compute BLAKE3 checksum for packaging\n\n",
        "Target & Toolchain Options:\n",
        "  --release                  Build with speed optimizations (Cranelift + AMIR O2)\n",
        "  --stdlib-path <dir>        Override path to standard library\n",
        "  --cache-dir <dir>          Override compiler cache directory\n",
        "  --layout=host|ptr4|ptr8|i686  (default: host)\n",
        "                             layout model only; cross compiler/sysroot are external\n",
        "  --color=auto|always|never  Control ANSI color output (default: auto)\n",
        "  --no-color                 Disable ANSI color output (respects https://no-color.org)\n",
        "  --vcs=auto|git|none        VCS initialization mode for new projects\n",
        "  -v, --verbose              Enable detailed progress and timing logs\n",
        "  -V, --version              Print compiler version and exit\n",
        "  -h, --help                 Print this help message\n\n",
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
        "  backend: build → Cranelift baseline; build --release → Cranelift speed + AMIR O2\n",
        "  stdlib:  --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)\n",
        "  cache:   --cache-dir > ARANDU_CACHE_DIR > platform-native user cache"
    );
    finish(Err(CliFailure::usage(message)))
}
