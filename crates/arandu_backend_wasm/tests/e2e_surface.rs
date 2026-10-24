//! End-to-end tests for the surface language: Arandu source → AMIR → wasm
//! bytes → real execution inside the wasmtime runtime. These are the first
//! tests that *run* emitted modules instead of only checking well-formedness.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

mod common;

use arandu_middle::amir::block::{AmirBasicBlock, BlockId};
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::program::AmirFunc;
use arandu_middle::amir::stmt::{AmirStmt, AmirStmtTable, AmirTerminator};
use arandu_middle::amir::value::{AmirConstant, AmirOperand, AmirRvalue};
use arandu_middle::cfg::compute_cfg_edges;
use arandu_middle::layout::DenseRange;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use common::{
    compile_source, compile_trusted_core_fixture, emit_one, find_func_export, foreign_sym,
    run_main_i32, run_main_traps, temp,
};

#[test]
fn bit_intrinsics_cover_every_bit_position_and_zero() {
    let source = include_str!("../../arandu_backend_c/tests/bit_intrinsics.aru");
    assert_eq!(run_main_i32(&compile_trusted_core_fixture(source)), 0);
}

#[test]
fn ordinary_bit_names_are_not_intrinsics() {
    let bytes = compile_source(
        "func bitCountOnes32(value: u32): u32 { return value + 7 } func main(): int { if bitCountOnes32(5) != 12 { return 1 }; return 0 }",
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn integer_division_and_remainder_have_defined_boundaries() {
    for (ty, minimum) in [
        ("i8", "-128"),
        ("i16", "-32768"),
        ("i32", "-2147483648"),
        ("i64", "-9223372036854775808"),
    ] {
        let division = compile_source(&format!(
            "func probe(a: {ty}, b: {ty}): {ty} {{ return a / b }} func main(): int {{ return probe({minimum}, -1) as int }}"
        ));
        assert!(run_main_traps(&division), "{ty}: MIN / -1 must trap");
        let remainder = compile_source(&format!(
            "func probe(a: {ty}, b: {ty}): {ty} {{ return a % b }} func main(): int {{ return probe({minimum}, -1) as int }}"
        ));
        assert_eq!(run_main_i32(&remainder), 0, "{ty}: MIN % -1");
    }
    for ty in ["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"] {
        for operator in ["/", "%"] {
            let bytes = compile_source(&format!(
                "func probe(a: {ty}, b: {ty}): {ty} {{ return a {operator} b }} func main(): int {{ return probe(7, 0) as int }}"
            ));
            assert!(
                run_main_traps(&bytes),
                "{ty}: dynamic zero divisor {operator}"
            );
        }
    }
}

#[test]
fn dynamic_shifts_use_i32_or_i64_execution_width() {
    let source = r#"
func shift32(value: i32, count: i32): i32 { return value >> count }
func shift64(value: u64, count: u64): u64 { return value << count }
func narrowSigned(value: i8, count: i8): i8 { return value >> count }
func narrowUnsigned(value: u16, count: u16): u16 { return value >> count }
func main(): int {
    if shift32(-8, 32) != -8 { return 1 }
    if shift32(-8, -1) != -1 { return 2 }
    if shift64(1, 127) != 9223372036854775808 { return 3 }
    if narrowSigned(-128, 8) != -1 { return 4 }
    if narrowUnsigned(65535, 16) != 0 { return 5 }
    return 0
}
"#;
    assert_eq!(run_main_i32(&compile_source(source)), 0);
}

#[test]
fn scalar_float_integer_casts_preserve_width_and_signedness() {
    let bytes = compile_source(
        r#"
func convert(x: f64, y: f32, a: u64, b: i32): int {
    if x as u64 != 9223372036854775808 as u64 { return 1 }
    if y as i64 != -42 { return 2 }
    if a as f64 != 9223372036854775808.0 { return 3 }
    if b as f32 != -42.0 as f32 { return 4 }
    return 0
}

func main(): int {
    let a: f64 = 9223372036854775808.0
    let b: f32 = -42.75
    // Separate positive input for unsigned truncation.
    let positive: f32 = 42.75
    if positive as u32 != 42 { return 5 }
    if 42.75 as u64 != 42 { return 6 }
    if 42 as f64 != 42.0 { return 7 }
    return convert(a, b, 9223372036854775808 as u64, -42)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn literal_numeric_casts_do_not_narrow_before_float_conversion() {
    let bytes = compile_source(
        "func main(): int { if (4294967296 as f64) != 4294967296.0 { return 1 }; if (4294967296 as f32) != (4294967296.0 as f32) { return 2 }; if (42.75 as u32) != 42 { return 3 }; return 0 }",
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn unit_arguments_do_not_leave_wasm_stack_values() {
    let bytes = compile_source(
        "func unit(): void {} func consume(a: void, x: int, b: void, y: int): int { return x + y } func main(): int { let value = unit(); return consume(value, 19, unit(), 23) }",
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn nullable_nil_argument_is_not_confused_with_unit() {
    let bytes = compile_source(
        "func missing(value: int?, marker: int): int { if value == nil { return marker }; return 1 } func main(): int { return missing(nil, 42) }",
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn result_error_storage_is_preserved_when_unit_call_arguments_are_erased() {
    let bytes = compile_source(
        "func ok_seven(): Result<int, Err> { return .Ok(7) }\nfunc extract_seven(): Result<int, Err> { let value = ok_seven()?; return .Ok(value) }\nfunc main(): int { return match extract_seven() {\nOk(value) => value\nErr(_) => 0\n} }\n",
    );
    assert_eq!(run_main_i32(&bytes), 7);
}

#[test]
fn foreign_calls_use_declared_unit_and_nullable_parameter_types() {
    let bytes = compile_source(
        "extern \"C\" { func inspect(unit: void, value: ptr[int]?, marker: int): int }\nfunc unit(): void {}\nfunc main(): int { return unsafe { inspect(unit(), nil, 42) } }\n",
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("valid foreign-call ABI");
    let mut linker = wasmtime::Linker::new(&engine);
    linker
        .func_wrap("env", "inspect", |value: i32, marker: i32| {
            assert_eq!(value, 0);
            marker
        })
        .unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &find_func_export(&bytes, "main").unwrap())
        .unwrap();
    assert_eq!(main.call(&mut store, ()).unwrap(), 42);
}

#[test]
fn scalar_float_integer_casts_cover_all_wasm_widths_and_signs() {
    macro_rules! check {
        ($from:literal, $to:literal, $input:expr, $expected:expr, $rust_from:ty, $rust_to:ty) => {{
            let bytes = compile_source(&format!(
                "public func main(value: {}): {} {{ return value as {} }}",
                $from, $to, $to
            ));
            let engine = wasmtime::Engine::default();
            let module = wasmtime::Module::new(&engine, &bytes).unwrap();
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
            let function = instance
                .get_typed_func::<$rust_from, $rust_to>(
                    &mut store,
                    &find_func_export(&bytes, "main").unwrap(),
                )
                .unwrap();
            assert_eq!(
                function.call(&mut store, $input).unwrap(),
                $expected,
                "{} -> {}",
                $from,
                $to
            );
        }};
    }
    check!("f32", "i32", -42.75, -42, f32, i32);
    check!("f32", "u32", 2147483648.0, i32::MIN, f32, i32);
    check!("f32", "i64", -42.75, -42, f32, i64);
    check!("f32", "u64", 9223372036854775808.0, i64::MIN, f32, i64);
    check!("f64", "i32", -42.75, -42, f64, i32);
    check!("f64", "u32", 2147483648.0, i32::MIN, f64, i32);
    check!("f64", "i64", -42.75, -42, f64, i64);
    check!("f64", "u64", 9223372036854775808.0, i64::MIN, f64, i64);
    check!("i32", "f32", -42, -42.0, i32, f32);
    check!("u32", "f32", i32::MIN, 2147483648.0, i32, f32);
    check!("i64", "f32", -42, -42.0, i64, f32);
    check!("u64", "f32", i64::MIN, 9223372036854775808.0, i64, f32);
    check!("i32", "f64", -42, -42.0, i32, f64);
    check!("u32", "f64", i32::MIN, 2147483648.0, i32, f64);
    check!("i64", "f64", -42, -42.0, i64, f64);
    check!("u64", "f64", i64::MIN, 9223372036854775808.0, i64, f64);
    check!("f64", "u8", 300.75, 44, f64, i32);
}

#[test]
fn copy_frames_reuse_loop_storage_and_restore_normal_and_early_returns() {
    let bytes = compile_source(
        r#"
public func main(early: bool): int {
    let mut i = 0
    while i < 1000 {
        let original: [64]i32 = [7; 64]
        let mut copy = original
        copy[63] = 9
        if original[63] != 7 || copy[63] != 9 { return 1 }
        if early { return original[0] as int + 35 }
        i = i + 1
    }
    return 0
}
"#,
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, &find_func_export(&bytes, "main").unwrap())
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let before = memory.data_size(&store);
    for _ in 0..200 {
        assert_eq!(main.call(&mut store, 0).unwrap(), 0);
        assert_eq!(main.call(&mut store, 1).unwrap(), 42);
    }
    assert_eq!(memory.data_size(&store), before);
}

#[test]
fn contextual_nominal_variant_payload_keeps_its_narrow_array_type() {
    let bytes = compile_source(
        r#"
enum Packet { Data([64]u8), Empty }
func packet(): Packet { return .Data([42; 64]) }
func main(): int {
    match packet() {
        Packet.Data(bytes) => { return bytes[63] as int - 42 }
        Packet.Empty => { return 1 }
    }
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn copy_frames_do_not_overlap_large_rodata_or_callee_frames() {
    let literal = "x".repeat(180_000);
    let source = format!(
        r#"
import io
func recurse(depth: int): int {{
    let a: [64]i32 = [3; 64]
    if depth == 0 {{ return a[63] as int }}
    return recurse(depth - 1) + a[0] as int
}}
public func main(): int {{
    io.print("{literal}")
    return recurse(40)
}}
"#
    );
    let bytes = compile_source(&source);
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut linker = wasmtime::Linker::<()>::new(&engine);
    linker
        .func_wrap(
            "io",
            "print",
            |mut caller: wasmtime::Caller<'_, ()>, ptr: i32, len: i32| {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let start = usize::try_from(ptr).unwrap();
                let end = start + usize::try_from(len).unwrap();
                assert_eq!(end - start, 180_000);
                assert!(
                    memory.data(&caller)[start..end]
                        .iter()
                        .all(|&byte| byte == b'x')
                );
            },
        )
        .unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &find_func_export(&bytes, "main").unwrap())
        .unwrap();
    for _ in 0..100 {
        assert_eq!(main.call(&mut store, ()).unwrap(), 123);
    }
}

#[test]
fn large_and_affine_scratch_backing_is_reused_and_released_per_invocation() {
    let bytes = compile_source(
        r#"
struct Owned { bytes: [2048]u8 }
@Destructor
func Owned.close(own self): void {}
public func main(early: bool): int {
    let mut i = 0
    while i < 100 {
        let original: [2048]u8 = [7; 2048]
        let mut independent = original
        independent[2047] = 42
        let owned = Owned { bytes: [9; 2048] }
        if owned.bytes[2047] != 9 || original[2047] != 7 || independent[2047] != 42 { return 1 }
        if early { return 0 }
        i += 1
    }
    return 0
}
"#,
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, &find_func_export(&bytes, "main").unwrap())
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let before = memory.data_size(&store);
    for _ in 0..1000 {
        assert_eq!(main.call(&mut store, 0).unwrap(), 0);
        assert_eq!(main.call(&mut store, 1).unwrap(), 0);
    }
    assert_eq!(memory.data_size(&store), before);
}

#[test]
fn copy_results_outlive_callee_frames_without_growing_caller_memory() {
    let bytes = compile_source(
        r#"
func make(depth: int): [2048]u8 {
    if depth == 0 { return [42; 2048] }
    return make(depth - 1)
}
public func main(): int {
    let mut i = 0
    while i < 100 {
        let result = make(4)
        if result[0] != 42 || result[2047] != 42 { return 1 }
        i += 1
    }
    return 0
}
"#,
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &find_func_export(&bytes, "main").unwrap())
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let before = memory.data_size(&store);
    for _ in 0..100 {
        assert_eq!(main.call(&mut store, ()).unwrap(), 0);
    }
    assert_eq!(memory.data_size(&store), before);
}

#[test]
fn affine_results_transfer_resources_once_and_reclaim_backing() {
    let bytes = compile_source(
        r#"
import io
struct Owned { data: ptr[u8], bytes: [2048]u8 }
func release(data: ptr[u8]): void {
    unsafe { free(data) }
    io.print(".")
}
@Destructor
func Owned.close(own self): void { release(self.data) }
func make(depth: int): Owned {
    if depth == 0 {
        let data = alloc(80) as ptr[u8]
        return Owned { data: data, bytes: [42; 2048] }
    }
    return make(depth - 1)
}
public func main(early: bool): int {
    let mut i = 0
    while i < 100 {
        let result = make(4)
        if result.bytes[0] != 42 || result.bytes[2047] != 42 { return 1 }
        if early { return 0 }
        i += 1
    }
    return 0
}
"#,
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut linker = wasmtime::Linker::<usize>::new(&engine);
    linker
        .func_wrap(
            "io",
            "print",
            |mut caller: wasmtime::Caller<'_, usize>, ptr: i32, len: i32| {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let start = usize::try_from(ptr).unwrap();
                assert_eq!(len, 1);
                assert_eq!(memory.data(&caller)[start], b'.');
                *caller.data_mut() += 1;
            },
        )
        .unwrap();
    let mut store = wasmtime::Store::new(&engine, 0);
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, &find_func_export(&bytes, "main").unwrap())
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let before = memory.data_size(&store);
    for iteration in 1..=100 {
        assert_eq!(main.call(&mut store, 0).unwrap(), 0);
        assert_eq!(main.call(&mut store, 1).unwrap(), 0);
        assert_eq!(*store.data(), iteration * 101);
    }
    assert_eq!(memory.data_size(&store), before);
}

#[test]
fn print_import_writes_exact_bytes_without_a_newline() {
    let bytes = compile_source(
        r#"
import io
func main(): int {
    io.print("hello")
    io.print("")
    io.print("\0world")
    return 0
}
"#,
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("valid module");
    let mut linker = wasmtime::Linker::<Vec<u8>>::new(&engine);
    linker
        .func_wrap(
            "io",
            "print",
            |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                let memory = caller
                    .get_export("memory")
                    .and_then(|export| export.into_memory())
                    .expect("guest memory");
                let start = usize::try_from(pointer).expect("nonnegative address");
                let len = usize::try_from(length).expect("nonnegative length");
                let mut bytes = vec![0; len];
                memory
                    .read(&caller, start, &mut bytes)
                    .expect("valid string storage");
                caller.data_mut().extend(bytes);
            },
        )
        .expect("register print");
    let mut store = wasmtime::Store::new(&engine, Vec::new());
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("link print import");
    let name = find_func_export(&bytes, "main").expect("main export");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("main signature");
    assert_eq!(main.call(&mut store, ()).expect("run main"), 0);
    assert_eq!(store.data(), b"hello\0world");
}

#[test]
fn byte_to_char_preserves_every_unsigned_codepoint() {
    let bytes = compile_source(
        r#"
func main(): int {
    let mut value: u32 = 0
    while value < 256 {
        let character = (value as u8) as char
        if character as u32 != value { return 1 }
        value = value + 1
    }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn option_result_equality_inspects_only_active_payload_values() {
    let bytes = compile_source(
        r#"
func main(): int {
    let a: Option<int> = Option.Some(7)
    let b: Option<int> = Option.Some(7)
    let c: Option<int> = Option.Some(8)
    let n: Option<int> = Option.None
    let m: Option<int> = Option.None
    if a != b || a == c || a == n || n != m { return 1 }
    let x: Option<Option<int>> = Option.Some(a)
    let y: Option<Option<int>> = Option.Some(b)
    let z: Option<Option<int>> = Option.Some(c)
    if x != y || x == z { return 2 }
    let ok: Result<int, int> = Result.Ok(42)
    let same: Result<int, int> = Result.Ok(42)
    let bad: Result<int, int> = Result.Err(42)
    if ok != same || ok == bad { return 3 }
    let plus: Option<f64> = Option.Some(0.0)
    let minus: Option<f64> = Option.Some(-0.0)
    if plus != minus { return 4 }
    let nan: f64 = 0.0 / 0.0
    let wrapped: Option<f64> = Option.Some(nan)
    if wrapped == wrapped { return 5 }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn repeated_arrays_execute_in_wasm_with_ctfe_and_generic_lengths() {
    let bytes = compile_source(
        r#"
func flags<comptime N: uint>(): [N]bool { return [false; N] }
func main(): int {
    let flags = flags<256>()
    let frozen = comptime { let mut table = [false; 256]; table[32] = true; table }
    let nested = comptime [[7; 2]; 3]
    if flags[255] || !frozen[32] || frozen[33] { return 1 }
    return nested[2][1] - 7
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_arithmetic_returns_5() {
    let bytes = compile_source(
        r#"
func main(): int {
    return 2 + 3
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 5);
}

#[test]
fn static_loop_residuals_execute_with_nested_and_structured_exits() {
    for (source, expected) in [
        (
            "func main(): int { let mut sum = 0\ncomptime for i in 0..3 { comptime for j in 0..2 { sum += i + j } } return sum }",
            9,
        ),
        (
            "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { if i == 1 { continue } if i == 3 { break } sum += i } return sum }",
            2,
        ),
        (
            "func main(): int { comptime for i in 0..4 { return i + 7 } return 99 }",
            7,
        ),
    ] {
        assert_eq!(run_main_i32(&compile_source(source)), expected);
    }
}

#[test]
fn public_frozen_float_and_string_values_execute_in_wasm() {
    let bytes = compile_source(
        r#"
func text(): str { return "Olá\0🦀" }
func main(): int {
    let negative: f64 = comptime (-0.0)
    if 1.0 / negative >= 0.0 { return 1 }
    let tiny: f64 = comptime (5e-324 + 5e-324)
    if tiny != 1e-323 { return 2 }
    let nan: f64 = comptime (0.0 / 0.0)
    if nan == nan { return 3 }
    let frozen = comptime text()
    if frozen != "Olá\0🦀" { return 4 }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn frozen_float_literals_keep_nan_payloads_and_signed_zero_in_wasm() {
    let interner = TypeInterner::new();
    let ty = interner.intern(ArType::Primitive(Primitive::F64));
    let format = arandu_middle::ctfe::FloatType::new(Primitive::F64, common::wasm32()).unwrap();
    for bits in [
        0_u64,
        0x8000000000000000,
        1,
        0x7ff0000000000000,
        0xfff0000000000000,
        0x7ff8000000001234,
    ] {
        let mut literals = AmirLiteralPool::default();
        let literal =
            literals.intern_float_bits(arandu_middle::ctfe::ConstFloat::new(format, bits).unwrap());
        let mut statements = AmirStmtTable::new();
        statements.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(literal))),
        });
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 1),
            terminator: AmirTerminator::Return,
        }];
        let function = AmirFunc {
            symbol: foreign_sym(1),
            return_type: ty,
            receiver: None,
            params: vec![],
            locals: vec![],
            temps: vec![temp(0, ty)],
            cfg: compute_cfg_edges(&blocks),
            blocks,
            block_params: vec![],
            stmts: statements,
        };
        let bytes = emit_one(function, &interner, &mut literals);
        assert_eq!(common::run_main_f64(&bytes).to_bits(), bits);
    }
}

#[test]
fn public_layout_expressions_are_evaluated_for_wasm32_before_emission() {
    let bytes = compile_source(
        "func size<T>(): usize { return @sizeOf(T) }\nfunc main(): int { let word = comptime (@sizeOf(usize)); return (word + @alignOf(i64) + size<[3]u16>()) as int }",
    );
    assert_eq!(run_main_i32(&bytes), 18);
}

#[test]
fn surface_bitwise_not_uses_the_operand_width() {
    let bytes = compile_source(
        r#"
func main(): int {
    let inverted: u64 = ~(0 as u64)
    if inverted >> 1 == 9223372036854775807 as u64 { return 0 }
    return 1
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_string_equality_compares_contents_not_pointer_identity() {
    let bytes = compile_source(
        r#"
func main(): int {
    let left: str = (42).to_str()
    let same_contents: str = (42).to_str()
    let different_contents: str = (43).to_str()
    if left == same_contents && left != different_contents { return 0 }
    return 1
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_borrowed_string_equality_compares_contents_in_both_operand_orders() {
    let bytes = compile_source(include_str!(
        "../../../tests/corpus/borrowed_string_equality.aru"
    ));
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_if_else_branch_executes() {
    let bytes = compile_source(
        r#"
func main(): int {
    let x = 7
    let mut res = 0
    if x > 5 {
        res = 10
    } else {
        res = 20
    }
    return res
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 10);
}

#[test]
fn surface_while_loop_sums_0_through_4() {
    // Exercises SSA values flowing across loop edges (block parameters).
    let bytes = compile_source(
        r#"
func main(): int {
    let mut sum = 0
    let mut i = 0
    while i < 5 {
        sum = sum + i
        i = i + 1
    }
    return sum
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 10);
}

#[test]
fn surface_function_call_returns_42() {
    // Exercises call-argument typing (per-operand arity, not `int`-for-all).
    let bytes = compile_source(
        r#"
func add(a: int, b: int): int {
    return a + b
}
func main(): int {
    return add(20, 22)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn surface_mut_ref_slice_preserves_data_and_length_words() {
    let bytes = compile_trusted_core_fixture(
        r#"
module std.core.mut_slice_wasm

extern "arandu-intrinsic" {
    func mutSliceFromRaw(owner: ptr[u8], data: ptr[u8], len: uint): mut []u8
    func mutSliceLen<T>(source: mut []T): uint
}

func fill(buf: mut ref mut []u8): uint {
    let len = unsafe { mutSliceLen<u8>(*buf) }
    if len != 4 { return 99 }
    buf[1] = 42 as u8
    return len
}

func observedLen(buf: ref mut []u8): uint {
    return unsafe { mutSliceLen<u8>(*buf) }
}

func main(): int {
    let raw = alloc(4) as ptr[u8]
    let mut view = unsafe { mutSliceFromRaw(raw, raw, 4 as uint) }
    let len = fill(mut ref view)
    if len != 4 || observedLen(ref view) != 4 || view[1] != (42 as u8) {
        unsafe { free(raw) }
        return 1
    }
    unsafe { free(raw) }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_struct_field_access_and_mutation() {
    // Exercises StructLiteral (heap cell), empty-projection Load/Store value
    // copies, projected field Store via a real cell address and field loads.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func main(): int {
    let mut p: Point = Point { x: 10, y: 20 }
    p.x = 30
    p.y = 40
    return p.x + p.y
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 70);
}

#[test]
fn surface_array_index_read() {
    // Exercises ArrayLiteral (heap cell with per-element stores) and an Index
    // projection with a bounds check on a fixed-length array.
    let bytes = compile_source(
        r#"
func main(): int {
    let arr = [10, 20, 30]
    return arr[1]
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 20);
}

#[test]
fn surface_enum_discriminant_and_payload() {
    // Exercises EnumConstruct (tag + payload cell), Discriminant and the
    // layout-driven EnumPayload load inside an `if is` pattern.
    let bytes = compile_source(
        r#"
enum MyResult {
    MyOk(int),
    MyError(str),
}

func main(): int {
    let r = MyResult.MyOk(42)
    if r is MyResult.MyOk(x) {
        return x
    } else {
        return -1
    }
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn surface_string_literal_argument() {
    // Exercises the fat-pair (data_ptr, len) materialization of string
    // literals in the bump heap when passed as an argument.
    let bytes = compile_source(
        r#"
func consume(name: str): int {
    return 7
}
func main(): int {
    return consume("hello")
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 7);
}

#[test]
fn surface_whole_struct_argument() {
    // A whole aggregate passed by value: the callee addresses the caller's
    // heap cell through the scalar (single-slot) argument register.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func getX(p: Point): int {
    return p.x
}
func main(): int {
    return getX(Point { x: 9, y: 8 })
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 9);
}

#[test]
fn surface_struct_is_pattern_binds_int() {
    // Struct `is` pattern: binds `y` to the field while guarding `x` with a
    // literal. Reaches the guarded path only when the literal matches.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func main(): int {
    let p = Point { x: 8, y: 25 }
    if p is Point { x: 8, y } {
        return y
    }
    return -1
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 25);
}

#[test]
fn surface_if_is_bind_scalar() {
    // Plain `x is y` binder over a scalar local.
    let bytes = compile_source(
        r#"
func main(): int {
    let x = 42
    if x is y {
        return y
    }
    return -1
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn surface_enum_else_payload_str() {
    // Enum else path: a fat payload (str) enum value that does not match the
    // bound variant falls through to the else branch with its runtime tag.
    let bytes = compile_source(
        r#"
enum MyResult {
    MyOk(int),
    MyError(str),
}

func main(): int {
    let r = MyResult.MyError("file not found")
    if r is MyResult.MyOk(x) {
        return x
    } else {
        return -1
    }
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), -1);
}

#[test]
fn surface_enum_simple_multivariant_without_payload() {
    // Enums without payloads: tag-only cells (no fat slot). Exercises several
    // runtime discriminant comparisons against a single stacked tag.
    let bytes = compile_source(
        r#"
enum State {
    Idle,
    Running,
    Done,
}

func main(): int {
    let s = State.Done
    if s is State.Idle {
        return 0
    }
    if s is State.Running {
        return 1
    }
    return 2
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 2);
}

#[test]
fn surface_enum_payload_first_variant_condition() {
    let bytes = compile_source(
        r#"
enum Choice { Left(int), Right(int) }
func main(): int {
    let selected = Choice.Left(11)
    if selected is Choice.Left(value) { return value } else { return 20 }
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 11);
}

#[test]
fn surface_enum_payload_match_returns_first_matching_payload() {
    let source = r#"
enum Choice {
    Left(int),
    Right(int),
}
func main(): int {
    let selected = Choice.Left(11)
    return match selected {
        Choice.Left(value) => value
        Choice.Right(value) => value
    }
}
"#;
    let bytes = compile_source(source);
    assert_eq!(run_main_i32(&bytes), 11);
}

#[test]
fn surface_borrow_shared_field() {
    // `&p` lowers to `Borrow` (empty projections on a memory-backed local);
    // the callee reads through a `Deref` + `Field` projection chain.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func readX(p: &Point): int {
    return p.x
}
func main(): int {
    let p = Point { x: 9, y: 8 }
    return readX(&p)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 9);
}

#[test]
fn surface_array_oob_traps() {
    // Out-of-bounds array index: the bounds check lowers to `unreachable`.
    let bytes = compile_source(
        r#"
func main(): int {
    let arr = [1, 2, 3]
    return arr[5]
}
"#,
    );
    assert!(run_main_traps(&bytes));
}

#[test]
fn surface_struct_value_semantics_copy_does_not_mutate_original() {
    // Tests value semantics of aggregates: `let mut b = a` copies via
    // `memory.copy`. Mutating `b.x` must not alter `a.x`.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func main(): int {
    let a = Point { x: 10, y: 20 }
    let mut b = a
    b.x = 99
    return a.x + b.x
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 109);
}

#[test]
fn surface_memory_grow_allocates_beyond_initial_pages() {
    // Allocates > 128 KiB (2 initial pages) across loop iterations to verify
    // that auto-grow triggers `memory.grow` and continues without trap.
    let bytes = compile_source(
        r#"
struct BigNode {
    v1: int
    v2: int
    v3: int
    v4: int
}

func main(): int {
    let mut sum = 0
    let mut i = 0
    // Each BigNode is 32 bytes. 5000 iterations = 160,000 bytes > 128 KiB.
    while i < 5000 {
        let node = BigNode { v1: i, v2: 1, v3: 2, v4: 3 }
        sum = sum + node.v2
        i = i + 1
    }
    return sum
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 5000);
}

#[test]
fn surface_string_literals_in_rodata() {
    // Verifies that string literals emitted in DataSection are accessible
    // through fat pointer len and data operations.
    let bytes = compile_source(
        r#"
func check(s: str): int {
    return 42
}

func main(): int {
    let s1 = "static rodata string"
    let s2 = "another constant"
    return check(s1) + check(s2)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 84);
}

/// `free` must reclaim heap blocks so a tight allocate/free loop never runs out
/// of the initial pages: 1000 allocations × 80 bytes would cross 64 KiB of
/// heap without a real free list and trigger `memory.grow`.
#[test]
fn surface_alloc_free_loop_keeps_memory_at_initial_pages() {
    let bytes = compile_source(
        r#"
func main(): int {
    let mut i = 0
    while i < 1000 {
        let p = alloc(80)
        free(p)
        i = i + 1
    }
    return i
}
"#,
    );

    let name = find_func_export(&bytes, "main").expect("module must export `main`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`main` must have type () → i32");

    let before = memory.data_size(&mut store);
    let result = main
        .call(&mut store, ())
        .expect("`main` must run without trapping");
    let after = memory.data_size(&mut store);

    assert_eq!(result, 1000);
    assert_eq!(
        after, before,
        "recycling must keep the heap inside the initial pages (a leaking bump allocator would grow)"
    );
}

/// Drop elaboration turns an owned value into `Destroy(place)` before `return`;
/// the wasm backend must actually call the nominal `@Destructor` so resources it
/// owns are released. The destructor frees the handle the host handed in, which
/// the next same-size allocation must observe by reusing the very same block.
#[test]
fn surface_destructor_frees_owned_resource_at_return() {
    let bytes = compile_source(
        r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(own self): void {
    free(self.handle)
}

public func adopt(p: ptr[u8]): i32 {
    let r = Resource { handle: p }
    return 0
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");
    let name = find_func_export(&bytes, "adopt").expect("module must export `adopt`");
    let adopt = instance
        .get_typed_func::<i32, i32>(&mut store, &name)
        .expect("`adopt` must have type (i32) -> i32");

    // Hand a 64-byte block to the guest; its `@Destructor` must release it.
    let p = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("host allocation must not trap");
    let rc = adopt.call(&mut store, p).expect("`adopt` must not trap");
    assert_eq!(rc, 0);

    let q = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("reuse allocation must not trap");
    assert_eq!(
        q, p,
        "the @Destructor must free the owned handle before `adopt` returns"
    );
}

/// The AMIR contract materializes a method receiver as `func.params[0]`; the
/// wasm signature must therefore include it exactly once and `obj.m()` must
/// forward it as the first argument. Drop glue calls destructors through the
/// same method path, so this pins the receiver arity independently of `Destroy`.
#[test]
fn surface_method_call_passes_receiver_once() {
    let bytes = compile_source(
        r#"
struct Point {
    x: i32
    y: i32
}

func Point.sum(self): i32 {
    return self.x + self.y
}

public func main(): i32 {
    let p = Point { x: 20, y: 22 }
    return p.sum()
}
"#,
    );

    assert_eq!(run_main_i32(&bytes), 42);
}

/// A composite without its own `@Destructor` drops each field by elaborating
/// `Destroy(composite.field)`, then releases the composite's own cell. The
/// backend must load the field's *cell pointer* (named fields are boxed) at the
/// right offset and pass it to the field type's destructor.
///
/// Destructors run in reverse field order, and the composite's own cell is
/// released last, so the resulting free list is `C → CA → pa → CB → pb`
/// (allocations are appended, frees are prepended LIFO). A same-size 64-byte
/// reallocation skips the small cell blocks and returns the last-dropped
/// field's handle `pa`, proving the backend read it from the non-zero offset 4
/// (`pad` shifts `a` there).
#[test]
fn surface_nested_destructor_frees_field_handle_at_offset() {
    let bytes = compile_source(
        r#"
struct HandleA { p: ptr[u8] }
@Destructor
func HandleA.close(own self): void {
    free(self.p)
}

struct HandleB { p: ptr[u8] }
@Destructor
func HandleB.close(own self): void {
    free(self.p)
}

struct Bundle {
    pad: i32
    a: HandleA
    b: HandleB
}

public func adopt2(pa: ptr[u8], pb: ptr[u8]): i32 {
    let bundle = Bundle {
        pad: 0,
        a: HandleA { p: pa },
        b: HandleB { p: pb },
    }
    return 0
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");
    let name = find_func_export(&bytes, "adopt2").expect("module must export `adopt2`");
    let adopt2 = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, &name)
        .expect("`adopt2` must have type (i32, i32) -> i32");

    let pa = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("first host allocation must not trap");
    let pb = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("second host allocation must not trap");
    let rc = adopt2
        .call(&mut store, (pa, pb))
        .expect("`adopt2` must not trap");
    assert_eq!(rc, 0);

    let r1 = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("reuse allocation must not trap");
    assert_eq!(
        r1, pa,
        "the last-dropped field's @Destructor must load its handle from offset 4 and free it"
    );
}

/// AMIR locals and temps are distinct index spaces, so the backend must give
/// them distinct wasm slots. Sharing a slot by numeric index silently aliases a
/// live local with the temp of the same index; here both `a` and `b` are still
/// live after the return temp is written, so a shared slot would return a wrong
/// value (or trap).
#[test]
fn surface_local_and_temp_slots_do_not_alias() {
    let bytes = compile_source(
        r#"
struct Res { v: i32 }

@Destructor
func Res.close(own self): void {}

public func main(): i32 {
    let a = Res { v: 7 }
    let b = Res { v: 9 }
    return a.v + b.v
}
"#,
    );
    assert_eq!(
        run_main_i32(&bytes),
        16,
        "locals and temps must not share wasm slots"
    );
}

/// A `@Destructor` is the storage cleanup for the whole value: besides running
/// the user destructor, the backend must reclaim the receiver's heap cell, and
/// it must not materialize a temporary value-copy of the receiver. Either
/// omission leaks one 64-byte cell per call. `make` builds a 64-byte composite
/// and drops it before returning, so a tight host loop must stay inside the
/// initial pages (a leaking backend would need `memory.grow`).
#[test]
fn surface_destructor_reclaims_receiver_cell_without_growth() {
    let bytes = compile_source(
        r#"
struct Big {
    f0: i32
    f1: i32
    f2: i32
    f3: i32
    f4: i32
    f5: i32
    f6: i32
    f7: i32
    f8: i32
    f9: i32
    f10: i32
    f11: i32
    f12: i32
    f13: i32
    f14: i32
    f15: i32
}

@Destructor
func Big.close(own self): void {}

public func make(): i32 {
    let x = Big {
        f0: 1, f1: 2, f2: 3, f3: 4,
        f4: 5, f5: 6, f6: 7, f7: 8,
        f8: 9, f9: 10, f10: 11, f11: 12,
        f12: 13, f13: 14, f14: 15, f15: 16,
    }
    return x.f0
}
"#,
    );

    let name = find_func_export(&bytes, "make").expect("module must export `make`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");
    let make = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`make` must have type () → i32");

    let before = memory.data_size(&mut store);
    for _ in 0..10_000 {
        assert_eq!(make.call(&mut store, ()).expect("`make` must not trap"), 1);
    }
    let after = memory.data_size(&mut store);

    assert_eq!(
        after, before,
        "each @Destructor call must reclaim its receiver cell without a temporary copy"
    );
}

/// A composite without its own `@Destructor` still owns its heap cell: after the
/// field destructors run, `Destroy(root)` must release the container's cell too,
/// otherwise every drop leaks the container. `make_bundle` allocates a container
/// plus two field cells and drops them, so the host loop must not grow memory.
#[test]
fn surface_nested_destroy_reclaims_container_cell() {
    let bytes = compile_source(
        r#"
struct Handle { p: ptr[u8] }
@Destructor
func Handle.close(own self): void {
    free(self.p)
}

struct Bundle {
    a: Handle
    b: Handle
}

public func make_bundle(): i32 {
    let bundle = Bundle {
        a: Handle { p: nil },
        b: Handle { p: nil },
    }
    return 0
}
"#,
    );

    let name = find_func_export(&bytes, "make_bundle").expect("module must export `make_bundle`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");
    let make_bundle = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`make_bundle` must have type () → i32");

    let before = memory.data_size(&mut store);
    for _ in 0..50_000 {
        assert_eq!(
            make_bundle
                .call(&mut store, ())
                .expect("`make_bundle` must not trap"),
            0
        );
    }
    let after = memory.data_size(&mut store);

    assert_eq!(
        after, before,
        "dropping a composite must reclaim the container cell, not just its fields"
    );
}

#[test]
fn to_str_and_string_interp_with_f64_executes() {
    let interner = TypeInterner::new();
    let f64_ty = interner.intern(ArType::Primitive(Primitive::F64));
    let str_ty = interner.intern(ArType::Primitive(Primitive::Str));
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let pi_lit = pool.intern_float("3.14159");
    let pi_op = || AmirOperand::Constant(AmirConstant::Pool(pi_lit));

    let ret_t = TempId::from_usize(0);
    let str_t = TempId::from_usize(1);
    let len_t = TempId::from_usize(2);

    let mut stmts = AmirStmtTable::new();
    // str_t = ToStr(3.14159)
    stmts.push(AmirStmt::Assign {
        lhs: str_t,
        rhs: AmirRvalue::ToStr {
            value: pi_op(),
            src_ty: f64_ty,
        },
    });
    // len_t = Len(str_t)
    stmts.push(AmirStmt::Assign {
        lhs: len_t,
        rhs: AmirRvalue::Len(AmirOperand::Copy(str_t)),
    });
    // ret = len_t
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(len_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 3),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, str_ty), temp(2, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    // "3.14159" has length 7
    assert_eq!(run_main_i32(&bytes), 7);
}

#[test]
fn installed_comptime_corpus_executes_in_wasm() {
    let bytes = compile_source(include_str!(
        "../../../tests/projects/medium/comptime_core/src/main.aru"
    ));
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn runtime_range_loops_keep_ssa_values_and_exit_before_inclusive_overflow() {
    let bytes = compile_source(
        r#"
func first(): int { for value in 0..2 { return value }; return 99 }
func main(): int {
    if first() != 0 { return 1 }
    let max: u64 = 18446744073709551615
    let mut visits = 0
    for value in max..=max { if value != max { return 2 }; visits += 1 }
    if visits != 1 { return 3 }
    for value in 2..1 { return 4 }
    visits = 0
    for value in 1..=3 { if value == 2 { continue }; visits += value }
    if visits != 4 { return 5 }
    let mut sum = 0
    for value in [20, 22] { sum += value }
    if sum != 42 { return 6 }
    sum = 0
    for label in ["a", "b"] { if label == "a" { sum += 20 } else { sum += 22 } }
    return sum - 42
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn independent_string_results_restore_frames_and_reclaim_large_scratch() {
    let bytes = compile_source(
        r#"
func label(seed: int): str {
    let mut index = 0
    let mut sum = 0
    while index < 1000 {
        let row: [2048]u8 = [seed as u8; 2048]
        sum = sum + row[2047] as int
        index = index + 1
    }
    if sum > 0 { return "positive" }
    return "zero"
}
func forward(value: str, seed: int): str {
    let mut index = 0
    while index < 1000 {
        let row: [2048]u8 = [seed as u8; 2048]
        if row[2047] != seed as u8 { return "bad" }
        index = index + 1
    }
    return value
}
func main(): int {
    if label(1) == "positive" && label(0) == "zero" && forward("caller", 7) == "caller" { return 0 }
    return 1
}
"#,
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let main = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    // Warm allocator buckets once, then compare repeated invocations.
    assert_eq!(main.call(&mut store, ()).unwrap(), 0);
    let before = memory.data_size(&store);
    for _ in 0..50 {
        assert_eq!(main.call(&mut store, ()).unwrap(), 0);
    }
    assert_eq!(memory.data_size(&store), before);
}

#[test]
fn non_param_temps_reuse_wasm_locals_via_coloring() {
    let bytes = compile_source(
        r#"
public func main(): i32 {
    let a = 1 + 2
    let b = a + 3
    let c = b + 4
    let d = c + 5
    let e = d + 6
    let f = e + 7
    let g = f + 8
    let h = g + 9
    let i = h + 10
    let j = i + 11
    let k = j + 12
    let l = k + 13
    return l
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 91);
}

#[test]
fn borrowed_projections_preserve_pointer_loads_and_single_evaluation() {
    let bytes = compile_trusted_core_fixture(include_str!(
        "../../../tests/corpus/borrowed_projections.aru"
    ));
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_multi_field_variant_reads_each_payload_field() {
    // Each payload field is read at its own offset in the payload tuple; the
    // translator used to read field 0 for every binding.
    let bytes = compile_source(
        r#"
enum A {
    X(u8, i64)
    Y(bool, u16, i8)
}

func pick(a: A): int {
    return match a {
        A.X(p, q) => (p as int) * 100 + (q as int)
        A.Y(b, m, n) => if b { (m as int) + (n as int) } else { 0 }
    }
}

func main(): int {
    return pick(A.X(1 as u8, 23 as i64)) + pick(A.Y(true, 40 as u16, 2 as i8))
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 165);
}

#[test]
fn surface_float_special_values_format_without_leaving_the_caller() {
    // NaN and infinities used to `return` from the user's function mid-format.
    let bytes = compile_source(
        r#"
func main(): int {
    let b: f64 = 1e300 * 1e10
    let n: f64 = -b
    let z: f64 = b - b
    let c: f64 = 100.0
    let s = "${b}|${n}|${z}|${c}|${1.5}"
    if s == "inf|-inf|nan|100|1.5" { return 7 }
    return 9
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 7);
}
