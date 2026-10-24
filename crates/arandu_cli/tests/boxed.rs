//! Ownership and allocation regressions for `std.alloc.boxed`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
#[path = "common/wasm_host.rs"]
mod wasm_host;

use std::{fs, process::Command};

const SOURCE: &str = r#"module box_ownership
import std.alloc.boxed as boxed
import std.alloc.string as strings
import std.core.io as io
import std.core.intrinsics as intrinsics
struct Tracked { tag: int, text: strings.String }
@Destructor
func Tracked.destroy(self: own Tracked): void { io.println(self.tag) }
struct Empty {}
@Destructor
func Empty.destroy(self: own Empty): void { io.println(33) }
struct Pair { left: i64, right: i64 }
func dropOrdinary(): void {
    let value = boxed.Box<Tracked>.new(Tracked { tag: 11, text: strings.String.from("owned") })
    if value.get().text.len() != 5 { unsafe { intrinsics.abort() } }
}
func main(): int {
    dropOrdinary()
    let value = boxed.Box<Tracked>.new(Tracked { tag: 22, text: strings.String.from("moved") })
    let moved = value.intoInner()
    if moved.text.len() != 5 { return 1 }
    let zero = boxed.Box<Empty>.new(Empty {})
    let nested = boxed.Box<boxed.Box<Tracked>>.new(boxed.Box<Tracked>.new(Tracked { tag: 44, text: strings.String.from("nested") }))
    let rawOwner = boxed.Box<Tracked>.new(Tracked { tag: 55, text: strings.String.from("restored") })
    let raw = rawOwner.leak()
    let restored = unsafe { boxed.Box<Tracked>.fromRaw(raw) }
    if restored.get().text.len() != 8 { return 3 }
    let mut pair = boxed.Box<Pair>.new(Pair { left: 1, right: 2 })
    let access = pair.getMut()
    access.left = 73
    if pair.get().left != 73 || pair.get().right != 2 { return 2 }
    return 0
}
"#;

fn execute(source: &str, command: &str) -> std::process::Output {
    let directory = common::temp_dir("arandu-boxed").unwrap();
    let path = directory.join("boxed.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg(command)
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

#[test]
fn boxed_owns_noncopy_nested_and_zero_sized_values() {
    let output = execute(SOURCE, "run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let mut tags = output.lines().collect::<Vec<_>>();
    tags.sort_unstable();
    assert_eq!(tags, ["11", "22", "33", "44", "55"]);
}

#[test]
fn boxed_drops_and_mutable_borrows_in_generated_c() {
    if !Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return;
    }
    let emitted = execute(SOURCE, "emit-c");
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let directory = common::temp_dir("arandu-boxed-c").unwrap();
    let source = directory.join("boxed.c");
    fs::write(&source, emitted.stdout).unwrap();
    for optimization in ["-O0", "-O2"] {
        let executable = directory.join("boxed.exe");
        let mut command = Command::new("clang");
        command.args([optimization, "-fwrapv"]);
        if cfg!(unix) {
            command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
        }
        let compiled = command
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let output = Command::new(&executable).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        let mut tags = output.lines().collect::<Vec<_>>();
        tags.sort_unstable();
        assert_eq!(tags, ["11", "22", "33", "44", "55"]);
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn boxed_drops_and_allocations_in_wasm() {
    let emitted = execute(SOURCE, "emit-wasm");
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let run = wasm_host::run_main(&emitted.stdout);
    run.assert_returns(0, "Box contract in Wasmtime");
    let mut tags = run.stdout.lines().collect::<Vec<_>>();
    tags.sort_unstable();
    assert_eq!(tags, ["11", "22", "33", "44", "55"]);
}

#[test]
fn boxed_safe_api_rejects_ownership_and_borrow_violations() {
    for body in [
        "let first = boxed.Box<strings.String>.new(strings.String.from(\"owned\")); let second = first; first.get(); second.get()",
        "let owner = boxed.Box<strings.String>.new(strings.String.from(\"owned\")); let borrow = owner.get(); let value = owner.intoInner(); borrow.len()",
        "let mut owner = boxed.Box<strings.String>.new(strings.String.from(\"owned\")); let readView = owner.get(); let writeView = owner.getMut(); readView.len(); writeView.len()",
        "let forged = boxed.Box<int> { data: nil }; forged.get()",
        "let owner = boxed.Box<int>.new(42); let raw = owner.data",
        "let owner = boxed.Box<Holder>.new(Holder { text: strings.String.from(\"owned\") }); let duplicate = owner.get().text; duplicate.len()",
    ] {
        let source = format!(
            "module invalid_box\nimport std.alloc.boxed as boxed\nimport std.alloc.string as strings\nstruct Holder {{ text: strings.String }}\nfunc main(): int {{ {body}; return 0 }}"
        );
        let output = execute(&source, "check");
        assert!(
            !output.status.success(),
            "unsafe box program was accepted: {body}"
        );
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains("O0") || diagnostics.contains("T041"),
            "expected ownership or private-field rejection, got: {diagnostics}"
        );
    }
}

#[test]
fn boxed_borrows_cannot_escape_the_owner() {
    for (receiver, result) in [("get", "ref int"), ("getMut", "mut ref int")] {
        let source = format!(
            "module escaping_box\nimport std.alloc.boxed as boxed\nfunc escape(): {result} {{ let mut owner = boxed.Box<int>.new(42); return owner.{receiver}() }}\nfunc main(): int {{ return 0 }}"
        );
        let output = execute(&source, "check");
        assert!(!output.status.success(), "escaping {receiver} was accepted");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains("O0"),
            "expected ownership rejection, got: {diagnostics}"
        );
    }
}

#[test]
fn boxed_raw_reconstruction_requires_explicit_unsafe() {
    let output = execute(
        "module unsafe_box_factory\nimport std.alloc.boxed as boxed\nfunc main():int {let owner=boxed.Box<int>.new(42);let raw=owner.leak();let restored=boxed.Box<int>.fromRaw(raw);return *restored.get()-42}",
        "check",
    );
    assert!(
        !output.status.success(),
        "Box.fromRaw accepted unproved raw ownership in safe code"
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("O013") && diagnostics.contains("unsafe"),
        "{diagnostics}"
    );
}
