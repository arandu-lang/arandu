#![cfg(target_pointer_width = "64")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use arandu_backend_cranelift::CraneliftBackend;
use arandu_backend_llvm::emit_llvm_ir;
use arandu_middle::amir::{AmirProgram, VectorOp};
use arandu_middle::types::VectorType;
use arandu_semantics::{
    TypeCheckResult, lower_to_amir_with_interfaces, lower_to_hir, resolve_for_test, type_check,
};
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "../../../tests/support/native_simd.rs"]
mod fixture;

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn temporary_module_path() -> std::path::PathBuf {
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("arandu-llvm-{}-{id}.ll", std::process::id()))
}

fn compile_src(src: &str) -> (AmirProgram, TypeCheckResult) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let (amir, _) = lower_to_amir_with_interfaces(&mut tc, &hir, 8).expect("AMIR lowering failed");
    (amir, tc)
}

fn emit(src: &str) -> String {
    let (amir, tc) = compile_src(src);
    emit_llvm_ir(&amir, &tc.symbols, &*tc.type_info).expect("LLVM emission failed")
}

fn verify(ir: &str) {
    let Some(llvm_as) = tool("llvm-as") else {
        return;
    };
    let path = temporary_module_path();
    fs::write(&path, ir).expect("write IR");
    let output = Command::new(llvm_as)
        .arg(&path)
        .arg("-o")
        .arg("/dev/null")
        .output()
        .expect("run llvm-as");
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "llvm-as rejected the module:\n{}\n{}",
        ir,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(ir: &str) -> Option<i32> {
    let lli = tool("lli")?;
    let path = temporary_module_path();
    fs::write(&path, ir).expect("write IR");
    let status = Command::new(lli).arg(&path).status().expect("run lli");
    let _ = fs::remove_file(&path);
    Some(status.code().expect("lli terminated by signal"))
}

fn run_cranelift(src: &str) -> i32 {
    let (amir, tc) = compile_src(src);
    let module = CraneliftBackend::try_new()
        .expect("Cranelift backend")
        .compile(&amir, &tc.symbols, &*tc.type_info)
        .expect("Cranelift compile");
    let main = unsafe { module.get_fn::<unsafe fn() -> i32>("main") }.expect("main export");
    unsafe { main() }
}

fn tool(name: &str) -> Option<String> {
    if Command::new(name).arg("--version").output().is_ok() {
        Some(name.to_owned())
    } else {
        None
    }
}

#[test]
fn emits_direct_scalar_ir_and_runs_it() {
    let ir = emit(
        r#"
func add(a: int, b: int): int {
    return a + b
}

func main(): int {
    return add(20, 22)
}
"#,
    );
    assert!(ir.contains("source_filename = \"arandu\""));
    assert!(ir.contains("define internal i32 @add(i32 %t1, i32 %t2)"));
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 42);
    }
}

#[test]
fn lowers_branches_loops_and_mutable_locals() {
    let ir = emit(
        r#"
func abs(value: int): int {
    if value < 0 {
        return -value
    }
    return value
}

func factorial(n: int): int {
    let mut result = 1
    let mut index = 1
    while index <= n {
        result = result * index
        index = index + 1
    }
    return result
}

func main(): int {
    return abs(-20) + factorial(3)
}
"#,
    );
    assert!(ir.contains("br i1"));
    assert!(ir.contains("phi i32"));
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 26);
    }
}

#[test]
fn emits_calls_for_nominal_destroy_statements() {
    let ir = emit(
        r#"
struct Resource { handle: ptr[u8] }
@Destructor
func Resource.close(own self): void {}

func main(): int {
    let resource = Resource { handle: nil }
    return 0
}
"#,
    );
    assert!(ir.contains("call void @Resource.close("), "{ir}");
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 0);
    }
}

#[test]
fn frees_owned_strings_on_destroy() {
    let ir = emit(
        r#"
func render(n: int): int {
    let text = "owned=${n}"
    return 0
}

func main(): int { return render(1) }
"#,
    );
    assert!(ir.contains("call void @ar_rt_raw_free(ptr "), "{ir}");
    assert!(ir.contains("zeroinitializer, ptr %s"), "{ir}");
    verify(&ir);
}

#[test]
fn lowers_escaped_char_literals() {
    let ir = emit(
        r#"
func main(): int {
    let newline: char = '\n'
    let tab: char = '\t'
    if newline != '\n' { return 1 }
    if tab != '\t' { return 2 }
    return 0
}
"#,
    );
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 0);
    }
}

#[test]
fn lowers_string_literal_with_quotes_and_backslashes() {
    let ir = emit(
        r#"
func keep_str(s: str): int {
    return 0
}

func main(): int {
    let s: str = "<p class=\"badge\">\"hello\\world\"</p>"
    return keep_str(s)
}
"#,
    );
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 0);
    }
}

#[test]
fn lowers_tuple_destructure_and_return() {
    let ir = emit(
        r#"
func pair(): (int, int) {
    return 3, 4
}

func main(): int {
    let left, right = pair()
    return left + right
}
"#,
    );
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 7);
    }
}

#[test]
fn lowers_floating_point_arithmetic() {
    let ir = emit(
        r#"
func half(value: float): float {
    return value / 2.0
}

func main(): int {
    if half(9.0) == 4.5 {
        return 7
    }
    return 1
}
"#,
    );
    assert!(ir.contains("fdiv double"));
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 7);
    }
}

#[test]
fn emits_atomic_intrinsics() {
    let ir = emit(
        r#"
module std.core.atomicTest

extern "arandu-intrinsic" {
    func atomicFetchAdd32(value: mut ref u32, operand: u32): u32
    func atomicLoad32(value: ref u32): u32
    func atomicCompareExchange32(value: mut ref u32, expected: u32, desired: u32): u32
}

public func increment(counter: ptr[u32]): u32 {
    unsafe {
        return atomicFetchAdd32(mut ref *counter, 1 as u32)
    }
}

func main(): int {
    return 0
}
"#,
    );
    assert!(ir.contains("atomicrmw add ptr"));
    assert!(ir.contains("seq_cst"));
    verify(&ir);
}

#[test]
fn emits_vector_arithmetic_ir() {
    let case = fixture::arithmetic(VectorType::F32x4, VectorOp::Add);
    let ir = emit_llvm_ir(&case.program, &case.symbols, &case.info).expect("LLVM emission failed");
    assert!(ir.contains("<4 x float>"));
    assert!(ir.contains("fadd <4 x float>"));
    assert!(ir.contains("extractelement <4 x float>"));
    verify(&ir);
}

#[test]
fn matches_cranelift_exit_code_for_scalar_corpus() {
    let src = r#"
func sum(n: int): int {
    let mut total = 0
    let mut i = 1
    while i <= n {
        total = total + i
        i = i + 1
    }
    return total
}

func main(): int {
    return sum(9)
}
"#;
    let ir = emit(src);
    let Some(llvm_code) = run(&ir) else {
        return;
    };
    assert_eq!(llvm_code, run_cranelift(src));
}

#[test]
fn lowers_structs_arrays_and_aggregate_abi() {
    let ir = emit(
        r#"
struct Point {
    x: int
    y: int
}

func sum_point(p: Point): int {
    return p.x + p.y
}

func make_point(x: int, y: int): Point {
    return Point { x: x, y: y }
}

func main(): int {
    let p = Point { x: 10, y: 20 }
    let q = make_point(3, 4)
    let a: [3]int = [1, 2, 3]
    if sum_point(p) != 30 { return 1 }
    if q.x != 3 { return 2 }
    if a[0] + a[2] != 4 { return 3 }
    return 0
}
"#,
    );
    assert!(ir.contains("sum_point([8 x i8]"));
    assert!(ir.contains("define internal [8 x i8]"));
    assert!(ir.contains("getelementptr inbounds i8"));
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 0);
    }
}

#[test]
fn emits_safe_parameter_attributes() {
    let ir = emit(
        r#"
func read(value: ref int): int {
    return *value
}

func main(): int {
    return 0
}
"#,
    );
    assert!(ir.contains("ptr nonnull %t1"));
    assert!(ir.contains("nounwind"));
    verify(&ir);
}

#[test]
fn zero_extends_unsigned_byte_widening_casts() {
    // `u8 as u32` must zext. Sign-extending 0x80 to i32 breaks SHA-256 padding
    // (beU32 of a block containing 0x80) and Softwares/ita digests.
    let src = r#"
func pack(b0: u8, b1: u8, b2: u8, b3: u8): u32 {
    let x0 = b0 as u32
    let x1 = b1 as u32
    let x2 = b2 as u32
    let x3 = b3 as u32
    return (x0 << 24) | (x1 << 16) | (x2 << 8) | x3
}

func main(): int {
    // 0x80 must become 0x00000080, not 0xffffff80.
    if pack(0x00 as u8, 0x00 as u8, 0x00 as u8, 0x80 as u8) != (0x00000080 as u32) {
        return 1
    }
    if pack(0x80 as u8, 0x00 as u8, 0x00 as u8, 0x00 as u8) != (0x80000000 as u32) {
        return 2
    }
    return 0
}
"#;
    let ir = emit(src);
    assert!(
        ir.contains("zext i8"),
        "unsigned u8 widening must zext:\n{ir}"
    );
    assert!(!ir.contains("sext i8"), "u8 widening must not sext:\n{ir}");
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 0);
        assert_eq!(code, run_cranelift(src));
    }
}

#[test]
fn extracts_result_ok_payload_after_tag() {
    // Place projection `::<variant>::<payload>` must use field_offsets[1]
    // (payload), not [0] (tag). Loading from offset 0 returns the discriminant
    // and corrupts aggregate payloads (Softwares/ita sha256Bytes SEGV).
    let src = r#"
func main(): int {
    let r: Result<int, int> = Result.Ok(42)
    match r {
        Ok(value) => {
            return value
        }
        Err(_) => {
            return 0
        }
    }
}
"#;
    let ir = emit(src);
    assert!(
        ir.contains("getelementptr inbounds i8, ptr") && ir.contains(", i64 8"),
        "Ok payload must be loaded from offset 8 after the tag:\n{ir}"
    );
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 42, "Ok(42) must not read the discriminant as payload");
        assert_eq!(code, run_cranelift(src));
    }
}

#[test]
fn flattens_host_c_abi_fat_str_params() {
    // Runtime helpers such as `ar_fs_read_all` take path as (ptr, len) scalars.
    // Passing `{ ptr, i64 }` by value mis-aligns subsequent out-params and
    // corrupts String length/capacity (SEGV on Softwares/ita).
    let ir = emit(
        r#"
extern "C" {
    func host_take_str(path: str): void
}

func keep_str(path: str): int {
    if path == "ok" {
        return 1
    }
    return 0
}

func main(): int {
    unsafe {
        host_take_str("hi")
    }
    return keep_str("ok")
}
"#,
    );
    assert!(
        ir.contains("declare void @host_take_str(ptr, i64)"),
        "host str param must flatten to ptr+len:\n{ir}"
    );
    assert!(
        !ir.contains("declare void @host_take_str({ ptr, i64 })"),
        "must not pass fat str as a single aggregate to C:\n{ir}"
    );
    assert!(
        ir.contains("define internal i32 @keep_str({ ptr, i64 }"),
        "Arandu→Arandu calls keep first-class fat str:\n{ir}"
    );
    assert!(
        ir.contains("extractvalue { ptr, i64 }"),
        "extern call site must split fat pointer fields:\n{ir}"
    );
    verify(&ir);
}

#[test]
fn uses_sysv_runtime_abi_for_windows_fat_string_returns() {
    let source = r#"
extern "C" {
    func ar_env_arg(index: isize): str
}

func main(): int {
    let executable = unsafe {
        ar_env_arg(0 as isize)
    }
    if executable == "arandu-smith" {
        return 0
    }
    return 1
}
"#;
    let (program, checked) = compile_src(source);
    let mut info = (*checked.type_info).clone();
    info.target_identity = arandu_middle::db::TargetIdentity::from_triple("x86_64-pc-windows-msvc")
        .expect("canonical Windows triple");
    let ir =
        emit_llvm_ir(&program, &checked.symbols, &info).expect("LLVM emission for Windows target");

    assert!(
        ir.contains("declare x86_64_sysvcc { ptr, i64 } @ar_env_arg(i64)"),
        "Windows runtime declaration must retain the runtime ABI:\n{ir}"
    );
    assert!(
        ir.contains("call x86_64_sysvcc { ptr, i64 } @ar_env_arg(i64 "),
        "Windows runtime call must retain the runtime ABI:\n{ir}"
    );
    verify(&ir);
}

#[test]
fn emits_vector_memory_and_shuffle_ir() {
    let case = fixture::memory_shuffle(VectorType::I32x4);
    let ir = emit_llvm_ir(&case.program, &case.symbols, &case.info).expect("LLVM emission failed");
    assert!(ir.contains("insertelement <4 x i32>"));
    assert!(ir.contains("shufflevector <16 x i8>"));
    verify(&ir);
}

/// Names of the module-level `declare` entities, in emission order.
fn declared_symbols(ir: &str) -> Vec<String> {
    ir.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("declare ")?;
            let at = rest.find('@')?;
            let name = rest[at + 1..].split('(').next()?;
            Some(name.trim_matches('"').to_owned())
        })
        .collect()
}

/// Textual LLVM IR makes every declaration a visible module entity, unlike a
/// JIT symbol table. A declaration that the module never references is dead
/// output and, in freestanding targets, a misleading promise about the
/// runtime that will exist at link time.
fn assert_no_orphan_declarations(ir: &str) {
    let body = ir
        .lines()
        .filter(|line| !line.trim_start().starts_with("declare "))
        .collect::<Vec<_>>()
        .join("\n");
    for name in declared_symbols(ir) {
        assert!(
            body.contains(&format!("@{name}(")),
            "declaration @{name} is never referenced by the module:\n{ir}"
        );
    }
}

#[test]
fn scalar_module_declares_no_runtime_helpers() {
    let ir = emit(
        r#"
func main(): int {
    let value: int = 7
    return value
}
"#,
    );
    assert!(
        declared_symbols(&ir).is_empty(),
        "a pure scalar module must not declare any runtime helper:\n{ir}"
    );
    assert!(
        !ir.contains("alloca"),
        "locals without their address taken must not get a stack home:\n{ir}"
    );
    assert!(
        !ir.contains("select i1 true"),
        "identity assignments must alias the SSA value instead of emitting a copy:\n{ir}"
    );
    verify(&ir);
}

#[test]
fn export_emits_flat_c_symbols_for_module_functions() {
    let ir = emit(
        r#"
module hitbox.core.socd

@Export
func apply_socd(raw: u32): u32 { return raw }

@Export("arandu_core_process_inputs")
func process_inputs(raw: u32): u32 { return raw }
"#,
    );
    assert!(
        ir.contains("define i32 @apply_socd(i32"),
        "@Export must emit the unqualified function name:\n{ir}"
    );
    assert!(
        ir.contains("define i32 @arandu_core_process_inputs(i32"),
        "@Export(\"name\") must override the symbol:\n{ir}"
    );
    assert!(
        !ir.contains("hitbox.core.socd.apply_socd"),
        "an exported function must not keep its module-qualified host name:\n{ir}"
    );
    verify(&ir);
}

#[test]
fn unexported_functions_keep_internal_linkage() {
    let ir = emit(
        r#"
module hitbox.core.socd

@Export
func apply_socd(raw: u32): u32 { return helper(raw) }

func helper(raw: u32): u32 { return raw }

func main(): int { return 0 }
"#,
    );
    let definitions: Vec<&str> = ir
        .lines()
        .filter(|line| line.starts_with("define "))
        .collect();
    assert!(
        definitions
            .iter()
            .any(|line| line.contains("@apply_socd(") && !line.contains("internal")),
        "@Export publishes the symbol:\n{ir}"
    );
    assert!(
        definitions
            .iter()
            .any(|line| line.contains(".helper(") && line.contains("internal")),
        "an unannotated function must not leak a global symbol:\n{ir}"
    );
    assert!(
        definitions
            .iter()
            .any(|line| line.contains("@main(") && !line.contains("internal")),
        "main stays visible to the linker:\n{ir}"
    );
    verify(&ir);
}

#[test]
fn string_module_declares_only_the_helpers_it_uses() {
    let ir = emit(
        r#"
func main(): int {
    let n: int = 42
    let text = "n=${n}"
    if text != "n=42" {
        return 1
    }
    return 0
}
"#,
    );
    let declared = declared_symbols(&ir);
    assert!(
        declared.iter().any(|name| name == "ar_jit_i64_to_str"),
        "interpolation must declare the i64 ToStr helper:\n{ir}"
    );
    for forbidden in [
        "ar_co_block_on_i64",
        "ar_co_make_ready_i64",
        "ar_co_poll_i64",
        "ar_gen_insert_raw",
        "ar_rt_spawn_i64",
        "ar_rt_join_i64",
        "ar_rt_block_on_i64",
        "llvm.ctpop.i8",
        "llvm.ctpop.i64",
        "llvm.ctlz.i32",
        "llvm.trap",
    ] {
        assert!(
            !declared.iter().any(|name| name == forbidden),
            "unused runtime helper @{forbidden} leaked into the module:\n{ir}"
        );
    }
    assert_no_orphan_declarations(&ir);
    verify(&ir);
}

#[test]
fn export_marshals_repr_c_aggregates_through_abi_lanes() {
    // An 8-byte `{u32, f32}` is one 64-bit integer lane on every ABI family the
    // host can be built for (SysV, Windows x64, AAPCS64), so the shape below is
    // portable across CI hosts while still proving lane marshalling.
    let ir = emit(
        r#"
@Repr("C")
struct Frame {
    id: u32
    weight: f32
}

@Export
func shift(frame: Frame): Frame {
    let moved = Frame { id: frame.id, weight: frame.weight }
    return moved
}
"#,
    );
    assert!(
        ir.contains("define i64 @shift(i64 %p")
            && ir.lines().any(|line| line.starts_with("ret i64 %")),
        "an 8-byte C struct crosses as one i64 lane:\n{ir}"
    );
    assert!(
        ir.contains("alloca [8 x i8]"),
        "the body must materialize the aggregate representation:\n{ir}"
    );
    assert!(
        ir.contains("getelementptr inbounds i8"),
        "lanes are rebuilt through byte offsets:\n{ir}"
    );
    assert!(
        !ir.contains("define [8 x i8] @shift"),
        "the ABI lane type, not the memory representation, must appear in the signature:\n{ir}"
    );
    verify(&ir);
}

#[test]
fn unsigned_literal_casts_to_float_use_uitofp() {
    // A u64 literal above i64::MAX must convert unsigned; sitofp read it as -2048.
    let src = r#"
func main(): int {
    let wide: u64 = 18446744073709549568
    let real = wide as f64
    if real != 18446744073709549568.0 || real as u64 != wide { return 1 }
    return 0
}
"#;
    let ir = emit(src);
    assert!(
        !ir.contains("sitofp i64 18446744073709549568"),
        "unsigned literal must not use sitofp:\n{ir}"
    );
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 0);
    }
}

#[test]
fn negated_and_inverted_literals_take_the_destination_width() {
    let src = r#"
func main(): int {
    let neg: i64 = -5
    if (neg as f64) != -5.0 { return 1 }
    let mask: u64 = ~0
    if mask != 18446744073709551615 { return 2 }
    return 0
}
"#;
    let ir = emit(src);
    assert!(
        !ir.contains("sub i32 0, 5"),
        "i64 negation emitted at i32:\n{ir}"
    );
    verify(&ir);
    if let Some(code) = run(&ir) {
        assert_eq!(code, run_cranelift(src));
    }
}

/// Non-UTF-8 byte strings are frozen blobs: the LLVM backend must emit a raw
/// rodata global (not a string-shaped one) and keep every byte intact.
#[test]
fn lowers_non_utf8_byte_string_blob_to_raw_global() {
    let src = r#"
func main(): int {
    let raw = b"\xFF\x00AB"
    if raw.len != 4 { return 1 }
    if raw[0] != b'\xFF' { return 2 }
    if raw[3] != b'B' { return 3 }
    return 42
}
"#;
    let ir = emit(src);
    assert!(
        ir.contains(r#"c"\FF\00AB\00""#),
        "raw blob must reach the rodata global intact:\n{ir}"
    );
    assert_eq!(run_cranelift(src), 42);
    if let Some(code) = run(&ir) {
        assert_eq!(code, 42, "lli must agree with Cranelift");
    }
    verify(&ir);
}
