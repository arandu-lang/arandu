//! Human-readable console reporting for tests and benchmarks.

use arandu_codegen::testing::{BenchmarkEventV1, CapturedOutput, TestEventV1, TestStatus};
use std::io::{self, Write};

use crate::test_runner::statistics::{benchmark_stats, format_ns_per_op};
use crate::test_runner::types::{
    BenchmarkComparisonReport, BenchmarkComparisonStatus, JsonSummary,
};

pub fn report_run_header_human(total: usize) {
    let mut stderr = io::stderr().lock();
    let _ = write_run_header_human(&mut stderr, total);
    let _ = stderr.flush();
}

pub fn report_case_human(event: &TestEventV1, completed: usize, total: usize) {
    let mut stderr = io::stderr().lock();
    let _ = write_case_human(&mut stderr, event, completed, total);
    let _ = stderr.flush();
}

pub fn report_summary_human(events: &[TestEventV1], summary: JsonSummary) {
    let mut stderr = io::stderr().lock();
    let _ = write_summary_human(&mut stderr, events, summary);
    let _ = stderr.flush();
}

fn write_run_header_human(out: &mut impl Write, total: usize) -> io::Result<()> {
    writeln!(
        out,
        "running {} test{}",
        total,
        if total == 1 { "" } else { "s" }
    )
}

fn write_case_human(
    out: &mut impl Write,
    event: &TestEventV1,
    completed: usize,
    total: usize,
) -> io::Result<()> {
    writeln!(
        out,
        "[{completed}/{total}] {} {} ({:?})",
        status_name(event.status),
        event.id,
        event.duration
    )?;
    if let Some(failure) = &event.failure {
        if let Some(loc) = &failure.location {
            writeln!(out, "    location: {loc}")?;
        }
        if let (Some(exp), Some(act)) = (&failure.expected, &failure.actual) {
            writeln!(out, "    expected: `{exp}`")?;
            writeln!(out, "    actual:   `{act}`")?;
        }
        if !failure.message.is_empty() {
            writeln!(out, "    message:  {}", failure.message)?;
        }
    }
    for failure in &event.secondary_failures {
        writeln!(out, "    secondary: {}", failure.message)?;
    }
    for log in &event.logs {
        writeln!(out, "    log: {log}")?;
    }
    if event.logs_truncated {
        writeln!(out, "    log: <truncated>")?;
    }
    if is_unsuccessful(event.status) {
        write_captured_output(out, "stdout", &event.stdout)?;
        write_captured_output(out, "stderr", &event.stderr)?;
    }
    Ok(())
}

fn write_captured_output(
    out: &mut impl Write,
    label: &str,
    captured: &CapturedOutput,
) -> io::Result<()> {
    let text = String::from_utf8_lossy(&captured.bytes);
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() && !captured.truncated {
        return Ok(());
    }
    writeln!(out, "    --- {label} ---")?;
    for line in trimmed.lines() {
        writeln!(out, "    {line}")?;
    }
    if captured.truncated {
        writeln!(out, "    <{label} truncated>")?;
    }
    Ok(())
}

fn write_summary_human(
    out: &mut impl Write,
    events: &[TestEventV1],
    summary: JsonSummary,
) -> io::Result<()> {
    let failed_cases: Vec<&TestEventV1> = events
        .iter()
        .filter(|event| is_unsuccessful(event.status))
        .collect();
    if !failed_cases.is_empty() {
        writeln!(out)?;
        writeln!(out, "failures:")?;
        for event in failed_cases {
            if let Some(detail) = failure_summary_line(event) {
                writeln!(
                    out,
                    "    {} {}: {}",
                    status_name(event.status),
                    event.id,
                    detail
                )?;
            } else {
                writeln!(out, "    {} {}", status_name(event.status), event.id)?;
            }
        }
        writeln!(out)?;
    }
    writeln!(
        out,
        "test result: {}. {} passed; {} failed; {} skipped; {} timed out; {} crashed",
        if summary.failed == 0 && summary.timed_out == 0 && summary.crashed == 0 {
            "ok"
        } else {
            "FAILED"
        },
        summary.passed,
        summary.failed,
        summary.skipped,
        summary.timed_out,
        summary.crashed,
    )
}

fn is_unsuccessful(status: TestStatus) -> bool {
    !matches!(status, TestStatus::Passed | TestStatus::Skipped)
}

fn failure_summary_line(event: &TestEventV1) -> Option<String> {
    if let Some(failure) = &event.failure
        && let Some(line) = failure.message.lines().find(|line| !line.trim().is_empty())
    {
        return Some(line.trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&event.stderr.bytes);
    stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

pub fn report_benchmarks_human(
    events: &[BenchmarkEventV1],
    comparison: Option<&BenchmarkComparisonReport>,
) {
    for event in events {
        if let Some(failure) = &event.failure {
            eprintln!("FAILED {}: {failure}", event.id);
            continue;
        }
        let (median, mad, p95) = benchmark_stats(event);
        let median = median.unwrap_or(0.0);
        let mad = mad.unwrap_or(0.0);
        eprintln!(
            "bench {}: median {}; MAD {}; p95 {}; {} samples",
            event.id,
            format_ns_per_op(median),
            format_ns_per_op(mad),
            format_ns_per_op(p95.unwrap_or(0.0)),
            event.samples.len()
        );
        if median > 0.0 && mad / median > 0.10 {
            eprintln!(
                "warning: benchmark {} is noisy (MAD exceeds 10% of median)",
                event.id
            );
        }
    }
    if let Some(comparison) = comparison {
        if comparison.status == BenchmarkComparisonStatus::Compared {
            for case in &comparison.cases {
                eprintln!(
                    "compare {}: {:+.2}% ({:?}, threshold {:.2}%, uncertainty {:.2}%)",
                    case.id,
                    case.delta_percent,
                    case.classification,
                    case.effective_threshold_percent,
                    case.uncertainty_percent
                );
            }
            eprintln!(
                "benchmark comparison: {} regression(s) against baseline `{}`{}",
                comparison.regressions,
                comparison.baseline,
                if comparison.dry_run { " (dry-run)" } else { "" }
            );
        } else {
            eprintln!(
                "benchmark comparison: {:?} for baseline `{}`",
                comparison.status, comparison.baseline
            );
        }
    }
}

pub fn status_name(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Passed => "passed",
        TestStatus::Failed => "failed",
        TestStatus::Skipped => "skipped",
        TestStatus::TimedOut => "timed_out",
        TestStatus::Crashed => "crashed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_codegen::testing::TestFailure;
    use std::time::Duration;

    fn sample_event(id: &str, status: TestStatus) -> TestEventV1 {
        TestEventV1 {
            sequence: 0,
            id: id.to_string(),
            status,
            duration: Duration::from_millis(12),
            stdout: CapturedOutput::default(),
            stderr: CapturedOutput::default(),
            failure: None,
            secondary_failures: Vec::new(),
            logs: Vec::new(),
            logs_truncated: false,
        }
    }

    #[test]
    fn human_case_hides_output_on_pass_and_shows_captured_output_on_failure() {
        let mut passing = sample_event("pkg::test::ok", TestStatus::Passed);
        passing.stdout.bytes = b"noisy stdout\n".to_vec();
        passing.stderr.bytes = b"noisy stderr\n".to_vec();

        let mut pass_buf = Vec::new();
        write_case_human(&mut pass_buf, &passing, 1, 2).expect("write passing case");
        let pass_text = String::from_utf8(pass_buf).expect("utf8");
        assert!(pass_text.contains("[1/2] passed pkg::test::ok"));
        assert!(!pass_text.contains("noisy stdout"));
        assert!(!pass_text.contains("noisy stderr"));

        let mut failing = sample_event("pkg::test::boom", TestStatus::Crashed);
        failing.failure = Some(TestFailure::simple(
            "test process terminated without reporting a result",
        ));
        failing.stdout.bytes = b"debug line\n".to_vec();
        failing.stderr.bytes = b"panic: index out of bounds\n".to_vec();
        failing.stderr.truncated = true;

        let mut fail_buf = Vec::new();
        write_case_human(&mut fail_buf, &failing, 2, 2).expect("write failing case");
        let fail_text = String::from_utf8(fail_buf).expect("utf8");
        assert!(fail_text.contains("[2/2] crashed pkg::test::boom"));
        assert!(fail_text.contains("--- stdout ---"));
        assert!(fail_text.contains("    debug line"));
        assert!(fail_text.contains("--- stderr ---"));
        assert!(fail_text.contains("    panic: index out of bounds"));
        assert!(fail_text.contains("    <stderr truncated>"));
    }

    #[test]
    fn human_summary_lists_failures_before_final_result_line() {
        let mut failed = sample_event("pkg::test::a_fail", TestStatus::Failed);
        failed.failure = Some(TestFailure::simple("expected 42, got 7"));
        let mut crashed = sample_event("pkg::test::b_crash", TestStatus::Crashed);
        crashed.stderr.bytes = b"panic: custom abort\n".to_vec();
        let passed = sample_event("pkg::test::c_pass", TestStatus::Passed);

        let events = vec![failed, crashed, passed];
        let summary = JsonSummary {
            total: 3,
            passed: 1,
            failed: 1,
            skipped: 0,
            timed_out: 0,
            crashed: 1,
            duration_ms: 36,
        };

        let mut buf = Vec::new();
        write_summary_human(&mut buf, &events, summary).expect("write summary");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("failures:\n"));
        assert!(text.contains("    failed pkg::test::a_fail: expected 42, got 7\n"));
        assert!(text.contains("    crashed pkg::test::b_crash: panic: custom abort\n"));
        assert!(!text.contains("pkg::test::c_pass"));
        assert!(text.contains(
            "test result: FAILED. 1 passed; 1 failed; 0 skipped; 0 timed out; 1 crashed"
        ));
    }
}
