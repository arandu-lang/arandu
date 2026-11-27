//! E2E and diagnostic regressions for Onda D (text ergonomics, `mut []T`, abort messages, Option helpers, and target alloc ceilings).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::process::{Command, Output};

fn run_source(name: &str, source: &str) -> Output {
    let directory = common::temp_dir(&format!("arandu-wave-d-{name}")).unwrap();
    let path = directory.join("main.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

fn check_source(name: &str, source: &str) -> Output {
    let directory = common::temp_dir(&format!("arandu-wave-d-check-{name}")).unwrap();
    let path = directory.join("main.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

#[test]
fn text_ergonomics_display_interpolation_and_str_ordering_work_e2e() {
    let source = r#"module wave_d_text

import std.alloc.string as strings
import std.text as text
import std.core.fmt as fmt

struct Point {
    public x: int,
    public y: int,
}

func Point.fmt(self: ref Point, f: mut ref fmt.Formatter): Result<usize, fmt.FmtError> {
    let n1 = f.writeStr("(")?
    let n2 = f.writeInt(self.x as i64)?
    let n3 = f.writeStr(", ")?
    let n4 = f.writeInt(self.y as i64)?
    let n5 = f.writeStr(")")?
    return Result.Ok(n1 + n2 + n3 + n4 + n5)
}

func main(): int {
    let s = strings.String.from("arandu")
    let view: str = s.asStr()
    if view != "arandu" { return 1 }

    let mut sb = text.StringBuilder.new()
    let _ = sb.append("hello")
    let _ = sb.append("!")
    let sbView: str = sb.asStr()
    if sbView != "hello!" { return 2 }

    let p = Point { x: 3, y: 7 }
    let formatted: str = "pt=$p"
    if formatted != "pt=(3, 7)" { return 3 }

    if !("alpha" < "beta") { return 4 }
    if !("alpha" <= "alpha") { return 5 }
    if !("zeta" > "gamma") { return 6 }
    if !("delta" >= "delta") { return 7 }
    if "beta" < "alpha" { return 8 }
    if "a" >= "ab" { return 9 }
    return 0
}
"#;
    let output = run_source("text", source);
    assert!(
        output.status.success(),
        "JIT run failed (status {:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn string_as_str_rejects_dangling_borrow_on_return() {
    let escaping = r#"module wave_d_dangling_str
import std.alloc.string as strings

func makeDangling(): str {
    let local = strings.String.from("temp")
    return local.asStr()
}

func main(): int {
    let _ = makeDangling()
    return 0
}
"#;
    let out = check_source("dangling_str", escaping);
    assert!(
        !out.status.success(),
        "expected dangling `str` return to be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("O001") || stderr.contains("O006") || stderr.contains("O010"),
        "expected ownership/escape error for dangling `str`, got: {stderr}"
    );
}

#[test]
fn mut_slice_methods_and_simd_store_unaligned_work_e2e() {
    let source = r#"module wave_d_mut_slice

import std.alloc.vec as vec
import std.core.slice as slice
import std.core.simd as simd

func main(): int {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    v.push(30)
    v.push(40)

    let mut ms = v.asMutSlice()
    if ms.len() != (4 as usize) { return 1 }
    if let Some(first) = ms.firstMut() {
        *first = 11
    } else {
        return 2
    }
    if let Some(last) = ms.lastMut() {
        *last = 44
    } else {
        return 3
    }
    if let Some(mid) = ms.getMut(1 as usize) {
        *mid = 22
    } else {
        return 4
    }
    if !ms.swap(0 as usize, 3 as usize) { return 5 }
    ms.reverse()
    // After edits: [11, 22, 30, 44] -> swap(0,3): [44, 22, 30, 11] -> reverse: [11, 30, 22, 44]
    if ms[0] != 11 || ms[1] != 30 || ms[2] != 22 || ms[3] != 44 {
        return 6
    }
    ms.fill(7)
    if ms[0] != 7 || ms[3] != 7 { return 7 }

    let mut fv = vec.Vec<f32>.new()
    fv.push(0.0 as f32)
    fv.push(0.0 as f32)
    fv.push(0.0 as f32)
    fv.push(0.0 as f32)
    let fms = fv.asMutSlice()
    if !simd.F32x4.splat(5.5 as f32).storeUnaligned(fms) { return 8 }
    let fs = fv.asSlice()
    if fs[0] != (5.5 as f32) || fs[3] != (5.5 as f32) { return 9 }

    return 0
}
"#;
    let output = run_source("mut_slice", source);
    assert!(
        output.status.success(),
        "JIT run failed (status {:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn option_take_replace_and_target_alloc_ceilings_work_e2e() {
    let source = r#"module wave_d_option_and_alloc

import std.core.option as option
import std.core.mem as mem

func main(): int {
    let mut opt = option.Option.Some(10)
    let replaced = opt.replace(25)
    match replaced {
        Some(v) => {
            if v != 10 { return 1 }
        }
        None => { return 2 }
    }
    let taken = opt.take()
    match taken {
        Some(v) => {
            if v != 25 { return 3 }
        }
        None => { return 4 }
    }
    if opt.isSome() { return 5 }

    let maxBytes = mem.maxAllocBytes()
    let maxInts = mem.maxAllocElements<int>()
    if maxBytes == (0 as usize) || maxInts == (0 as usize) { return 6 }
    if maxInts > maxBytes { return 7 }

    let okOpt = option.Option.Some(99).expectOrAbort("must be present")
    if okOpt != 99 { return 8 }
    let okRes: Result<int, int> = Result.Ok(77)
    if okRes.expectOrAbort("must be ok") != 77 { return 9 }
    return 0
}
"#;
    let output = run_source("option_alloc", source);
    assert!(
        output.status.success(),
        "JIT run failed (status {:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn expect_or_abort_writes_reason_to_stderr_in_jit_and_c() {
    let option_abort_src = r#"module wave_d_option_abort
import std.core.option as option

func main(): int {
    let empty: option.Option<int> = option.Option.None
    let _ = empty.expectOrAbort("wave-d-option-boom")
    return 0
}
"#;
    let jit_opt = run_source("option_abort_jit", option_abort_src);
    assert!(
        !jit_opt.status.success(),
        "Option.expectOrAbort(None) must fail in JIT"
    );
    let jit_opt_err = String::from_utf8_lossy(&jit_opt.stderr);
    assert!(
        jit_opt_err.contains("wave-d-option-boom"),
        "expected option abort reason on stderr, got: {jit_opt_err}"
    );

    let result_abort_src = r#"module wave_d_result_abort
import std.core.result as result

func main(): int {
    let errVal: result.Result<int, int> = result.Result.Err(42)
    let _ = errVal.expectOrAbort("wave-d-result-boom")
    return 0
}
"#;
    let jit_res = run_source("result_abort_jit", result_abort_src);
    assert!(
        !jit_res.status.success(),
        "Result.expectOrAbort(Err) must fail in JIT"
    );
    let jit_res_err = String::from_utf8_lossy(&jit_res.stderr);
    assert!(
        jit_res_err.contains("wave-d-result-boom"),
        "expected result abort reason on stderr, got: {jit_res_err}"
    );

    // Also verify C backend emits and runs `abortWithMessage` when clang is available.
    let dir = common::temp_dir("arandu-wave-d-abort-c").unwrap();
    let aru_path = dir.join("main.aru");
    fs::write(&aru_path, option_abort_src).unwrap();
    let c_emit = common::cli_command()
        .arg("emit-c")
        .arg(&aru_path)
        .output()
        .unwrap();
    assert!(
        c_emit.status.success(),
        "emit-c failed: {}",
        String::from_utf8_lossy(&c_emit.stderr)
    );
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let c_path = dir.join("main.c");
        let exe_path = dir.join("main.exe");
        fs::write(&c_path, &c_emit.stdout).unwrap();
        let cc = Command::new("clang")
            .arg(&c_path)
            .arg("-o")
            .arg(&exe_path)
            .output()
            .unwrap();
        assert!(
            cc.status.success(),
            "clang failed: {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        let run = Command::new(&exe_path).output().unwrap();
        assert!(
            !run.status.success(),
            "C binary for Option.expectOrAbort(None) must fail"
        );
        let c_err = String::from_utf8_lossy(&run.stderr);
        assert!(
            c_err.contains("wave-d-option-boom"),
            "expected C abort reason on stderr, got: {c_err}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
