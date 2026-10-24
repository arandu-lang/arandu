#![cfg(target_pointer_width = "64")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Cranelift↔LLVM parity for string interpolation / ToStr and GenRef ABI ops.
//!
//! Linked execution uses `clang` + `libarandu_runtime.a`, matching
//! `arandu run -b llvm` / `arandu build -b llvm`.

use arandu_backend_cranelift::CraneliftBackend;
use arandu_backend_llvm::emit_llvm_ir;
use arandu_base::span::Span;
use arandu_semantics::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt,
    AmirStmtTable, AmirTemp, AmirTerminator, BlockId, GenArenaDomain, TempId,
};
use arandu_semantics::cfg::compute_cfg_edges;
use arandu_semantics::layout::DenseRange;
use arandu_semantics::literal_pool::AmirLiteralPool;
use arandu_semantics::types::{ArType, Primitive, TypeInterner};
use arandu_semantics::{
    SymbolKind, SymbolTable, TypeCheckResult, TypeInfo, lower_to_amir_with_interfaces,
    lower_to_hir, resolve_for_test, type_check,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

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

fn tool_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn locate_runtime() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("ARANDU_RUNTIME_LIB") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let exe = std::env::current_exe().ok()?;
    let (prefix, extension) = if cfg!(windows) {
        ("arandu_runtime-", "lib")
    } else {
        ("libarandu_runtime-", "a")
    };
    for ancestor in exe.ancestors().take(6) {
        let deps = if ancestor.join("deps").is_dir() {
            ancestor.join("deps")
        } else {
            ancestor.to_path_buf()
        };
        let Ok(entries) = fs::read_dir(&deps) else {
            continue;
        };
        let mut candidates = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.starts_with(prefix))
                    && path.extension().and_then(|ext| ext.to_str()) == Some(extension)
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let modified = |path: &Path| {
                fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            };
            modified(left)
                .cmp(&modified(right))
                .then_with(|| left.cmp(right))
        });
        if let Some(path) = candidates.pop() {
            return Some(path);
        }
    }
    None
}

fn run_linked(ir: &str) -> Option<i32> {
    if !tool_available("clang") {
        return None;
    }
    let runtime = locate_runtime()?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let stem = format!("arandu-llvm-parity-{}-{id}", std::process::id());
    let dir = std::env::temp_dir();
    let ir_path = dir.join(format!("{stem}.ll"));
    let bin_path = dir.join(if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem
    });
    fs::write(&ir_path, ir).expect("write IR");
    let mut compile = Command::new("clang");
    compile
        .arg(&ir_path)
        .arg(&runtime)
        .arg("-O0")
        .arg("-o")
        .arg(&bin_path);
    compile.args(arandu_middle::TargetSpec::for_host().cc_link_args());
    let status = compile.status().expect("start clang");
    let _ = fs::remove_file(&ir_path);
    if !status.success() {
        let _ = fs::remove_file(&bin_path);
        panic!("clang failed to link LLVM IR with {}", runtime.display());
    }
    let output = Command::new(&bin_path).output().expect("run linked binary");
    let _ = fs::remove_file(&bin_path);
    Some(output.status.code().unwrap_or(1))
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

fn assert_llvm_matches_cranelift(src: &str, expect_runtime_helpers: bool) {
    let ir = emit(src);
    if expect_runtime_helpers {
        assert!(
            ir.contains("ar_rt_raw_malloc")
                || ir.contains("ar_str_concat")
                || ir.contains("ar_jit_"),
            "expected interpolation/ToStr runtime helpers in IR:\n{ir}"
        );
    }
    let cranelift = run_cranelift(src);
    let Some(llvm) = run_linked(&ir) else {
        return;
    };
    assert_eq!(
        llvm, cranelift,
        "Cranelift={cranelift} LLVM={llvm}\n--- IR ---\n{ir}"
    );
}

#[test]
fn string_interpolation_matches_cranelift() {
    assert_llvm_matches_cranelift(
        r#"
func main(): int {
    let name = "Bruno"
    let a = "Oi, ${name}"
    let b = "Oi, $name"
    if a != "Oi, Bruno" { return 1 }
    if b != "Oi, Bruno" { return 2 }
    return 0
}
"#,
        true,
    );
}

#[test]
fn to_str_interpolation_matches_cranelift() {
    assert_llvm_matches_cranelift(
        r#"
func main(): int {
    let n: int = 42
    let text = "n=${n}:b=${true}:f=${1.5}"
    if text != "n=42:b=true:f=1.5" { return 1 }
    return 0
}
"#,
        true,
    );
}

#[test]
fn nested_interpolation_matches_cranelift() {
    assert_llvm_matches_cranelift(
        r#"
func main(): int {
    if "before:${"n=${42}"}:after" != "before:n=42:after" { return 1 }
    return 0
}
"#,
        // The whole expression constant-folds to the expected literal, so the
        // module legitimately references no interpolation helper. The parity
        // assertion below still covers semantics.
        false,
    );
}

#[test]
fn gen_insert_get_matches_cranelift_exit_code() {
    let interner = TypeInterner::new();
    let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
    let tuple_ty = interner.intern(ArType::tuple(&[int_ty, int_ty], &interner));
    let gen_ty = interner.intern(ArType::GenRef);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Tuple {
            items: vec![
                AmirOperand::Constant(AmirConstant::Pool(
                    arandu_semantics::literal_pool::LiteralId(0),
                )),
                AmirOperand::Constant(AmirConstant::Pool(
                    arandu_semantics::literal_pool::LiteralId(1),
                )),
            ],
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(TempId::from_usize(1)),
            payload_ty: tuple_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(3),
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(TempId::from_usize(2)),
            payload_ty: tuple_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::FieldAccess {
            base: AmirOperand::Copy(TempId::from_usize(3)),
            field: 1,
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 4),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let mut pool = AmirLiteralPool::default();
    assert_eq!(pool.intern_int("42").0, 0);
    assert_eq!(pool.intern_int("43").0, 1);

    let mut symbols = SymbolTable::new(0);
    let scope = symbols.global_scope();
    let main_sym = symbols
        .define(scope, "main", SymbolKind::Func, Span::new(0, 0, 0))
        .expect("define main");

    let func = AmirFunc {
        symbol: main_sym,
        return_type: int_ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![
            AmirTemp {
                id: TempId::from_usize(0),
                ty: int_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            },
            AmirTemp {
                id: TempId::from_usize(1),
                ty: tuple_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            },
            AmirTemp {
                id: TempId::from_usize(2),
                ty: gen_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            },
            AmirTemp {
                id: TempId::from_usize(3),
                ty: tuple_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            },
        ],
        blocks,
        block_params: Vec::new(),
        stmts,
        cfg,
    };
    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: pool,
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };
    let type_info = TypeInfo {
        type_interner: interner,
        ..Default::default()
    };
    let ir = emit_llvm_ir(&program, &symbols, &type_info).expect("emit GenRef IR");
    assert!(
        ir.contains("ar_gen_insert_raw") && ir.contains("ar_gen_get_raw"),
        "expected GenRef runtime calls:\n{ir}"
    );

    let cranelift = {
        let module = CraneliftBackend::try_new()
            .expect("Cranelift backend")
            .compile(&program, &symbols, &type_info)
            .expect("Cranelift GenRef compile");
        let main = unsafe { module.get_fn::<unsafe fn() -> i64>("main") }.expect("main");
        unsafe { main() }
    };
    let Some(llvm) = run_linked(&ir) else {
        return;
    };
    assert_eq!(i64::from(llvm), cranelift);
    assert_eq!(llvm, 43);
}
