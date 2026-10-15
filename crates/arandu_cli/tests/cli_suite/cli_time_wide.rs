#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use crate::common;

#[test]
fn monotonic_clock_and_duration_keep_more_than_signed_32_bit_nanoseconds() {
    let directory = common::temp_dir("arandu-time-wide").unwrap();
    let source = directory.join("main.aru");
    fs::write(
        &source,
        r#"
import std.time as time

func main(): int {
    let fiveSeconds = time.Duration.fromSecs(5)
    if fiveSeconds.asNanos() != 5000000000 as i64 { return 1 }
    if fiveSeconds.asMillis() != 5000 as i64 { return 3 }

    let start = time.Instant.now()
    while time.Instant.now().durationSince(start).asNanos() < 2200000000 as i64 {}
    let elapsed = time.Instant.now().durationSince(start).asNanos()
    if elapsed < 2200000000 as i64 { return 2 }
    return 0
}
"#,
    )
    .unwrap();

    let output = common::cli_command()
        .arg("run")
        .arg(&source)
        .output()
        .expect("run wide monotonic clock regression");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}
