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
    emit_llvm_ir(&amir, &tc.symbols, &tc.type_info).expect("LLVM emission failed")
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
        .compile(&amir, &tc.symbols, &tc.type_info)
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
    assert!(ir.contains("define i32 @add(i32 %t1, i32 %t2)"));
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
    assert!(ir.contains("define [8 x i8]"));
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
        ir.contains("define i32 @keep_str({ ptr, i64 }"),
        "Arandu→Arandu calls keep first-class fat str:\n{ir}"
    );
    assert!(
        ir.contains("extractvalue { ptr, i64 }"),
        "extern call site must split fat pointer fields:\n{ir}"
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
