//! End-to-end coverage for the freestanding `std.core` foundation.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn run_source(name: &str, source: &str) -> std::process::Output {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "arandu-core-foundation-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("create isolated test directory");
    let path = directory.join(name);
    fs::write(&path, source).expect("write test source");
    let output = common::cli_command()
        .args(["run", path.to_str().expect("valid UTF-8 path")])
        .output()
        .expect("run Arandu program");
    let _ = fs::remove_dir_all(directory);
    output
}

#[test]
fn core_string_algorithms_run_without_runtime_string_symbols() {
    let output = run_source(
        "core_str.aru",
        r#"module core_str

import std.core.str as strings

func main(): int {
    if !strings.startsWith("arandu", "ara") { return 1 }
    if !strings.endsWith("arandu", "ndu") { return 2 }
    if !strings.contains("a\0bcd", "bcd") { return 3 }
    if strings.contains("arandu", "xyz") { return 4 }
    match strings.find("bananana", "nana") {
        Option.Some(index) => {
            if index != 2 { return 5 }
        }
        Option.None => { return 6 }
    }
    match strings.find("abc", "") {
        Option.Some(index) => {
            if index != 0 { return 7 }
        }
        Option.None => { return 8 }
    }
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "core string program failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn q16_16_uses_widened_intermediates() {
    let output = run_source(
        "core_fixed.aru",
        r#"module core_fixed

import std.core.fixed as fixed

func main(): int {
    let five = fixed.fromInt(5 as i16)
    let two = fixed.fromInt(2 as i16)
    if five.mul(two).toRaw() != 655360 as i32 { return 1 }
    match five.div(two) {
        Option.Some(value) => {
            if value.toRaw() != 163840 as i32 { return 2 }
        }
        Option.None => { return 3 }
    }
    match five.div(fixed.fromRaw(0 as i32)) {
        Option.Some(_) => { return 4 }
        Option.None => {}
    }
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "Q16.16 program failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn slice_reader_and_writer_copy_and_seek_in_memory() {
    let output = run_source(
        "core_io.aru",
        r#"module core_io

import std.core.io as io
import std.alloc.vec as vec

func main(): int {
    let mut storage = vec.new<u8>()
    storage.push(0 as u8)
    storage.push(0 as u8)
    storage.push(0 as u8)
    storage.push(0 as u8)
    let mut input = vec.new<u8>()
    input.push(10 as u8)
    input.push(20 as u8)
    input.push(30 as u8)
    let storageSlice = storage.asSlice()
    let inputSlice = input.asSlice()
    let mut writer = io.newSliceWriter(storageSlice)
    if writer.remaining() != 4 { return 10 }
    match writer.write(inputSlice) {
        Result.Ok(count) => { if count != 3 { return 1 } }
        Result.Err(_) => { return 2 }
    }
    if writer.written() != 3 { return 11 }
    if storageSlice[0] != 10 as u8 { return 12 }
    let mut reader = io.newSliceReader(storageSlice)
    if reader.remaining() != 4 { return 13 }
    let mut output = vec.new<u8>()
    output.push(0 as u8)
    output.push(0 as u8)
    output.push(0 as u8)
    let mut outputSlice = output.asSlice()
    match reader.read(mut ref outputSlice) {
        Result.Ok(count) => { if count != 3 { return 3 } }
        Result.Err(_) => { return 4 }
    }
    if output[0] != 10 as u8 || output[1] != 20 as u8 || output[2] != 30 as u8 {
        return 5
    }
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "core I/O program failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
