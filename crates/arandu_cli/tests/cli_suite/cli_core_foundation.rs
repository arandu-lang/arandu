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
    let storageSlice = storage.asMutSlice()
    let inputSlice = input.asSlice()
    let mut writer = io.SliceWriter.new(storageSlice)
    if writer.remaining() != 4 { return 10 }
    match writer.write(inputSlice) {
        Result.Ok(count) => { if count != 3 { return 1 } }
        Result.Err(_) => { return 2 }
    }
    if writer.written() != 3 { return 11 }
    if writer.buffer()[0] != 10 as u8 { return 12 }
    let mut reader = io.SliceReader.new(writer.buffer())
    if reader.remaining() != 4 { return 13 }
    let mut output = vec.Vec<u8>.new()
    output.push(0 as u8)
    output.push(0 as u8)
    output.push(0 as u8)
    let mut outputSlice = output.asMutSlice()
    match reader.read(outputSlice) {
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

#[test]
fn decimal_128_exact_financial_arithmetic() {
    let output = run_source(
        "core_decimal.aru",
        r#"module core_decimal

import std.core.decimal as dec

func parseOrZero(s: str): dec.Decimal {
    match dec.Decimal.fromStr(s) {
        Option.Some(v) => { return v }
        Option.None => { return dec.Decimal.zero() }
    }
}

func main(): int {
    // 1. Exact 0.1 + 0.2 == 0.3
    let a = parseOrZero("0.1")
    let b = parseOrZero("0.2")
    let c = parseOrZero("0.3")
    let sum = a.add(b)
    if !sum.eq(c) { return 1 }

    // 2. Scale normalization: 1.5 == 1.500
    let d1 = parseOrZero("1.5")
    let d2 = parseOrZero("1.500")
    if !d1.eq(d2) { return 2 }

    // 3. Banker's rounding (HalfEven): 2.5 -> 2, 3.5 -> 4
    let r25 = parseOrZero("2.5").round(0 as u32, dec.RoundingMode.HalfEven)
    let r35 = parseOrZero("3.5").round(0 as u32, dec.RoundingMode.HalfEven)
    if r25.toInt() != 2 { return 3 }
    if r35.toInt() != 4 { return 4 }

    // 4. Commercial rounding (HalfUp): 2.5 -> 3
    let r25Up = parseOrZero("2.5").round(0 as u32, dec.RoundingMode.HalfUp)
    if r25Up.toInt() != 3 { return 5 }

    // 5. Financial multiplication and cents: 19.99 * 3 == 59.97 (5997 cents)
    let price = dec.Decimal.fromCents(1999)
    let qty = dec.Decimal.fromInt(3)
    let total = price.mul(qty)
    if total.toCents() != 5997 { return 6 }

    // 6. Division: 10.50 / 2 == 5.25 (525 cents)
    let ten50 = dec.Decimal.fromCents(1050)
    let two = dec.Decimal.fromInt(2)
    match ten50.div(two) {
        Option.Some(half) => {
            if half.toCents() != 525 { return 7 }
        }
        Option.None => { return 8 }
    }

    // 7. Division by zero returns None
    match ten50.div(dec.Decimal.zero()) {
        Option.Some(_) => { return 9 }
        Option.None => {}
    }

    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "Decimal 128 program failed (code {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bit_methods_and_checked_abs_are_available_without_imports() {
    let output = run_source(
        "bits.aru",
        r#"module bits
func main(): int {
    let zero: u32 = 0
    let high: u64 = 9223372036854775808
    if zero.countOnes() != 0 { return 1 }
    if zero.leadingZeros() != 32 { return 2 }
    if zero.trailingZeros() != 32 { return 3 }
    if high.countOnes() != 1 { return 4 }
    if high.leadingZeros() != 0 { return 5 }
    if high.trailingZeros() != 63 { return 6 }
    if high.reverseBits() != (1 as u64) { return 7 }
    let min: int = -2147483647 - 1
    match min.checkedAbs() { Option.Some(_) => { return 8 } Option.None => {} }
    if (-123).abs() != 123 { return 9 }
    return 0
}
"#,
    );
    assert!(
        output.status.success(),
        "{:?} {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn absolute_value_traps_instead_of_returning_negative_minimum() {
    let output = run_source(
        "absMinimum.aru",
        r#"module absMinimum
func main(): int {
    let minimum: int = -2147483647 - 1
    let result = minimum.abs()
    if result < 0 { return 0 }
    return 0
}
"#,
    );
    assert!(
        !output.status.success(),
        "abs(MIN) must not return normally"
    );
    assert!(
        output.stderr.is_empty(),
        "expected execution trap, not a compile error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ordinary_functions_named_like_bit_intrinsics_keep_their_body() {
    let output = run_source(
        "bitNames.aru",
        r#"module bitNames
func bitCountOnes32(value: u32): u32 { return value + 7 }
func main(): int { if bitCountOnes32(5) != 12 { return 1 }; return 0 }
"#,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn foreign_bit_names_still_require_unsafe() {
    let output = run_source(
        "foreignBits.aru",
        r#"module foreignBits
extern "C" { func bitCountOnes32(value: u32): u32 }
func main(): int { return bitCountOnes32(5) as int }
"#,
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("O013"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bit_intrinsic_abi_requires_trusted_standard_library_source() {
    for declaration in [
        "func bitCountOnes32(value: u32): u32",
        "func bitCountOnes32(): u32",
        "func bitCountOnes32(value: u64): u32",
        "func bitCountOnes64(value: u64): u64",
        "func bitReverse64(value: u64): u32",
        "func bitReverse32(value: ref u32): u32",
        "func bitLeadingZeros32<T>(value: u32): u32",
        "func bitTrailingZeros64(a: u64, b: u64): u32",
    ] {
        let source = format!(
            "module std.core.invalidBits\nextern \"arandu-intrinsic\" {{ {declaration} }}\nfunc main(): int {{ return 0 }}"
        );
        let output = run_source("invalidBits.aru", &source);
        assert!(!output.status.success(), "accepted {declaration}");
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(
            errors.contains("U001")
                && errors.contains("requires a trusted standard-library source"),
            "{declaration}: {errors}"
        );
        assert!(!errors.contains("ICE"), "{declaration}: {errors}");
    }
}

#[test]
fn arena_get_cannot_duplicate_a_noncopy_owner() {
    let output = run_source(
        "arenaCopy.aru",
        r#"module arenaCopy
import std.alloc.gen_arena as arena
import std.alloc.string as strings
func main(): int {
    let mut values = arena.GenArena<strings.String>.new()
    let handle = values.insert(strings.String.from("owned"))
    let duplicate = values.get(handle)
    return duplicate.len() as int
}
"#,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Copy"),
        "expected Copy-bound diagnostic: {stderr}"
    );
}

#[test]
fn arena_borrow_keeps_noncopy_storage_owned_until_removal() {
    let output = run_source(
        "arenaBorrow.aru",
        r#"module arenaBorrow
import std.alloc.gen_arena as arena
import std.alloc.string as strings
func main(): int {
    let mut values = arena.GenArena<strings.String>.new()
    let handle = values.insert(strings.String.from("owned"))
    let borrowed = values.getRef(handle)
    if borrowed.len() != 5 { return 1 }
    match values.remove(handle) {
        Option.Some(value) => { if value.len() != 5 { return 2 } }
        Option.None => { return 3 }
    }
    return 0
}
"#,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn borrowed_projections_do_not_copy_payloads_or_repeat_side_effects() {
    // The standalone backend fixture declares its own compiler ABI. CLI users
    // obtain these operations from the loader-verified standard library instead.
    let source = include_str!("../../../../tests/corpus/borrowed_projections.aru").replacen(
        "extern \"arandu-intrinsic\" { func addressOf<T>(value: mut ref T): ptr[T]\nfunc refWrite<T>(value: mut ref T, replacement: T): void }",
        "from std.core.intrinsics import { addressOf }\nfrom std.core.mem import { refWrite }",
        1,
    );
    let output = run_source("borrowedProjections.aru", &source);
    assert!(
        output.status.success(),
        "{:?} {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
