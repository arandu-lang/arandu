//! BV.3 product regressions for safe Slice/String views.
#![allow(clippy::expect_used)]

use std::fs;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn invoke(mode: &str, source: &str) -> Output {
    invoke_with_args(mode, source, &[])
}

fn invoke_with_args(mode: &str, source: &str, extra_args: &[&str]) -> Output {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "arandu-borrowed-view-{}-{id}.aru",
        std::process::id()
    ));
    fs::write(&path, source).expect("write borrowed-view fixture");
    let mut command = Command::new(env!("CARGO_BIN_EXE_arandu_cli"));
    command
        .current_dir(workspace_root())
        .args([mode, path.to_str().expect("UTF-8 temp path")])
        .args(extra_args);
    let output = command.output().expect("run Arandu check");
    let _ = fs::remove_file(path);
    output
}

fn check(source: &str) -> Output {
    invoke("check", source)
}

const DIRECTORY_NAME_VIEWS_SOURCE: &str = r#"module tests.borrowed_views.directory_multiple_names
import std.fs as fs

@Effects(FileRead, Foreign)
func main(): int {
    match fs.readDir(".") {
        Ok(listing) => {
            if listing.count() < 2 { return 1 }
            let first = listing.nameStr(0)
            let second = listing.nameStr(1)
            if *first == *second { return 2 }
            return 0
        }
        Err(_) => { return 3 }
    }
}
"#;

#[test]
fn vec_slice_is_a_zero_copy_borrowed_view() {
    let output = check(
        r#"module tests.borrowed_views.valid
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 7)
    let view = vec.asSlice<int>(values)
    return slice.len<int>(view) as int
}
"#,
    );
    assert!(
        output.status.success(),
        "valid borrowed slice rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_executes_slice_len_through_the_public_api() {
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.run
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 7)
    let view = vec.asSlice<int>(values)
    return slice.len<int>(view) as int
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "JIT slice execution failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_formatter_writes_string_bytes_and_rejects_partial_write() {
    let output = invoke(
        "run",
        r#"module tests.formatter.write_string
import std.core.fmt as fmt
import std.alloc.vec as vec

func main(): int {
    let mut storage = vec.new<u8>()
    let mut i: uint = 0
    while i < 5 {
        vec.push<u8>(storage, 0 as u8)
        i = i + 1
    }
    let view = vec.asSlice<u8>(storage)
    let mut writer = fmt.newFormatter(view)
    match writer.writeStr("hey") {
        Ok(3) => {}
        _ => { return 1 }
    }
    if view[0] != (104 as u8) || view[1] != (101 as u8) || view[2] != (121 as u8) {
        return 2
    }
    if writer.len() != 3 { return 3 }
    match writer.writeStr("bye") {
        Err(_) => {}
        _ => { return 4 }
    }
    if writer.len() != 3 || view[0] != (104 as u8) { return 5 }
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "JIT formatter string write failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_string_truncate_preserves_utf8_boundaries() {
    let output = invoke(
        "run",
        r#"module tests.string.truncate_utf8
import std.alloc.string as strings

func main(): int {
    let mut value = strings.from("aéz")
    value.truncate(2)
    if value.len() != 4 { return 1 }
    value.truncate(3)
    if value.len() != 3 || *value.asStr() != "aé" { return 2 }
    value.truncate(0)
    if value.len() != 0 { return 3 }
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "JIT UTF-8 truncate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_executes_borrowed_element_and_subslice() {
    let output = invoke("run", include_str!("fixtures/borrowed_views_bv3.aru"));
    assert_eq!(
        output.status.code(),
        Some(8),
        "JIT borrowed element/subslice failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_indexes_a_slice_stored_in_a_struct_field() {
    for optimized in [false, true] {
        let extra_args = if optimized { &["--opt"][..] } else { &[] };
        let output = invoke_with_args(
            "run",
            include_str!("fixtures/struct_slice_field.aru"),
            extra_args,
        );
        assert_eq!(
            output.status.code(),
            Some(7),
            "optimized={optimized}: JIT struct slice indexing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn live_slice_blocks_owner_reallocation() {
    let output = check(
        r#"module tests.borrowed_views.reallocation
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 1)
    let view = vec.asSlice<int>(values)
    vec.push<int>(values, 2)
    return slice.len<int>(view) as int
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "reallocation with live view passed"
    );
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected ownership conflict, got: {stderr}"
    );
}

#[test]
fn slice_of_local_owner_cannot_escape() {
    let output = check(
        r#"module tests.borrowed_views.escape
import std.alloc.vec as vec

func escape(): []int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 1)
    return vec.asSlice<int>(values)
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "dangling slice escaped");
    assert!(stderr.contains("O010"), "expected O010, got: {stderr}");
}

#[test]
fn jit_consumes_string_from_free_and_associated_ctor() {
    // `from` is a soft keyword: it must work as both the stdlib free function
    // (`strings.from`) and the associated constructor (`strings.String.from`).
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.string_from
import std.alloc.string as strings

func main(): int {
    let a = strings.from("free")
    let b = strings.String.from("associated")
    let lenA = strings.len(a)
    let lenB = strings.len(b)
    if lenA != 4 {
        return 10
    }
    if lenB != 10 {
        return 11
    }
    return 12
}
"#,
    );
    let code = output.status.code();
    assert_eq!(
        code,
        Some(12),
        "JIT String.from (free/associated) failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn string_views_block_mutation() {
    let output = check(
        r#"module tests.borrowed_views.string
import std.alloc.string as strings
import std.core.slice as slice

func main(): int {
    let mut text = strings.new()
    let bytes = strings.asBytes(text)
    strings.pushScalar(text, 'a')
    return slice.len<u8>(bytes) as int
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "String mutated with a live byte view"
    );
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected ownership conflict, got: {stderr}"
    );
}

#[test]
fn method_autoref_rejects_an_immutable_value_receiver() {
    let output = check(
        r#"module tests.borrowed_views.immutable_method_receiver
import std.alloc.string as strings

func main(): int {
    let text = strings.new()
    text.pushScalar('a')
    return 0
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "immutable receiver was mutably borrowed"
    );
    assert!(
        stderr.contains("T026") && stderr.contains("cannot mutably borrow"),
        "expected exclusive receiver diagnostic, got: {stderr}"
    );
}

#[test]
fn generic_method_autoref_rejects_an_immutable_value_receiver() {
    let output = check(
        r#"module tests.borrowed_views.immutable_generic_method_receiver
import std.alloc.vec as vec

func main(): int {
    let values = vec.new<int>()
    values.push<int>(1)
    return 0
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "immutable generic receiver was mutably borrowed"
    );
    assert!(
        stderr.contains("T026") && stderr.contains("cannot mutably borrow"),
        "expected exclusive receiver diagnostic, got: {stderr}"
    );
}

#[test]
fn method_autoref_accepts_a_mutable_value_receiver() {
    let output = check(
        r#"module tests.borrowed_views.mutable_method_receiver
import std.alloc.string as strings

func main(): int {
    let mut text = strings.new()
    text.pushScalar('a')
    return 0
}
"#,
    );
    assert!(
        output.status.success(),
        "mutable receiver was rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn method_receiver_accepts_an_immutable_exclusive_reference_binding() {
    let output = check(
        r#"module tests.borrowed_views.exclusive_reference_receiver
import std.alloc.string as strings

func main(): int {
    let mut text = strings.new()
    let alias = &mut text
    alias.pushScalar('a')
    return 0
}
"#,
    );
    assert!(
        output.status.success(),
        "exclusive reference receiver was rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn owned_path_join_runs_and_destroys_its_buffer() {
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.owned_path_join
import std.path as path
import std.alloc.string as strings

func main(): int {
    let joined: strings.String = path.joinOwned("/tmp", "owned")
    let s: str = *joined.asStr()
    if path.fileName(s) != "owned" {
        return 1
    }
    joined.destroy()
    return 0
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "owned path join failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn directory_name_views_coexist_without_mutating_the_listing() {
    let output = invoke("run", DIRECTORY_NAME_VIEWS_SOURCE);
    assert_eq!(
        output.status.code(),
        Some(0),
        "two directory name views failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn c_backend_directory_name_views_coexist() {
    if !Command::new("gcc")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return;
    }
    let emitted = invoke_with_args("emit-c", DIRECTORY_NAME_VIEWS_SOURCE, &["--layout=host"]);
    assert!(
        emitted.status.success(),
        "C emission failed: {}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let stem = std::env::temp_dir().join(format!(
        "arandu-directory-names-{}-{id}",
        std::process::id()
    ));
    let c_file = stem.with_extension("c");
    fs::write(&c_file, &emitted.stdout).expect("write C source");
    let compiled = Command::new("gcc")
        .arg(&c_file)
        .arg("-o")
        .arg(&stem)
        .output()
        .expect("compile emitted C");
    assert!(
        compiled.status.success(),
        "C compilation failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(&stem)
        .current_dir(workspace_root())
        .output()
        .expect("run emitted C");
    let _ = fs::remove_file(c_file);
    let _ = fs::remove_file(stem);
    assert_eq!(
        executed.status.code(),
        Some(0),
        "C directory name views failed: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
}

#[test]
fn directory_name_view_blocks_listing_destruction() {
    let output = check(
        r#"module tests.borrowed_views.directory
import io
import std.fs as fs

@Effects(FileRead, Foreign)
func main(): int {
    match fs.readDir(".") {
        Ok(mut listing) => {
            let name = listing.nameStr(0)
            listing.destroy()
            io.println(*name)
        }
        Err(_) => {}
    }
    return 0
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "dangling directory name view passed"
    );
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected ownership conflict, got: {stderr}"
    );
}

#[test]
fn jit_appends_utf8_and_reads_string_view() {
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.string_run
import std.alloc.string as strings

func main(): int {
    let mut value = strings.new()
    if !strings.pushStr(value, "olá") {
        return 1
    }
    let view = strings.asStr(value)
    if *view == "olá" {
        return strings.len(value) as int
    }
    return 2
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(4),
        "JIT String.pushStr/asStr failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn c_backend_erases_view_intrinsics_to_the_ptr_len_abi() {
    let output = invoke(
        "emit-c",
        r#"module tests.borrowed_views.c_backend
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 5)
    let view = vec.asSlice<int>(values)
    return slice.len<int>(view) as int
}
"#,
    );
    assert!(
        output.status.success(),
        "C emission failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = String::from_utf8_lossy(&output.stdout);
    assert!(
        c.matches("sliceFromRaw").count() <= 1,
        "slice constructor remained as a runtime call"
    );
    assert!(
        c.contains("ArType_Slice_"),
        "slice ABI type was not emitted"
    );
    assert!(
        !c.contains("*(T**)((uint8_t*)"),
        "generic field templates leaked into a monomorphic C function"
    );
}
