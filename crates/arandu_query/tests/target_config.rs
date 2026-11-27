#![allow(clippy::unwrap_used, clippy::expect_used)]
//! TargetConfig drives pointer-width type checking (T038 usize literal bounds)
//! through the Salsa input instead of a host hardcode.

use arandu_middle::layout::DataLayout;
use arandu_middle::literal_pool::{parse_int_literal, AmirLiteralEntry};
use arandu_middle::DiagCode;
use arandu_middle::Severity;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

const SRC: &str = "func main(): int {\n    let x: usize = 4294967296\n    return 0\n}\n";

fn t038_count(db: &DatabaseImpl, file: arandu_query::SourceFile) -> usize {
    type_check(db, file)
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T038IntegerLiteralOutOfRange)
        .count()
}

#[test]
fn host_target_config_accepts_u64_literal() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("target_64.aru".into(), SRC.into());
    assert_eq!(
        t038_count(&db, file),
        0,
        "usize literal must fit on the host 64-bit layout"
    );
}

#[test]
fn target_config_32bit_flags_u64_literal() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file("target_32.aru".into(), SRC.into());
    assert_eq!(
        t038_count(&db, file),
        1,
        "usize literal beyond u32::MAX must be rejected on a 32-bit layout"
    );
}

#[test]
fn target_config_32bit_update_revalidates() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file("target_revalid.aru".into(), SRC.into());
    assert_eq!(t038_count(&db, file), 1);

    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(
        t038_count(&db, file),
        0,
        "edit of TargetConfig must invalidate target-dependent diagnostics"
    );
}

// `std.core.mem` resolvable in a bare query DB: registered under its import key.
const MEM_MODULE: &str = "module std.core.mem\n\nextern \"arandu-intrinsic\" {\n    func sizeOf<T>() : usize\n    func alignOf<T>() : usize\n}\n";

const MAIN_WITH_MEM_INTRINSICS: &str = "import std.core.mem as mem\nfunc main(): usize {\n    let a = mem.sizeOf<usize>()\n    let b = mem.sizeOf<u64>()\n    let c = mem.alignOf<usize>()\n    return a + b + c\n}\n";

fn folded_int_literals(db: &DatabaseImpl, file: arandu_query::SourceFile) -> Vec<i128> {
    let tc = type_check(db, file);
    assert!(
        !tc.diagnostics.iter().any(|d| d.severity == Severity::Error),
        "type check failed: {:?}",
        tc.diagnostics
    );
    let artifacts = lower_amir(db, file);
    assert!(
        !artifacts.amir.funcs.is_empty(),
        "surface program did not reach AMIR: {:?}",
        artifacts.type_check.diagnostics
    );
    let mut values: Vec<i128> = artifacts
        .amir
        .literal_pool
        .entries
        .iter()
        .filter_map(|e| match e {
            AmirLiteralEntry::Int(s) => parse_int_literal(s.as_str()),
            _ => None,
        })
        .collect();
    values.sort_unstable();
    values
}

#[test]
fn mem_sizeof_alignof_fold_with_target_pointer_width() {
    let mut db = DatabaseImpl::new();
    db.new_stdlib_file("stdlib/core/mem.aru".into(), MEM_MODULE.into());
    let file = db.new_file("main_mem.aru".into(), MAIN_WITH_MEM_INTRINSICS.into());

    // ILP32 natural layout: usize=4, u64=8, usize align=4.
    db.set_target_config(DataLayout::ptr_width(4));
    assert_eq!(
        folded_int_literals(&db, file),
        vec![4, 8],
        "mem.sizeOf/alignOf must fold to 32-bit layout constants"
    );

    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(
        folded_int_literals(&db, file),
        vec![8],
        "mem.sizeOf/alignOf must fold to 64-bit layout constants"
    );
}

#[test]
fn mem_intrinsics_preserve_non_natural_abi_alignments_in_runtime_and_ctfe() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_middle::types::FunctionInstance;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, ctfe_eval_instance, CtfeInstanceRequest, CtfeRequest};
    use arandu_query::passes::declaration_signatures;
    use arandu_query::runtime::Instance;

    let mut db = DatabaseImpl::new();
    db.new_stdlib_file("stdlib/core/mem.aru".into(), MEM_MODULE.into());
    let source = "import std.core.mem as mem\nstruct Pair { tag: u8, value: u64 }\nfunc main(): usize { return mem.alignOf<Pair>() }\n";
    let file = db.new_file("main.aru".into(), source.into());
    let public_file = db.new_file("public_layout.aru".into(), "import std.core.mem as mem\nstruct Pair { tag: u8, value: u64 }\nfunc main(): usize { return comptime mem.alignOf<Pair>() }".into());
    let definition = declaration_signatures(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "main")
        .expect("main definition")
        .id;
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    for (layout, alignment) in [
        (DataLayout::ptr_width(4), 8),
        (DataLayout::i686_sysv(), 4),
        (DataLayout::ptr_width(8), 8),
        (DataLayout::i686_sysv(), 4),
    ] {
        db.set_target_config(layout);
        assert_eq!(folded_int_literals(&db, file), vec![alignment]);
        assert_eq!(folded_int_literals(&db, public_file), vec![alignment]);
        let source_result = ctfe_eval(&db, CtfeRequest::new(&db, file, definition, vec![], budget));
        let instance = Instance::new(
            &db,
            file,
            FunctionInstance {
                definition,
                arguments: vec![],
            },
        );
        let concrete_result =
            ctfe_eval_instance(&db, CtfeInstanceRequest::new(&db, instance, vec![], budget));
        for (mode, result) in [("source", source_result), ("concrete", concrete_result)] {
            let ConstValue::Integer(value) = result
                .as_ref()
                .unwrap_or_else(|error| panic!("{mode} layout CTFE: {error:?}"))
            else {
                panic!("expected an alignment integer");
            };
            assert_eq!(value.value(), alignment);
        }
    }
}

#[test]
fn imported_layout_intrinsics_do_not_depend_on_sibling_bodies() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use salsa::Setter;

    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let module = format!("{MEM_MODULE}\nfunc sibling(): int {{ return 1 }}");
    let memory = db.new_stdlib_file("stdlib/core/mem.aru".into(), module.clone());
    let file = db.new_file(
        "main.aru".into(),
        "import std.core.mem as mem\nfunc main(): usize { return mem.sizeOf<u64>() }".into(),
    );
    let symbol = arandu_query::passes::declaration_signatures(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "main")
        .expect("main")
        .id;
    let budget = arandu_mir::ctfe::Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let first = ctfe_eval(&db, CtfeRequest::new(&db, file, symbol, vec![], budget)).clone();
    assert!(matches!(first, Ok(ConstValue::Integer(value)) if value.value() == 8));
    log.clear();
    memory
        .set_text(&mut db)
        .to(module.replace("return 1", "return 999").into());
    let next = ctfe_eval(&db, CtfeRequest::new(&db, file, symbol, vec![], budget));
    assert_eq!(&first, next);
    for query in ["ctfe_func_amir", "ctfe_extern_hir", "ctfe_eval"] {
        assert_eq!(
            log.count_executions_matching(query),
            0,
            "{query}: {}",
            log.format_chain(true)
        );
    }
}

#[test]
fn wasm32_target_rejects_thread_park_timeout_with_t099() {
    let mut db = DatabaseImpl::new();
    let wasm_identity = arandu_middle::db::TargetIdentity {
        os: "wasi".into(),
        arch: "wasm32".into(),
        triple: String::new(),
    };
    db.set_target(DataLayout::ptr_width(4), wasm_identity);
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );
    let file = db.new_file(
        "wasm_threads.aru".into(),
        r#"
import std.thread as thread

func main(): void {
    thread.parkTimeout(10)
}
"#
        .into(),
    );
    let result = type_check(&db, file);
    let t099 = result
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T099RequiresOperatingSystem)
        .expect("expected T099RequiresOperatingSystem on wasm32 for parkTimeout");
    assert!(
        t099.message.contains("requires target thread support"),
        "unexpected message: {}",
        t099.message
    );
}

#[test]
fn wasm32_target_rejects_thread_park_and_unpark_with_t099() {
    let mut db = DatabaseImpl::new();
    let wasm_identity = arandu_middle::db::TargetIdentity {
        os: "wasi".into(),
        arch: "wasm32".into(),
        triple: String::new(),
    };
    db.set_target(DataLayout::ptr_width(4), wasm_identity);
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );
    let file = db.new_file(
        "wasm_threads_park.aru".into(),
        r#"
import std.thread as thread

func main(): void {
    thread.park()
    thread.unpark(0 as usize)
}
"#
        .into(),
    );
    let result = type_check(&db, file);
    let t099_count = result
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T099RequiresOperatingSystem)
        .count();
    assert_eq!(
        t099_count, 2,
        "expected two T099 diagnostics for park and unpark on wasm32"
    );
}

#[test]
fn bit_intrinsics_evaluate_at_comptime() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::passes::declaration_signatures;

    let mut db = DatabaseImpl::new();
    db.new_stdlib_file(
        "stdlib/core/intrinsics.aru".into(),
        include_str!("../../../stdlib/core/intrinsics.aru").into(),
    );
    let source = r#"
import std.core.intrinsics as intrinsics

func test_popcnt(): u32 {
    let a: u32 = 305419896
    return intrinsics.bitCountOnes32(a)
}

func test_popcnt64(): u32 {
    let a: u64 = 305419896
    return intrinsics.bitCountOnes64(a)
}

func test_clz(): u32 {
    let a: u32 = 65535
    return intrinsics.bitLeadingZeros32(a)
}

func test_ctz(): u32 {
    let a: u32 = 65536
    return intrinsics.bitTrailingZeros32(a)
}

func test_bitrev(): u32 {
    let a: u32 = 305419896
    return intrinsics.bitReverse32(a)
}

func test_bitrev64(): u64 {
    let a: u64 = 305419896
    return intrinsics.bitReverse64(a)
}
"#;
    let file = db.new_file("test_bits.aru".into(), source.into());
    let sigs = declaration_signatures(&db, file);
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let eval_func = |name: &str| -> i128 {
        let sym = sigs.symbols.iter().find(|s| s.name == name).expect(name).id;
        let res = ctfe_eval(&db, CtfeRequest::new(&db, file, sym, vec![], budget))
            .as_ref()
            .unwrap();
        match res {
            ConstValue::Integer(v) => v.value(),
            other => panic!("expected integer for {name}, got {other:?}"),
        }
    };

    assert_eq!(
        eval_func("test_popcnt"),
        (305419896_u32).count_ones() as i128
    );
    assert_eq!(
        eval_func("test_popcnt64"),
        (305419896_u64).count_ones() as i128
    );
    assert_eq!(eval_func("test_clz"), (65535_u32).leading_zeros() as i128);
    assert_eq!(eval_func("test_ctz"), (65536_u32).trailing_zeros() as i128);
    assert_eq!(
        eval_func("test_bitrev"),
        (305419896_u32).reverse_bits() as i128
    );
    assert_eq!(
        eval_func("test_bitrev64"),
        (305419896_u64).reverse_bits() as i128
    );
}

#[test]
fn rotate_and_swap_intrinsics_evaluate_at_comptime() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::passes::declaration_signatures;

    let mut db = DatabaseImpl::new();
    db.new_stdlib_file(
        "stdlib/core/intrinsics.aru".into(),
        include_str!("../../../stdlib/core/intrinsics.aru").into(),
    );
    let source = r#"
import std.core.intrinsics as intrinsics

func rotl32(): u32 {
    let a: u32 = 2147483649
    return intrinsics.bitRotateLeft32(a, 1)
}

func rotl32_wraps(): u32 {
    let a: u32 = 305419896
    return intrinsics.bitRotateLeft32(a, 36)
}

func rotr32(): u32 {
    let a: u32 = 305419896
    return intrinsics.bitRotateRight32(a, 8)
}

func rotl64(): u64 {
    let a: u64 = 81985529216486895
    return intrinsics.bitRotateLeft64(a, 13)
}

func rotr64(): u64 {
    let a: u64 = 81985529216486895
    return intrinsics.bitRotateRight64(a, 77)
}

func bswap32(): u32 {
    let a: u32 = 305419896
    return intrinsics.bitSwapBytes32(a)
}

func bswap64(): u64 {
    let a: u64 = 81985529216486895
    return intrinsics.bitSwapBytes64(a)
}
"#;
    let file = db.new_file("test_rotate.aru".into(), source.into());
    let sigs = declaration_signatures(&db, file);
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let eval_func = |name: &str| -> i128 {
        let sym = sigs.symbols.iter().find(|s| s.name == name).expect(name).id;
        let res = ctfe_eval(&db, CtfeRequest::new(&db, file, sym, vec![], budget))
            .as_ref()
            .unwrap();
        match res {
            ConstValue::Integer(v) => v.value(),
            other => panic!("expected integer for {name}, got {other:?}"),
        }
    };

    let a64 = 81_985_529_216_486_895_u64;
    assert_eq!(
        eval_func("rotl32"),
        i128::from(2_147_483_649_u32.rotate_left(1))
    );
    assert_eq!(
        eval_func("rotl32_wraps"),
        i128::from(305_419_896_u32.rotate_left(4))
    );
    assert_eq!(
        eval_func("rotr32"),
        i128::from(305_419_896_u32.rotate_right(8))
    );
    assert_eq!(eval_func("rotl64"), i128::from(a64.rotate_left(13)));
    assert_eq!(eval_func("rotr64"), i128::from(a64.rotate_right(13)));
    assert_eq!(eval_func("bswap32"), i128::from(0x7856_3412_u32));
    assert_eq!(eval_func("bswap64"), i128::from(a64.swap_bytes()));
}

#[test]
fn fma_and_spin_intrinsics_evaluate_at_comptime() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::passes::declaration_signatures;

    let mut db = DatabaseImpl::new();
    db.new_stdlib_file(
        "stdlib/core/intrinsics.aru".into(),
        include_str!("../../../stdlib/core/intrinsics.aru").into(),
    );
    let source = r#"
import std.core.intrinsics as intrinsics

func test_fma32(): f32 {
    let a: f32 = 2.5
    let b: f32 = 4.0
    let c: f32 = 1.25
    return intrinsics.fma32(a, b, c)
}

func test_fma64(): f64 {
    let a: f64 = 3.0
    let b: f64 = 7.0
    let c: f64 = 0.5
    intrinsics.spinLoopHint()
    return intrinsics.fma64(a, b, c)
}
"#;
    let file = db.new_file("test_fma.aru".into(), source.into());
    let sigs = declaration_signatures(&db, file);
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let eval_float = |name: &str| -> f64 {
        let sym = sigs.symbols.iter().find(|s| s.name == name).expect(name).id;
        let res = ctfe_eval(&db, CtfeRequest::new(&db, file, sym, vec![], budget))
            .as_ref()
            .unwrap();
        match res {
            ConstValue::Float(f) => {
                if f.ty().bit_width() == 32 {
                    f32::from_bits(f.bits() as u32) as f64
                } else {
                    f64::from_bits(f.bits())
                }
            }
            other => panic!("expected float for {name}, got {other:?}"),
        }
    };

    assert_eq!(eval_float("test_fma32"), 11.25);
    assert_eq!(eval_float("test_fma64"), 21.5);
}

#[test]
fn overflow_and_saturating_intrinsics_evaluate_at_comptime() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::passes::declaration_signatures;

    let mut db = DatabaseImpl::new();
    db.new_stdlib_file(
        "stdlib/core/intrinsics.aru".into(),
        include_str!("../../../stdlib/core/intrinsics.aru").into(),
    );
    let source = include_str!("../../arandu_backend_c/tests/overflow_saturating_intrinsics.aru");
    // Strip the fixture's local `extern "arandu-intrinsic"` header or verify CTFE against std.core.intrinsics.
    let ctfe_source = r#"
import std.core.intrinsics as intrinsics

func run_ctfe(): int {
    let max32: i32 = 2147483647 as i32
    let p = intrinsics.addWithOverflowI32(max32, 1 as i32)
    if !p.1 { return 1 }
    let m = intrinsics.mulWithOverflowU64(4294967296 as u64, 4294967296 as u64)
    if !m.1 { return 2 }
    if intrinsics.saturatingAddI32(max32, 5 as i32) != max32 { return 3 }
    if intrinsics.saturatingSubU64(3 as u64, 9 as u64) != (0 as u64) { return 4 }
    return 0
}
"#;
    let _ = source;
    let file = db.new_file("test_overflow_ctfe.aru".into(), ctfe_source.into());
    let sigs = declaration_signatures(&db, file);
    let sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "run_ctfe")
        .expect("run_ctfe")
        .id;
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let res = ctfe_eval(&db, CtfeRequest::new(&db, file, sym, vec![], budget))
        .as_ref()
        .unwrap();
    match res {
        ConstValue::Integer(v) => assert_eq!(v.value(), 0),
        other => panic!("expected integer 0, got {other:?}"),
    }
}

#[test]
fn likely_and_unlikely_intrinsics_evaluate_at_comptime() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::passes::declaration_signatures;

    let mut db = DatabaseImpl::new();
    db.new_stdlib_file(
        "stdlib/core/intrinsics.aru".into(),
        include_str!("../../../stdlib/core/intrinsics.aru").into(),
    );
    let source = r#"
import std.core.intrinsics as intrinsics

func run_ctfe(): int {
    let x = 10
    if intrinsics.likely(x > 3) && !intrinsics.unlikely(x < 0) {
        return 7
    }
    return 1
}
"#;
    let file = db.new_file("test_branch_hints_ctfe.aru".into(), source.into());
    let sigs = declaration_signatures(&db, file);
    let sym = sigs
        .symbols
        .iter()
        .find(|s| s.name == "run_ctfe")
        .expect("run_ctfe")
        .id;
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let res = ctfe_eval(&db, CtfeRequest::new(&db, file, sym, vec![], budget))
        .as_ref()
        .unwrap();
    match res {
        ConstValue::Integer(v) => assert_eq!(v.value(), 7),
        other => panic!("expected integer 7, got {other:?}"),
    }
}
