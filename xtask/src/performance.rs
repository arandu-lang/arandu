use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arandu_query::{DatabaseImpl, RebuildCounts, RebuildLog, SourceFile};
use salsa::Setter;

pub fn cmd_check_project_performance(workspace_root: &Path) -> i32 {
    match check_project_performance(workspace_root) {
        Ok(report) => {
            println!("{report}");
            0
        }
        Err(error) => {
            eprintln!("check-project-performance: error: {error}");
            1
        }
    }
}

#[derive(Debug)]
struct Budget {
    samples: usize,
    warmup: usize,
    endurance_revisions: usize,
    max_noop_executes: usize,
    max_edit_p95_executes: usize,
    max_rss_growth_bytes: u64,
}

struct Fixture {
    db: DatabaseImpl,
    log: Arc<RebuildLog>,
    entry: SourceFile,
    main: SourceFile,
    util: SourceFile,
}

#[derive(Debug)]
struct Window {
    elapsed: Duration,
    counts: RebuildCounts,
}

fn check_project_performance(workspace_root: &Path) -> Result<String, String> {
    let baseline_path = workspace_root.join("tests/perf/s2-baseline.txt");
    let budget = load_budget(&baseline_path)?;

    for _ in 0..budget.warmup {
        let mut fixture = Fixture::new();
        let _ = measure(&mut fixture, |_| {});
    }

    let mut cold = Vec::with_capacity(budget.samples);
    for _ in 0..budget.samples {
        let mut fixture = Fixture::new();
        cold.push(measure(&mut fixture, |_| {}).elapsed);
    }

    let mut fixture = Fixture::new();
    let _ = measure(&mut fixture, |_| {});
    let mut noop = Vec::with_capacity(budget.samples);
    let mut noop_executes = 0;
    for _ in 0..budget.samples {
        let window = measure(&mut fixture, |_| {});
        noop_executes = noop_executes.max(window.counts.executed);
        noop.push(window.elapsed);
    }
    if noop_executes > budget.max_noop_executes {
        return Err(format!(
            "noop executed {noop_executes} queries; budget is {}",
            budget.max_noop_executes
        ));
    }

    let mut item_edit = Vec::with_capacity(budget.samples);
    let mut item_edit_executes = Vec::with_capacity(budget.samples);
    for revision in 0..budget.samples {
        let literal = if revision.is_multiple_of(2) { 7 } else { 9 };
        let window = measure(&mut fixture, |fixture| {
            fixture.util.set_text(&mut fixture.db).to(Arc::from(format!(
                "public func answer(): int {{ return {literal} }}\n"
            )));
        });
        item_edit.push(window.elapsed);
        item_edit_executes.push(window.counts.executed);
    }
    let item_edit_p95_executes = percentile_usize(&mut item_edit_executes, 95);
    let mut block_edit = Vec::with_capacity(budget.samples);
    let mut block_edit_executes = Vec::with_capacity(budget.samples);
    for revision in 0..budget.samples {
        let literal = if revision.is_multiple_of(2) { 1 } else { 2 };
        let window = measure(&mut fixture, |fixture| {
            fixture.main.set_text(&mut fixture.db).to(Arc::from(format!(
                "module perf\nimport perf.util as util\nfunc stable(): int {{ return 1 }}\nfunc main(): int {{ return util.answer() + stable() + {literal} - {literal} }}\n"
            )));
        });
        block_edit.push(window.elapsed);
        block_edit_executes.push(window.counts.executed);
    }
    let block_edit_p95_executes = percentile_usize(&mut block_edit_executes, 95);
    let edit_p95_executes = item_edit_p95_executes.max(block_edit_p95_executes);
    if edit_p95_executes > budget.max_edit_p95_executes {
        return Err(format!(
            "isolated edit p95 executed {edit_p95_executes} queries; budget is {}",
            budget.max_edit_p95_executes
        ));
    }

    let rss_before = current_rss_bytes();
    let mut rss_windows = rss_before.into_iter().collect::<Vec<_>>();
    for revision in 0..budget.endurance_revisions {
        // Keep instrumentation itself bounded; otherwise the event log, rather
        // than Salsa memos, would dominate the retention measurement.
        fixture.log.clear();
        fixture.util.set_text(&mut fixture.db).to(Arc::from(format!(
            "public func answer(): int {{ return {} }}\n",
            revision % 97
        )));
        let _ = arandu_query::passes::lower_amir(&fixture.db, fixture.entry);
        if (revision + 1).is_multiple_of(250) {
            if let Some(rss) = current_rss_bytes() {
                rss_windows.push(rss);
            }
        }
    }
    let rss_after = current_rss_bytes();
    let rss_growth = rss_before
        .zip(rss_after)
        .map(|(before, after)| after.saturating_sub(before));
    if rss_growth.is_some_and(|growth| growth > budget.max_rss_growth_bytes) {
        return Err(format!(
            "RSS grew by {} bytes; functional budget is {}",
            rss_growth.unwrap_or_default(),
            budget.max_rss_growth_bytes
        ));
    }

    let registry = fixture.db.registry_metrics();
    if registry.registered_paths != 4
        || registry.live_file_ids != 2
        || registry.allocated_file_ids != 2
    {
        return Err(format!("unexpected registry retention: {registry:?}"));
    }
    let rss_window_values = if rss_windows.is_empty() {
        "unavailable".to_owned()
    } else {
        rss_windows
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let report = format!(
        "check-project-performance: ok\nprotocol=1 samples={} warmup={} endurance_revisions={}\ntoolchain={} platform={} cpu={} commit={}\ncold_median_us={} cold_p95_us={}\nnoop_median_us={} noop_p95_us={} noop_max_executes={}\nitem_edit_median_us={} item_edit_p95_us={} item_edit_p95_executes={}\nblock_edit_median_us={} block_edit_p95_us={} block_edit_p95_executes={}\nrss_before_bytes={} rss_after_bytes={} rss_growth_bytes={} rss_windows={}\nregistered_paths={} live_file_ids={} allocated_file_ids={}\n",
        budget.samples,
        budget.warmup,
        budget.endurance_revisions,
        toolchain_name(workspace_root),
        std::env::consts::OS,
        cpu_name(),
        std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local-working-tree".into()),
        statistic_us(&mut cold, 50),
        statistic_us(&mut cold, 95),
        statistic_us(&mut noop, 50),
        statistic_us(&mut noop, 95),
        noop_executes,
        statistic_us(&mut item_edit, 50),
        statistic_us(&mut item_edit, 95),
        item_edit_p95_executes,
        statistic_us(&mut block_edit, 50),
        statistic_us(&mut block_edit, 95),
        block_edit_p95_executes,
        optional_metric(rss_before),
        optional_metric(rss_after),
        optional_metric(rss_growth),
        rss_window_values,
        registry.registered_paths,
        registry.live_file_ids,
        registry.allocated_file_ids,
    );
    let report_path = workspace_root.join("target/s2-performance-report.txt");
    fs::write(&report_path, &report)
        .map_err(|error| format!("failed to write {}: {error}", report_path.display()))?;
    Ok(report)
}

impl Fixture {
    fn new() -> Self {
        let (mut db, log) = DatabaseImpl::with_rebuild_log();
        let util_text = "public func answer(): int { return 7 }\n";
        let main_text = "module perf\nimport perf.util as util\nfunc stable(): int { return 1 }\nfunc main(): int { return util.answer() + stable() }\n";
        let util = db.new_file("perf/util.aru".into(), util_text.into());
        db.register_source_file("util.aru".into(), util);
        let entry = db.new_file("perf/main.aru".into(), main_text.into());
        db.register_source_file("main.aru".into(), entry);
        Self {
            db,
            log,
            entry,
            main: entry,
            util,
        }
    }
}

fn measure(fixture: &mut Fixture, mutate: impl FnOnce(&mut Fixture)) -> Window {
    mutate(fixture);
    fixture.log.clear();
    let started = Instant::now();
    let _ = arandu_query::passes::lower_amir(&fixture.db, fixture.entry);
    Window {
        elapsed: started.elapsed(),
        counts: fixture.log.counts(),
    }
}

fn load_budget(path: &Path) -> Result<Budget, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut values = BTreeMap::new();
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("{}:{}: expected key=value", path.display(), line_number + 1))?;
        if values.insert(key.trim(), value.trim()).is_some() {
            return Err(format!(
                "{}:{}: duplicate key",
                path.display(),
                line_number + 1
            ));
        }
    }
    let number = |key: &str| -> Result<usize, String> {
        values
            .get(key)
            .ok_or_else(|| format!("missing budget `{key}`"))?
            .parse::<usize>()
            .map_err(|error| format!("invalid budget `{key}`: {error}"))
    };
    Ok(Budget {
        samples: number("samples")?,
        warmup: number("warmup")?,
        endurance_revisions: number("endurance_revisions")?,
        max_noop_executes: number("max_noop_executes")?,
        max_edit_p95_executes: number("max_edit_p95_executes")?,
        max_rss_growth_bytes: u64::try_from(number("max_rss_growth_bytes")?)
            .map_err(|error| error.to_string())?,
    })
}

fn statistic_us(samples: &mut [Duration], percentile: usize) -> u128 {
    samples.sort_unstable();
    let index = percentile_index(samples.len(), percentile);
    samples.get(index).copied().unwrap_or_default().as_micros()
}

fn percentile_usize(samples: &mut [usize], percentile: usize) -> usize {
    samples.sort_unstable();
    samples
        .get(percentile_index(samples.len(), percentile))
        .copied()
        .unwrap_or_default()
}

pub(crate) fn percentile_index(len: usize, percentile: usize) -> usize {
    len.saturating_mul(percentile)
        .saturating_add(99)
        .checked_div(100)
        .unwrap_or_default()
        .saturating_sub(1)
        .min(len.saturating_sub(1))
}

#[cfg(target_os = "linux")]
fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn current_rss_bytes() -> Option<u64> {
    None
}

pub(crate) fn cpu_name() -> String {
    std::env::var("PROCESSOR_IDENTIFIER")
        .or_else(|_| std::env::var("HOSTTYPE"))
        .unwrap_or_else(|_| "unreported".into())
        .replace(['\n', '\r'], " ")
}

pub(crate) fn toolchain_name(workspace_root: &Path) -> String {
    fs::read_to_string(workspace_root.join("rust-toolchain.toml"))
        .ok()
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix("channel = "))
                .map(|value| value.trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| "unreported".into())
}

fn optional_metric(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".into(), |number| number.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile_index(9, 50), 4);
        assert_eq!(percentile_index(9, 95), 8);
        assert_eq!(percentile_index(0, 95), 0);
    }
}
