//! Project lifecycle commands: new, init, clean, audit, inspect, vendor, update, cache, watch.

use std::env;
use std::path::Path;

use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::pipeline::{fail_operational, fail_usage, finish};
use crate::project::{self, ProjectFlags};
use crate::watch;
use arandu_middle::layout::DataLayout;

pub fn cmd_new(args: &[String], quiet: bool) -> CliResult {
    if args.len() < 3 {
        fail_usage("usage: arandu_cli new <project-name> [--bin|--lib] [--vcs=auto|git|none]");
    }
    let options = project::parse_scaffold_options(&args[3..])
        .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
    project::cmd_new(&args[2], options, quiet)
}

pub fn cmd_init(args: &[String], quiet: bool) -> CliResult {
    let options = project::parse_scaffold_options(&args[2..])
        .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
    let root = env::current_dir().unwrap_or_else(|error| {
        fail_operational("resolve current directory", None, error.to_string())
    });
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| fail_usage("current directory has no valid UTF-8 package name"));
    project::cmd_init(&root, name, options, quiet)
}

pub fn cmd_clean(start: &Path, quiet: bool) -> CliResult {
    let discovery = crate::manifest_io::find_manifest(start)
        .unwrap_or_else(|error| {
            fail_operational(
                "discover project",
                Some(start.to_path_buf()),
                error.to_string(),
            )
        })
        .unwrap_or_else(|| {
            fail_operational(
                "discover project",
                Some(start.to_path_buf()),
                "no arandu.toml found",
            )
        });
    crate::manifest_io::load_manifest(&discovery.path).unwrap_or_else(|error| {
        fail_operational(
            "load project manifest",
            Some(discovery.path.clone()),
            error.to_string(),
        )
    });
    let root = discovery.path.parent().unwrap_or_else(|| Path::new("."));
    let removed = artifact::clean(root).unwrap_or_else(|error| finish(Err(error)));
    if !quiet {
        if removed {
            println!("removed target");
        } else {
            println!("already clean");
        }
    }
    Ok(CliSuccess::Done)
}

pub fn cmd_update(start: &Path, flags: &ProjectFlags) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::default();
    let ctx = project::load_project(&mut db, start, flags).unwrap_or_else(|e| {
        fail_operational("review dependency graph", Some(start.to_path_buf()), e)
    });
    if !flags.quiet {
        println!("accepted graph {}", ctx.lockfile.manifest_fingerprint);
    }
    Ok(CliSuccess::Done)
}

pub fn cmd_vendor(start: &Path, flags: &ProjectFlags) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::default();
    let policy = ProjectFlags {
        locked: true,
        offline: true,
        ..flags.clone()
    };
    let ctx = project::load_project(&mut db, start, &policy).unwrap_or_else(|e| {
        fail_operational("prepare verified vendor", Some(start.to_path_buf()), e)
    });
    let path = arandu_package::vendor::materialize(&ctx.root, &ctx.cache, &ctx.lockfile)
        .unwrap_or_else(|e| fail_operational("publish verified vendor", Some(ctx.root.clone()), e));
    if !flags.quiet {
        println!("vendored locked graph at {}", path.display());
    }
    Ok(CliSuccess::Done)
}

pub fn cmd_audit(start: &Path, flags: &ProjectFlags) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::default();
    let policy = ProjectFlags {
        locked: true,
        offline: true,
        ..flags.clone()
    };
    let ctx = project::load_project(&mut db, start, &policy).map_err(|error| {
        CliFailure::operational(
            "audit locked project graph",
            Some(start.to_path_buf()),
            error,
        )
    })?;
    println!("audit graph {}", ctx.lockfile.manifest_fingerprint);
    let mut remote = 0usize;
    for package in &ctx.lockfile.packages {
        if let (Some(origin), Some(commit), Some(digest)) = (
            package.origin.as_deref(),
            package.commit.as_deref(),
            package.content_digest.as_deref(),
        ) {
            remote += 1;
            println!(
                "remote {} origin={} commit={} digest={}",
                package.name, origin, commit, digest
            );
        }
    }
    println!(
        "integrity=verified locked=offline remote_packages={remote} advisories=not-configured"
    );
    Ok(CliSuccess::Done)
}

pub fn cmd_inspect(start: &Path, flags: &ProjectFlags, verify: bool) -> CliResult {
    let mut policy = flags.clone();
    if verify {
        policy.locked = true;
        policy.offline = true;
    }
    let mut db = arandu_query::DatabaseImpl::default();
    let ctx = project::load_project(&mut db, start, &policy).map_err(|error| {
        CliFailure::operational(
            if verify {
                "verify locked project graph"
            } else {
                "resolve project graph"
            },
            Some(start.to_path_buf()),
            error,
        )
    })?;
    let lock = ctx.lockfile;
    println!("graph {}", lock.manifest_fingerprint);
    for package in lock.packages {
        let digest = package.content_digest.as_deref().unwrap_or("local");
        println!(
            "{} {} {} {}",
            package.name, package.version, package.source, digest
        );
        for dependency in package.dependencies {
            println!("  -> {dependency}");
        }
    }
    if verify {
        println!("verified locked offline graph");
    }
    Ok(CliSuccess::Done)
}

pub fn cmd_cache(args: &[String], project_flags: &ProjectFlags) -> CliResult {
    use arandu_package::cache;
    if args.len() < 3 {
        fail_usage("usage: arandu_cli cache <dir|inspect|verify|verify-tree|prune> [options]");
    }
    let layout = cache::resolve_cache_layout(project_flags.cache_dir.as_deref())
        .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
    if args[2] == "dir" {
        if args.len() != 3 {
            fail_usage("usage: arandu_cli cache dir [--cache-dir=<absolute-dir>]");
        }
        println!("{}", layout.root().display());
        return Ok(CliSuccess::Done);
    }
    let allow_dry_run = args[2] == "prune";
    if args[2] == "verify-tree" {
        if args.len() != 5 {
            fail_usage("usage: arandu_cli cache verify-tree <archive-digest> <tree-digest>");
        }
        let archive: arandu_query::CacheDigest = args[3]
            .parse()
            .unwrap_or_else(|error| fail_usage(format!("error: invalid archive digest: {error}")));
        let tree: arandu_query::CacheDigest = args[4]
            .parse()
            .unwrap_or_else(|error| fail_usage(format!("error: invalid tree digest: {error}")));
        let report = cache::CacheStore::new(layout)
            .verify_tree(archive, tree, cache::TreeLimits::default())
            .unwrap_or_else(|error| {
                fail_operational("verify extracted package tree", None, error.to_string())
            });
        println!(
            "tree={} files={} bytes={} depth={}",
            report.digest, report.files, report.bytes, report.depth
        );
        return Ok(CliSuccess::Done);
    }
    let (limits, dry_run) = cache::parse_scan_flags(&args[3..], allow_dry_run)
        .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
    let store = cache::CacheStore::new(layout);
    match args[2].as_str() {
        "inspect" => {
            let report = store.inspect(limits).unwrap_or_else(|error| {
                fail_operational("inspect package cache", None, error.to_string())
            });
            println!(
                "archives={} bytes={} invalid={} staging={} quarantine={}",
                report.archives,
                report.archive_bytes,
                report.invalid_entries,
                report.staging_files,
                report.quarantine_files
            );
            Ok(CliSuccess::Done)
        }
        "verify" => {
            let report = store.verify(limits).unwrap_or_else(|error| {
                fail_operational("verify package cache", None, error.to_string())
            });
            println!(
                "verified={} bytes={} corrupt={} invalid={}",
                report.verified, report.verified_bytes, report.corrupt, report.invalid_entries
            );
            let code = i32::from(report.corrupt != 0 || report.invalid_entries != 0);
            Ok(CliSuccess::ProgramExit(code))
        }
        "prune" => {
            let report = store.prune(limits, dry_run).unwrap_or_else(|error| {
                fail_operational("prune package cache", None, error.to_string())
            });
            println!(
                "files={} bytes={} dry_run={}",
                report.files, report.bytes, report.dry_run
            );
            Ok(CliSuccess::Done)
        }
        _ => fail_usage("usage: arandu_cli cache <dir|inspect|verify|verify-tree|prune> [options]"),
    }
}

pub fn cmd_watch(start: &Path, flags: &ProjectFlags, data_layout: DataLayout) -> CliResult {
    watch::cmd_watch(start, flags, data_layout)
}
