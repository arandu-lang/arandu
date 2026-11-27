//! Test and benchmark reporters: human, JSON, and JUnit XML.

pub mod human;
pub mod json;
pub mod junit;

use arandu_codegen::testing::{TestEventV1, TestStatus};

pub use junit::junit_report;

use crate::test_runner::process::atomic_write_file;
use crate::test_runner::types::{JsonSummary, RunnerOptions, TestOutputFormat};

pub fn report(events: &[TestEventV1], options: &RunnerOptions) -> Result<(), String> {
    let total = events.len();
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut timed_out = 0;
    let mut crashed = 0;
    let mut total_duration_ms: u128 = 0;

    for event in events {
        total_duration_ms += event.duration.as_millis();
        match event.status {
            TestStatus::Passed => passed += 1,
            TestStatus::Failed => failed += 1,
            TestStatus::Skipped => skipped += 1,
            TestStatus::TimedOut => timed_out += 1,
            TestStatus::Crashed => crashed += 1,
        }
    }

    let summary = JsonSummary {
        total,
        passed,
        failed,
        skipped,
        timed_out,
        crashed,
        duration_ms: total_duration_ms,
    };

    match options.format {
        TestOutputFormat::Json => json::report_tests_json(events, options, summary),
        TestOutputFormat::Junit => {
            let encoded = junit_report(events, total_duration_ms);
            if let Some(output) = &options.output {
                atomic_write_file(output, encoded.as_bytes())?;
            } else {
                println!("{encoded}");
            }
            Ok(())
        }
        TestOutputFormat::Human => {
            human::report_summary_human(events, summary);
            Ok(())
        }
    }
}
