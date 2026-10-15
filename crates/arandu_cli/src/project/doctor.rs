//! Comprehensive environment and toolchain diagnosis (`arandu doctor`).

use std::path::{Path, PathBuf};

use arandu_query::{
    MANIFEST_FILENAME, ManifestSpelling, STDLIB_ENV, StdlibResolveOpts,
    ensure_toolchain_compatible, resolve_stdlib_root,
};

use super::load::{ARANDU_VERSION, ProjectFlags};
use crate::manifest_io::{find_manifest, load_manifest};

/// Diagnose toolchain / project / backend (Flutter-style doctor report).
pub fn cmd_doctor(flags: &ProjectFlags) -> i32 {
    let color = flags.color.should_color_stdout();
    let mut categories: Vec<DoctorCategory> = Vec::new();

    // [Arandu] toolchain binary (show raw + canonical when they differ)
    categories.push(match std::env::current_exe() {
        Ok(exe) => {
            let (real, _) = arandu_query::resolve_exe_path(exe.clone());
            let mut details = vec![
                DoctorDetail::Info(format!("binary at {}", exe.display())),
                DoctorDetail::Info(format!("version {ARANDU_VERSION}")),
            ];
            if real != exe {
                details.push(DoctorDetail::Info(format!(
                    "resolved path {} (symlink followed)",
                    real.display()
                )));
            } else if flags.verbose {
                details.push(DoctorDetail::Info(format!(
                    "canonical path {}",
                    real.display()
                )));
            }
            if flags.verbose {
                details.push(DoctorDetail::Info(format!(
                    "host {}-{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )));
            }
            DoctorCategory {
                status: DoctorStatus::Ok,
                title: format!("Arandu toolchain (v{ARANDU_VERSION})"),
                details,
            }
        }
        Err(e) => DoctorCategory {
            status: DoctorStatus::Fail,
            title: "Arandu toolchain".into(),
            details: vec![
                DoctorDetail::Error(format!("could not resolve current_exe(): {e}")),
                DoctorDetail::Hint(
                    "reinstall the arandu binary or check PATH / install prefix".into(),
                ),
            ],
        },
    });

    // [Stdlib]
    categories.push(
        match resolve_stdlib_root(StdlibResolveOpts {
            explicit: flags.stdlib_path.clone(),
            ..Default::default()
        }) {
            Ok(root) => {
                let mut details = vec![
                    DoctorDetail::Info(format!("stdlib at {}", root.path.display())),
                    DoctorDetail::Info(format!("resolved via {}", root.source)),
                ];
                if flags.verbose {
                    details.push(DoctorDetail::Info(
                        "cascade: --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)"
                            .into(),
                    ));
                }
                DoctorCategory {
                    status: DoctorStatus::Ok,
                    title: "Stdlib".into(),
                    details,
                }
            }
            Err(e) => {
                let mut details = vec![DoctorDetail::Error(e.to_string().replace('\n', " "))];
                // Expand "tried" lines as nested bullets when verbose.
                if flags.verbose {
                    for line in e.tried {
                        details.push(DoctorDetail::Info(line));
                    }
                }
                details.push(DoctorDetail::Hint(format!(
                "pass --stdlib-path=<dir>, set {STDLIB_ENV}, or install under share/arandu/stdlib"
            )));
                DoctorCategory {
                    status: DoctorStatus::Fail,
                    title: "Stdlib".into(),
                    details,
                }
            }
        },
    );

    // [Project] Arandu.toml (optional when not in a package)
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    categories.push(match find_manifest(&cwd) {
        Ok(Some(discovery)) => {
            let path = discovery.path;
            match load_manifest(&path) {
                Ok((data, hash, _)) => {
                    let mut details = vec![
                        DoctorDetail::Info(format!("manifest at {}", path.display())),
                        DoctorDetail::Info(format!(
                            "package {} {}  entry={}",
                            data.name, data.version, data.entry
                        )),
                    ];
                    if discovery.spelling == ManifestSpelling::Legacy {
                        details.push(DoctorDetail::Error(format!(
                            "legacy manifest name `{}`; rename it to `{MANIFEST_FILENAME}`",
                            arandu_query::LEGACY_MANIFEST_FILENAME
                        )));
                    }
                    let toolchain_error =
                        ensure_toolchain_compatible(&path, &data, ARANDU_VERSION).err();
                    if let Some(error) = &toolchain_error {
                        details.push(DoctorDetail::Error(error.to_string()));
                    }
                    if flags.verbose {
                        details.push(DoctorDetail::Info(format!(
                            "schema={} edition={:?} kind={:?}",
                            data.schema, data.edition, data.kind
                        )));
                        details.push(DoctorDetail::Info(format!(
                            "content hash {}…",
                            &hash[..12.min(hash.len())]
                        )));
                        let entry = path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(&data.entry);
                        if entry.is_file() {
                            details.push(DoctorDetail::Info(format!(
                                "entry file ok ({})",
                                entry.display()
                            )));
                        } else {
                            details.push(DoctorDetail::Error(format!(
                                "entry file missing ({})",
                                entry.display()
                            )));
                        }
                    }
                    let entry_ok = path
                        .parent()
                        .map(|p| p.join(&data.entry).is_file())
                        .unwrap_or(false);
                    DoctorCategory {
                        status: if entry_ok
                            && discovery.spelling == ManifestSpelling::Canonical
                            && toolchain_error.is_none()
                        {
                            DoctorStatus::Ok
                        } else {
                            DoctorStatus::Partial
                        },
                        title: format!("Project ({MANIFEST_FILENAME})"),
                        details: {
                            let mut d = details;
                            if !entry_ok {
                                d.push(DoctorDetail::Error(format!(
                                    "entry `{}` does not exist on disk",
                                    data.entry
                                )));
                                d.push(DoctorDetail::Hint(
                                    "fix the entry path in Arandu.toml or create the file".into(),
                                ));
                            }
                            d
                        },
                    }
                }
                Err(e) => DoctorCategory {
                    status: DoctorStatus::Fail,
                    title: format!("Project ({MANIFEST_FILENAME})"),
                    details: vec![
                        // BUG-09: never swallow parse errors
                        DoctorDetail::Error(e.to_string()),
                        DoctorDetail::Hint(
                            "fix the TOML (required: name, version, entry as quoted strings)"
                                .into(),
                        ),
                    ],
                },
            }
        }
        Ok(None) => DoctorCategory {
            status: DoctorStatus::Skip,
            title: format!("Project ({MANIFEST_FILENAME})"),
            details: vec![
                DoctorDetail::Info(format!("no package found from {}", cwd.display())),
                DoctorDetail::Info("not an error outside a project directory".into()),
                DoctorDetail::Hint("run `arandu new <name>` to scaffold a package".into()),
            ],
        },
        Err(error) => DoctorCategory {
            status: DoctorStatus::Fail,
            title: format!("Project ({MANIFEST_FILENAME})"),
            details: vec![DoctorDetail::Error(error.to_string())],
        },
    });

    // [Cranelift] dev backend
    categories.push(
        match arandu_backend_cranelift::CraneliftBackend::try_new() {
            Ok(_) => DoctorCategory {
                status: DoctorStatus::Ok,
                title: "Cranelift backend (dev JIT)".into(),
                details: vec![
                    DoctorDetail::Info("ISA initialized".into()),
                    DoctorDetail::Info("used by `run` and `build` (default)".into()),
                ],
            },
            Err(diag) => DoctorCategory {
                status: DoctorStatus::Fail,
                title: "Cranelift backend (dev JIT)".into(),
                details: vec![
                    DoctorDetail::Error(format!("failed to initialize ISA ({})", diag.message)),
                    DoctorDetail::Hint(
                        "run `arandu run <file.aru> -Zdebug-backend` for more detail".into(),
                    ),
                ],
            },
        },
    );

    // [Cranelift] release AOT backend
    categories.push(
        match arandu_backend_cranelift::CraneliftObjectBackend::host_release() {
            Ok(_) => DoctorCategory {
                status: DoctorStatus::Ok,
                title: "Cranelift backend (release AOT)".into(),
                details: vec![
                    DoctorDetail::Info("host ISA initialized with speed optimization".into()),
                    DoctorDetail::Info("used by `build --release` with AMIR O2".into()),
                ],
            },
            Err(diag) => DoctorCategory {
                status: DoctorStatus::Fail,
                title: "Cranelift backend (release AOT)".into(),
                details: vec![DoctorDetail::Error(format!(
                    "failed to initialize release ISA ({})",
                    diag.message
                ))],
            },
        },
    );

    // Env extras only in verbose
    if flags.verbose
        && let Ok(val) = std::env::var(STDLIB_ENV)
    {
        categories.push(DoctorCategory {
            status: DoctorStatus::Ok,
            title: format!("Environment ({STDLIB_ENV})"),
            details: vec![DoctorDetail::Info(val)],
        });
    }

    // ── Print Flutter-style report ──────────────────────────────────────
    if flags.verbose {
        println!("Doctor summary (verbose):");
    } else {
        println!("Doctor summary (to see all details, run arandu doctor -v):");
    }
    println!();

    let mut issues = 0usize;
    for cat in &categories {
        if matches!(cat.status, DoctorStatus::Fail | DoctorStatus::Partial) {
            issues += 1;
        }
        print_category(cat, color, flags.verbose);
        println!();
    }

    if issues == 0 {
        println!("{} No issues found!", bullet_ok(color));
        0
    } else {
        println!(
            "{} Doctor found issues in {issues} categor{}.",
            bullet_warn(color),
            if issues == 1 { "y" } else { "ies" }
        );
        1
    }
}

#[derive(Clone, Copy)]
enum DoctorStatus {
    Ok,
    Partial,
    Fail,
    Skip,
}

struct DoctorCategory {
    status: DoctorStatus,
    title: String,
    details: Vec<DoctorDetail>,
}

enum DoctorDetail {
    Info(String),
    Error(String),
    Hint(String),
}

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn status_tag(status: DoctorStatus, color: bool) -> String {
    match status {
        DoctorStatus::Ok => paint(color, "32", "[✓]"),
        DoctorStatus::Partial => paint(color, "33", "[!]"),
        DoctorStatus::Fail => paint(color, "31", "[✗]"),
        DoctorStatus::Skip => paint(color, "90", "[-]"),
    }
}

fn bullet_ok(color: bool) -> String {
    paint(color, "32", "•")
}

fn bullet_warn(color: bool) -> String {
    paint(color, "33", "!")
}

fn print_category(cat: &DoctorCategory, color: bool, verbose: bool) {
    println!("{} {}", status_tag(cat.status, color), cat.title);
    let show_all = verbose || matches!(cat.status, DoctorStatus::Fail | DoctorStatus::Partial);
    if !show_all && !verbose {
        // Compact mode: one-line category is enough when healthy; still show
        // first info line for Skip so users know why it is blank.
        if matches!(cat.status, DoctorStatus::Skip)
            && let Some(DoctorDetail::Info(msg)) = cat.details.first()
        {
            println!("    • {msg}");
        }
        return;
    }
    for d in &cat.details {
        match d {
            DoctorDetail::Info(msg) => println!("    • {msg}"),
            DoctorDetail::Error(msg) => {
                println!("    {} {msg}", paint(color, "31", "✗"));
            }
            DoctorDetail::Hint(msg) => {
                println!("    {} {msg}", paint(color, "36", "→"));
            }
        }
    }
}
