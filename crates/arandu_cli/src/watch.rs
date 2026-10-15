//! `arandu watch` — package check loop with notify-debouncer-full.
//!
//! OS events are correlated/debounced by `notify-debouncer-full` (rename as one
//! event), then fed into [`arandu_query::PackageWatchSession`] which applies
//! **one** Salsa commit per quiet window (shared debounce design with LSP).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use arandu_query::{DEFAULT_DEBOUNCE, FsChange, PackageWatchSession, scan_aru_entries};
use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, DebouncedEvent, new_debouncer};

use crate::cli_error::{CliFailure, CliResult};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;

/// Run package-mode watch until Ctrl-C / fatal error.
pub fn cmd_watch(start: &Path, flags: &ProjectFlags, data_layout: DataLayout) -> CliResult {
    let (mut db, rebuild_log) = arandu_query::DatabaseImpl::with_rebuild_log();
    db.set_target_config(data_layout);
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };

    // Register all package modules under import keys so first check is multi-file aware.
    let package_src = ctx
        .entry_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ctx.root.clone());
    for rel in scan_aru_entries(&package_src) {
        let abs = package_src.join(&rel);
        if let Ok(text) = std::fs::read_to_string(&abs) {
            let pkg_key = format!("{}/{}", ctx.name, rel.replace('\\', "/"));
            db.new_file(pkg_key, text.clone());
            db.new_file(rel.replace('\\', "/"), text);
        }
    }

    let Some(roots) = db.module_roots() else {
        return Err(CliFailure::operational(
            "start package watcher",
            Some(ctx.root),
            "package module roots not initialized (internal)",
        ));
    };
    let listing = *roots.package_listing(&db);

    let mut sess = PackageWatchSession::new(
        &mut db,
        arandu_query::PackageWatchConfig {
            package_root: ctx.root.clone(),
            package_src,
            package_name: ctx.name.clone(),
            entry_rel: ctx.entry_rel.clone(),
            entry_abs: ctx.entry_path.clone(),
            manifest_path: ctx.manifest_path.clone(),
            listing,
            module_roots: roots,
            manifest: ctx.manifest,
        },
    );

    let entry_key = format!("{}/{}", ctx.name, "main.aru");
    let entry_key_alt = ctx.entry_path.to_string_lossy().into_owned();
    let entry = db
        .source_file_by_path(&entry_key)
        .or_else(|| {
            let rel = ctx
                .entry_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("main.aru");
            db.source_file_by_path(&format!("{}/{}", ctx.name, rel))
        })
        .or_else(|| db.source_file_by_path(&entry_key_alt))
        .unwrap_or_else(|| {
            let text = std::fs::read_to_string(&ctx.entry_path).unwrap_or_default();
            db.new_file(entry_key.clone(), text)
        });

    // Initial check.
    print_check(&db, &rebuild_log, entry, "initial", flags);

    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(DEFAULT_DEBOUNCE, None, tx) {
        Ok(d) => d,
        Err(e) => {
            return Err(CliFailure::operational(
                "start file watcher",
                Some(ctx.root.clone()),
                e.to_string(),
            ));
        }
    };

    // Watch package root (covers src/ + Arandu.toml).
    debouncer
        .watch(&ctx.root, RecursiveMode::Recursive)
        .map_err(|e| CliFailure::operational("watch", Some(ctx.root.clone()), e.to_string()))?;
    if !flags.quiet {
        eprintln!(
            "watching {} (package `{}`) — Ctrl-C to stop",
            ctx.root.display(),
            ctx.name
        );
    }

    loop {
        // Wait for debounced events (or timeout to flush WatchBuffer if nested debounce).
        let timeout = sess
            .buffer_mut()
            .next_deadline()
            .unwrap_or(Duration::from_millis(500));

        match rx.recv_timeout(timeout) {
            Ok(Ok(events)) => {
                for ev in events {
                    map_debounced_event(&mut sess, &ev);
                }
            }
            Ok(Err(errors)) => {
                for e in errors {
                    eprintln!("watch error: {e}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Fall through to commit any due buffer entries.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CliFailure::operational(
                    "watch project",
                    Some(ctx.root.clone()),
                    "watcher channel closed",
                ));
            }
        }

        let summary = sess.commit(&mut db, false);
        if summary.modified + summary.created + summary.removed + summary.renamed > 0
            || summary.manifest_reloaded
        {
            // Entry SourceFile may have been re-registered after package rename.
            let entry = db
                .source_file_by_path(&format!("{}/main.aru", sess.package_name))
                .or_else(|| {
                    let rel = Path::new(&sess.entry_rel)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("main.aru");
                    db.source_file_by_path(&format!("{}/{}", sess.package_name, rel))
                })
                .unwrap_or(entry);
            rebuild_log.clear();
            print_check(&db, &rebuild_log, entry, "rebuild", flags);
        }
    }
}

fn map_debounced_event(sess: &mut PackageWatchSession, ev: &DebouncedEvent) {
    use notify::event::{ModifyKind, RenameMode};

    let paths: Vec<PathBuf> = ev.event.paths.clone();
    if paths.is_empty() {
        return;
    }

    match &ev.event.kind {
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() >= 2 => {
            sess.push_rename(paths[0].clone(), paths[1].clone());
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            // Partial rename — wait for To; debouncer-full usually correlates Both.
            sess.push(paths[0].clone(), FsChange::Remove);
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            sess.push(paths[0].clone(), FsChange::Create);
        }
        EventKind::Remove(_) => {
            for p in paths {
                sess.push(p, FsChange::Remove);
            }
        }
        EventKind::Create(_) => {
            for p in paths {
                sess.push(p, FsChange::Create);
            }
        }
        EventKind::Modify(_) => {
            for p in paths {
                sess.push(p, FsChange::Modify);
            }
        }
        _ => {
            // Treat other kinds that touch .aru / Arandu.toml as modify.
            for p in paths {
                if p.extension().and_then(|e| e.to_str()) == Some("aru")
                    || matches!(
                        p.file_name().and_then(|s| s.to_str()),
                        Some("arandu.toml" | "Arandu.toml")
                    )
                {
                    sess.push(p, FsChange::Modify);
                }
            }
        }
    }
}

fn print_check(
    db: &arandu_query::DatabaseImpl,
    rebuild_log: &std::sync::Arc<arandu_query::RebuildLog>,
    entry: arandu_query::SourceFile,
    tag: &str,
    flags: &ProjectFlags,
) {
    let filepath = entry.path(db).to_string_lossy().into_owned();
    let diags = match crate::pipeline::pipeline_lower_checked(db, entry) {
        Ok(outcome) => outcome.diagnostics,
        Err(diagnostics) => diagnostics,
    };

    if !flags.quiet
        && (flags.verbose
            || arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed))
    {
        eprintln!("{}", rebuild_log.status_line());
    }
    let errors = diags
        .iter()
        .filter(|d| matches!(d.severity, arandu_middle::Severity::Error))
        .count();
    crate::pipeline::render_nonfatal_diagnostics(db, &diags, &filepath);
    if errors == 0 {
        if !flags.quiet {
            eprintln!("ok ({tag}) — no errors");
        }
    } else {
        eprintln!("failed ({tag}) — {errors} error(s)");
    }
}
