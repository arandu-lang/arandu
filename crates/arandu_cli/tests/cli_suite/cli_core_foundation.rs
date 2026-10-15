//! End-to-end coverage for the freestanding `std.core` foundation.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;

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
fn noncopy_element_slices_remain_usable_after_length_queries() {
    let output = run_source(
        "noncopySlice.aru",
        r#"module noncopySlice
import std.alloc.vec as vec
import std.alloc.string as strings
import std.core.slice as slice
struct Step { name: strings.String }
func main(): int {
    let mut steps = vec.Vec<Step>.new()
    steps.push(Step { name: strings.String.from("field") })
    let view = steps.asSlice()
    if slice.len<Step>(view) != 1 { return 1 }
    if slice.len<Step>(view) != 1 { return 2 }
    match slice.get<Step>(view, 0) {
        Option.Some(item) => {
            if item.name.len() != 5 { return 3 }
        }
        Option.None => { return 4 }
    }
    if slice.len<Step>(view) != 1 { return 5 }
    match steps.get(0) {
        Option.Some(item) => {
            if item.name.len() != 5 { return 6 }
        }
        Option.None => { return 7 }
    }
    match steps.get(1) {
        Option.Some(_) => { return 8 }
        Option.None => {}
    }
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn filesystem_reads_owned_binary_bytes_without_text_conversion() {
    let directory = crate::common::temp_dir("arandu-read-bytes").expect("temporary directory");
    let binary = directory.join("binary.dat");
    let empty = directory.join("empty.dat");
    let missing = directory.join("missing.dat");
    fs::write(&binary, [0, 128, 255]).expect("binary file");
    fs::write(&empty, []).expect("empty file");
    // JSON escaping also produces valid Arandu string literals for native paths.
    let binary_path = serde_json::to_string(&binary.to_string_lossy()).expect("binary path");
    let empty_path = serde_json::to_string(&empty.to_string_lossy()).expect("empty path");
    let missing_path = serde_json::to_string(&missing.to_string_lossy()).expect("missing path");
    let source = format!(
        r#"module readBytes
import std.fs as fs
func main(): int {{
    match fs.readToBytes({binary_path}) {{
        Ok(bytes) => {{
            if bytes.len() != 3 {{ return 1 }}
            let view = bytes.asSlice()
            if view[0] != 0 || view[1] != 128 || view[2] != 255 {{ return 2 }}
        }}
        Err(_) => {{ return 3 }}
    }}
    match fs.readToBytes({empty_path}) {{
        Ok(bytes) => {{ if !bytes.isEmpty() {{ return 4 }} }}
        Err(_) => {{ return 5 }}
    }}
    match fs.readToBytes({missing_path}) {{
        Ok(_) => {{ return 6 }}
        Err(_) => {{}}
    }}
    return 0
}}
"#
    );
    let output = run_source("readBytes.aru", &source);
    let _ = fs::remove_dir_all(directory);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn borrowed_string_bytes_and_partial_stdout_are_public_helpers() {
    let output = run_source(
        "public_text_helpers.aru",
        r#"module public_text_helpers
import std.io as io
import std.core.str as strings
import std.core.slice as slice
func main(): int {
    let text = "olá"
    let bytes = strings.asBytes(ref text)
    if slice.len<u8>(bytes) != 4 { return 1 }
    if bytes[0] != 111 { return 2 }
    let emptyText = ""
    let empty = strings.asBytes(ref emptyText)
    if slice.len<u8>(empty) != 0 { return 3 }
    io.print("hello")
    io.print("")
    io.print("\0world")
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"hello\0world");
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
    let five = fixed.Q16_16.fromInt(5 as i16)
    let two = fixed.Q16_16.fromInt(2 as i16)
    if five.mul(two).toRaw() != 655360 as i32 { return 1 }
    match five.div(two) {
        Option.Some(value) => {
            if value.toRaw() != 163840 as i32 { return 2 }
        }
        Option.None => { return 3 }
    }
    match five.div(fixed.Q16_16.fromRaw(0 as i32)) {
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
    let mut storage = vec.Vec<u8>.new()
    storage.push(0 as u8)
    storage.push(0 as u8)
    storage.push(0 as u8)
    storage.push(0 as u8)
    let mut input = vec.Vec<u8>.new()
    input.push(10 as u8)
    input.push(20 as u8)
    input.push(30 as u8)
    let storageSlice = storage.asSlice()
    let inputSlice = input.asSlice()
    let mut writer = io.SliceWriter.new(storageSlice)
    if writer.remaining() != 4 { return 10 }
    match writer.write(inputSlice) {
        Result.Ok(count) => { if count != 3 { return 1 } }
        Result.Err(_) => { return 2 }
    }
    if writer.written() != 3 { return 11 }
    if storageSlice[0] != 10 as u8 { return 12 }
    let mut reader = io.SliceReader.new(storageSlice)
    if reader.remaining() != 4 { return 13 }
    let mut output = vec.Vec<u8>.new()
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
    match reader.seek(io.SeekFrom.Current(-2147483647 - 1)) {
        Result.Err(_) => {}
        Result.Ok(_) => { return 6 }
    }
    match reader.seek(io.SeekFrom.Current(0)) {
        Result.Ok(position) => { if position != 3 { return 7 } }
        Result.Err(_) => { return 7 }
    }
    match reader.seek(io.SeekFrom.End(1)) {
        Result.Err(_) => {}
        Result.Ok(_) => { return 8 }
    }
    match reader.seek(io.SeekFrom.End(-1)) {
        Result.Ok(position) => { if position != 3 { return 9 } }
        Result.Err(_) => { return 10 }
    }
    match writer.seek(io.SeekFrom.Current(2147483647)) {
        Result.Err(_) => {}
        Result.Ok(_) => { return 11 }
    }
    match writer.seek(io.SeekFrom.Current(-1)) {
        Result.Ok(position) => { if position != 2 { return 12 } }
        Result.Err(_) => { return 12 }
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
