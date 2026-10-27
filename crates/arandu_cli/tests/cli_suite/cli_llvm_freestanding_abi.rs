//! `@Export` boundary on the three canonical freestanding targets.
//!
//! Each case drives the real driver and then the LLVM toolchain the product
//! is meant for: the generated header must compile for the target with
//! `-Werror`, the archive must expose exactly the exported symbols, and a C
//! caller must link against it with `ld.lld` without undefined references.
//! No emulator runs here; behavior on hardware is outside this evidence.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common;

/// Public target, clang triple, extra clang flags.
const TARGETS: [(&str, &str, &[&str]); 3] = [
    ("thumbv6m-none-eabi", "thumbv6m-none-eabi", &[]),
    ("thumbv7em-none-eabihf", "thumbv7em-none-eabihf", &[]),
    (
        "riscv32imc-unknown-none-elf",
        "riscv32-unknown-none-elf",
        &["-march=rv32imc", "-mabi=ilp32"],
    ),
];

const LIBRARY: &str = r#"
@Repr("C")
public struct Inner {
    a: u8
    b: u32
}

@Repr("C")
public struct Outer {
    tag: u16
    inner: Inner
    tail: u8
}

@Repr("C")
public struct Big {
    a: u64
    b: u64
    c: u64
    d: u32
}

@Export
func outer_tail(value: Outer): u32 {
    return value.inner.b + value.tail as u32
}

@Export
func make_outer(seed: u32): Outer {
    let inner = Inner { a: 1 as u8, b: seed }
    let made = Outer { tag: 7 as u16, inner: inner, tail: 3 as u8 }
    return made
}

@Export
func big_sum(value: Big): u64 {
    return value.a + value.b + value.c + value.d as u64
}

@Export
func make_big(seed: u64): Result<Big, i32> {
    if seed == 0 as u64 {
        return Result.Err(5 as i32)
    }
    return Result.Ok(Big { a: seed, b: seed + 1 as u64, c: seed + 2 as u64, d: 9 as u32 })
}

@Export
func narrow(x: u32): u8 {
    return x as u8
}

@Export
func narrow_signed(x: i32): i16 {
    return x as i16
}

@Export
func widen(x: u8): u32 {
    return x as u32
}

func helper(x: u32): u32 {
    return x
}
"#;

const CALLER: &str = r#"
#include "boundary.h"

volatile uint32_t sink;

int caller_main(void) {
    Outer o = make_outer(41);
    if (outer_tail(o) != 44) return 1;
    Big big;
    if (make_big(0, &big) != 5) return 2;
    if (make_big(10, &big) != 0) return 3;
    if (big_sum(big) != 42) return 4;
    sink = narrow(0x12C);
    sink = (uint32_t)(int32_t)narrow_signed(0x18000);
    sink = widen(200);
    return 0;
}

void _start(void) {
    sink = (uint32_t)caller_main();
    for (;;) {}
}
"#;

const LINKER_SCRIPT: &str = "MEMORY\n{\n  FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 256K\n  RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 64K\n}\nSECTIONS\n{\n  .text : { *(.text*) *(.rodata*) } > FLASH\n  .data : { *(.data*) } > RAM AT > FLASH\n  .bss : { *(.bss*) *(COMMON) } > RAM\n}\n";

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn llvm_toolchain_available() -> bool {
    ["clang", "ld.lld", "llvm-nm", "llvm-ar"]
        .iter()
        .all(|tool| tool_available(tool))
}

fn run_cli_in(dir: &Path, args: &[&str]) -> Output {
    let stdlib = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .unwrap();
    common::cli_command()
        .current_dir(dir)
        .args(args)
        .arg("--stdlib-path")
        .arg(stdlib)
        .output()
        .expect("cli should run")
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn new_library(tmp: &Path, name: &str, source: &str) -> PathBuf {
    let init = run_cli_in(tmp, &["new", name, "--lib", "--vcs=none"]);
    assert_success(&init, "new library");
    let project = tmp.join(name);
    fs::write(project.join("src/lib.aru"), source).unwrap();
    project
}

fn nm_globals(path: &Path, flag: &str) -> Vec<String> {
    let output = Command::new("llvm-nm")
        .args(["--extern-only", flag])
        .arg(path)
        .output()
        .expect("run llvm-nm");
    assert_success(&output, "llvm-nm");
    let mut names = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.ends_with(':'))
        .filter_map(|line| line.split_whitespace().last().map(str::to_owned))
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

/// Header, object, staticlib and a user-scripted ELF for each target; a C
/// caller compiled by clang for the same target links against the archive.
#[test]
fn freestanding_exports_compile_link_and_expose_only_exported_symbols() {
    if !llvm_toolchain_available() {
        return;
    }
    let tmp = common::temp_dir("arandu_freestanding_abi").unwrap();
    let project = new_library(&tmp, "boundary", LIBRARY);
    let script = tmp.join("memory.ld");
    fs::write(&script, LINKER_SCRIPT).unwrap();
    fs::write(tmp.join("caller.c"), CALLER).unwrap();
    for (target, clang_target, clang_args) in TARGETS {
        let dir = tmp.join(target);
        fs::create_dir_all(&dir).unwrap();
        let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let target_flag = format!("--target={target}");
        for (emit, file) in [
            ("--emit=header", "boundary.h"),
            ("--emit=obj", "boundary.o"),
            ("--crate-type=staticlib", "libboundary.a"),
        ] {
            let output = run_cli_in(
                &project,
                &["build", "-b", "llvm", &target_flag, emit, "-o", &path(file)],
            );
            assert_success(&output, &format!("{target} {emit}"));
        }

        // Nested by-value structs must be complete before use, and every
        // `_Static_assert` must hold for the target's own C compiler.
        let caller = Command::new("clang")
            .arg(format!("--target={clang_target}"))
            .args(clang_args)
            .args(["-ffreestanding", "-O2", "-Wall", "-Wextra", "-Werror", "-c"])
            .arg("-I")
            .arg(&dir)
            .arg(tmp.join("caller.c"))
            .arg("-o")
            .arg(dir.join("caller.o"))
            .output()
            .expect("run clang");
        assert_success(&caller, &format!("{target}: header + C caller"));

        assert_eq!(
            nm_globals(&dir.join("libboundary.a"), "--defined-only"),
            [
                "big_sum",
                "make_big",
                "make_outer",
                "narrow",
                "narrow_signed",
                "outer_tail",
                "widen"
            ],
            "{target}: only @Export may be global in the staticlib"
        );

        let firmware = dir.join("firmware.elf");
        let link = Command::new("ld.lld")
            .arg("-T")
            .arg(&script)
            .args(["-e", "_start"])
            .arg(dir.join("caller.o"))
            .arg(dir.join("libboundary.a"))
            .arg("-o")
            .arg(&firmware)
            .output()
            .expect("run ld.lld");
        assert_success(&link, &format!("{target}: ld.lld caller + staticlib"));
        assert!(
            nm_globals(&firmware, "--undefined-only").is_empty(),
            "{target}: firmware has undefined references"
        );

        // `--linker-script` links with LLD, never the host `ld`.
        let elf = run_cli_in(
            &project,
            &[
                "build",
                "-b",
                "llvm",
                &target_flag,
                "--linker-script",
                &script.to_string_lossy(),
                "-o",
                &path("library.elf"),
            ],
        );
        assert_success(&elf, &format!("{target}: --linker-script"));
    }
    let _ = fs::remove_dir_all(tmp);
}

/// Sub-word integers carry the extension attributes clang emits, so the
/// callee widens `u8`/`i16` returns as AAPCS32 and RISC-V require.
#[test]
fn exported_sub_word_scalars_carry_c_extension_attributes() {
    let tmp = common::temp_dir("arandu_freestanding_ext").unwrap();
    let project = new_library(&tmp, "ext", LIBRARY);
    for (target, _, _) in TARGETS {
        let ir = tmp.join(format!("{target}.ll"));
        let output = run_cli_in(
            &project,
            &[
                "build",
                "-b",
                "llvm",
                &format!("--target={target}"),
                "--emit=llvm-ir",
                "-o",
                &ir.to_string_lossy(),
            ],
        );
        assert_success(&output, target);
        let text = fs::read_to_string(&ir).unwrap();
        for expected in [
            "define zeroext i8 @narrow(i32",
            "define signext i16 @narrow_signed(i32",
            "define i32 @widen(i8 zeroext",
        ] {
            assert!(text.contains(expected), "{target}: missing `{expected}`");
        }
    }
    let riscv = fs::read_to_string(tmp.join("riscv32imc-unknown-none-elf.ll")).unwrap();
    assert!(
        riscv.contains("target triple = \"riscv32-unknown-none-elf\""),
        "clang rejects the Rust-style riscv32imc triple in the module header"
    );
    let _ = fs::remove_dir_all(tmp);
}

/// Names that cannot be published: C/C++ keywords, the runtime namespace,
/// and the same symbol exported from two modules of one package.
#[test]
fn export_names_reject_keywords_runtime_prefix_and_cross_module_clashes() {
    let tmp = common::temp_dir("arandu_export_names").unwrap();
    let project = new_library(&tmp, "names", "");
    let header = tmp.join("names.h").to_string_lossy().into_owned();
    let build = |source: &str| {
        fs::write(project.join("src/lib.aru"), source).unwrap();
        run_cli_in(&project, &["build", "--emit=header", "-o", &header])
    };
    for (source, reason) in [
        (
            "@Export(\"int\")\nfunc f(x: u32): u32 { return x }\n",
            "C or C++ keyword",
        ),
        (
            "@Export(\"ar_rt_alloc_aligned\")\nfunc f(x: u32): u32 { return x }\n",
            "uses the `ar_` prefix",
        ),
        (
            "@Export(\"__start\")\nfunc f(x: u32): u32 { return x }\n",
            "is reserved to the C",
        ),
    ] {
        let output = build(source);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{source} must be rejected");
        assert!(
            stderr.contains("T049") && stderr.contains(reason),
            "{source}: {stderr}"
        );
    }

    fs::write(
        project.join("src/other.aru"),
        "module names.other\n\n@Export(\"shared\")\npublic func b(x: u32): u32 { return x }\n",
    )
    .unwrap();
    // An export of an imported module is published, not silently internal.
    let output =
        build("import other\n\n@Export(\"root\")\nfunc a(x: u32): u32 { return other.b(x) }\n");
    assert_success(&output, "imported export");
    let text = fs::read_to_string(&header).unwrap();
    assert!(text.contains("uint32_t shared(uint32_t);"), "{text}");

    let output =
        build("import other\n\n@Export(\"shared\")\nfunc a(x: u32): u32 { return other.b(x) }\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a cross-module clash must fail");
    assert!(
        stderr.contains("T049") && stderr.contains("'shared' is claimed"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(tmp);
}
