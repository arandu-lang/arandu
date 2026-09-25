//! Hand-built AMIR regression tests. These do not depend on the surface
//! language; they pin the exact wasm translation contracts the fixes target
//! (SSA block parameters, per-type locals, stackifier ordering, aggregate
//! heap cells, canonical-ABI realloc and destructor/coroutine lowering).

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

mod common;

use arandu_backend_wasm::emit_wasm;
use arandu_middle::amir::AmirProgram;
use arandu_middle::amir::block::{AmirBasicBlock, BlockId, BlockParam};
use arandu_middle::amir::local::{LocalId, TempId};
use arandu_middle::amir::program::AmirFunc;
use arandu_middle::amir::stmt::{AmirStmt, AmirStmtTable, AmirTerminator};
use arandu_middle::amir::value::{AmirConstant, AmirOperand, AmirPlace, AmirRvalue};
use arandu_middle::cfg::compute_cfg_edges;
use arandu_middle::layout::DenseRange;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::ops::BinaryOp;
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use arandu_semantics::DiagCode;
use arandu_semantics::SymbolTable;
use common::{
    compile_source, emit_one, emit_with_imported_symbols, foreign_sym, register_imported_sym,
    run_main_f64, run_main_i32, run_main_i64, temp, wasm32,
};

#[test]
fn hand_built_loop_with_block_params_executes() {
    // bb0: goto bb1(1)
    // bb1(p0): p1 = p0 + 1; p2 = p1 < 5; br p2 ? bb2 : bb3
    // bb2: goto bb1(p1)
    // bb3: _0 = p1; return _0   → 5
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let bool = interner.intern(ArType::Primitive(Primitive::Bool));
    let mut pool = AmirLiteralPool::default();
    let lit_one = pool.intern_int("1");
    let lit_five = pool.intern_int("5");
    let one = || AmirOperand::Constant(AmirConstant::Pool(lit_one));
    let five = || AmirOperand::Constant(AmirConstant::Pool(lit_five));

    let ret_t = TempId::from_usize(0);
    let p0 = TempId::from_usize(1);
    let next_t = TempId::from_usize(2);
    let cond_t = TempId::from_usize(3);
    let temps = vec![temp(0, int), temp(1, int), temp(2, int), temp(3, bool)];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: next_t,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Add,
            left: AmirOperand::Copy(p0),
            right: one(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: cond_t,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Lt,
            left: AmirOperand::Copy(next_t),
            right: five(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(next_t)),
    });

    let bb1_body = DenseRange::new(0, 2);
    let bb3_body = DenseRange::new(2, 1);
    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: vec![one()],
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            params: DenseRange::new(0, 1),
            statements: bb1_body,
            terminator: AmirTerminator::Branch {
                condition: AmirOperand::Copy(cond_t),
                if_true: BlockId::from_usize(2),
                true_args: vec![],
                if_false: BlockId::from_usize(3),
                false_args: vec![],
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(2),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: vec![AmirOperand::Copy(next_t)],
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(3),
            params: DenseRange::empty(),
            statements: bb3_body,
            terminator: AmirTerminator::Return,
        },
    ];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps,
        blocks,
        block_params: vec![BlockParam {
            id: p0,
            local: arandu_middle::amir::local::LocalId::from_usize(0),
            ty: int,
            from: None,
            moved: false,
        }],
        stmts,
        cfg,
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 5);
}

#[test]
fn hand_built_f64_locals_execute() {
    // _0: f64 = 1.5 * 2.0 → 3.0. Declaring these temps as `i32` (the former
    // behaviour) makes the module fail wasm validation, so execution pins the
    // per-type local declaration.
    let interner = TypeInterner::new();
    let f64 = interner.intern(ArType::Primitive(Primitive::F64));
    let mut pool = AmirLiteralPool::default();
    let lit_a = pool.intern_float("1.5");
    let lit_b = pool.intern_float("2.0");
    let a = || AmirOperand::Constant(AmirConstant::Pool(lit_a));
    let b = || AmirOperand::Constant(AmirConstant::Pool(lit_b));

    let ret_t = TempId::from_usize(0);
    let left_t = TempId::from_usize(1);
    let right_t = TempId::from_usize(2);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: left_t,
        rhs: AmirRvalue::Use(a()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: right_t,
        rhs: AmirRvalue::Use(b()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Mul,
            left: AmirOperand::Copy(left_t),
            right: AmirOperand::Copy(right_t),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 3),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: f64,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, f64), temp(1, f64), temp(2, f64)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_f64(&bytes), 3.0);
}

#[test]
fn hand_built_i64_logical_and_and_or_execute() {
    // 64-bit binary ops previously fell through to `emit_zero` for the
    // logical `And`/`Or`; a 64-bit *result* drives the I64 path, so the
    // boolean normalization must use the I64 comparison too:
    //   r = (3 != 0) & (0 != 0)  → 0
    //   ret = r | (1 != 0)        → 1
    let interner = TypeInterner::new();
    let i64 = interner.intern(ArType::Primitive(Primitive::I64));
    let mut pool = AmirLiteralPool::default();
    let lit_three = pool.intern_int("3");
    let lit_zero = pool.intern_int("0");
    let lit_one = pool.intern_int("1");
    let three = || AmirOperand::Constant(AmirConstant::Pool(lit_three));
    let zero = || AmirOperand::Constant(AmirConstant::Pool(lit_zero));
    let one = || AmirOperand::Constant(AmirConstant::Pool(lit_one));

    // temps 0..4 are all i64.
    let ret_t = TempId::from_usize(0);
    let a_t = TempId::from_usize(1);
    let b_t = TempId::from_usize(2);
    let r_t = TempId::from_usize(3);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: a_t,
        rhs: AmirRvalue::Use(three()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: b_t,
        rhs: AmirRvalue::Use(zero()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: r_t,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::And,
            left: AmirOperand::Copy(a_t),
            right: AmirOperand::Copy(b_t),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Or,
            left: AmirOperand::Copy(r_t),
            right: one(),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 4),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: i64,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, i64), temp(1, i64), temp(2, i64), temp(3, i64)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i64(&bytes), 1);
}

#[test]
fn hand_built_slice_len_executes() {
    // Alloc → SliceView{data, len} → Len  ⇒ the runtime length read back from
    // the fat second slot equals the constructed length.
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let slice = interner.intern(ArType::Slice(int));
    let mut pool = AmirLiteralPool::default();
    let lit_size = pool.intern_int("64");
    let lit_len = pool.intern_int("1");
    let lit_zero = pool.intern_int("0");
    let size = || AmirOperand::Constant(AmirConstant::Pool(lit_size));
    let len = || AmirOperand::Constant(AmirConstant::Pool(lit_len));
    let zero = || AmirOperand::Constant(AmirConstant::Pool(lit_zero));

    let ret_t = TempId::from_usize(0);
    let buf_t = TempId::from_usize(1);
    let sv_t = TempId::from_usize(2);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: buf_t,
        rhs: AmirRvalue::Alloc(size()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: sv_t,
        rhs: AmirRvalue::SliceView {
            owner: zero(),
            data: AmirOperand::Copy(buf_t),
            len: len(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Len(AmirOperand::Copy(sv_t)),
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
        temps: vec![temp(0, int), temp(1, int), temp(2, slice)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 1);
}

#[test]
fn hand_built_slice_subslice_executes() {
    // Alloc → SliceView{data, len=10} → SliceSubslice{start=2, len=5} → Len ⇒ 5.
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let slice = interner.intern(ArType::Slice(int));
    let mut pool = AmirLiteralPool::default();
    let lit_size = pool.intern_int("64");
    let lit_len10 = pool.intern_int("10");
    let lit_start2 = pool.intern_int("2");
    let lit_len5 = pool.intern_int("5");
    let lit_zero = pool.intern_int("0");
    let size = || AmirOperand::Constant(AmirConstant::Pool(lit_size));
    let len10 = || AmirOperand::Constant(AmirConstant::Pool(lit_len10));
    let start2 = || AmirOperand::Constant(AmirConstant::Pool(lit_start2));
    let len5 = || AmirOperand::Constant(AmirConstant::Pool(lit_len5));
    let zero = || AmirOperand::Constant(AmirConstant::Pool(lit_zero));

    let ret_t = TempId::from_usize(0);
    let buf_t = TempId::from_usize(1);
    let sv_t = TempId::from_usize(2);
    let sub_t = TempId::from_usize(3);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: buf_t,
        rhs: AmirRvalue::Alloc(size()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: sv_t,
        rhs: AmirRvalue::SliceView {
            owner: zero(),
            data: AmirOperand::Copy(buf_t),
            len: len10(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: sub_t,
        rhs: AmirRvalue::SliceSubslice {
            slice: AmirOperand::Copy(sv_t),
            start: start2(),
            len: len5(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Len(AmirOperand::Copy(sub_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 4),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int), temp(2, slice), temp(3, slice)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 5);
}

#[test]
fn hand_built_slice_index_access_executes() {
    // Alloc → SliceView{data, len} → IndexAccess at 0: the bounds check reads
    // the length from the fat second slot and the element load goes through
    // the data pointer pushed from the first slot.
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let slice = interner.intern(ArType::Slice(int));
    let mut pool = AmirLiteralPool::default();
    let lit_size = pool.intern_int("64");
    let lit_len = pool.intern_int("1");
    let lit_zero = pool.intern_int("0");
    let size = || AmirOperand::Constant(AmirConstant::Pool(lit_size));
    let len = || AmirOperand::Constant(AmirConstant::Pool(lit_len));
    let zero = || AmirOperand::Constant(AmirConstant::Pool(lit_zero));

    let ret_t = TempId::from_usize(0);
    let buf_t = TempId::from_usize(1);
    let sv_t = TempId::from_usize(2);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: buf_t,
        rhs: AmirRvalue::Alloc(size()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: sv_t,
        rhs: AmirRvalue::SliceView {
            owner: zero(),
            data: AmirOperand::Copy(buf_t),
            len: len(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::IndexAccess {
            base: AmirOperand::Copy(sv_t),
            index: zero(),
        },
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
        temps: vec![temp(0, int), temp(1, int), temp(2, slice)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn invalid_projected_nonmemory_store_is_icegen002() {
    // A `Borrow` (address-taking) over a scalar local base with no memory
    // backing must be rejected as an internal compiler error report, not a
    // silent miscompile and not a panic.
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();

    let ret_t = TempId::from_usize(0);
    let base_local = LocalId::from_usize(1);
    let base = AmirPlace {
        local: base_local,
        projections: Default::default(),
    };

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Borrow(base),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 1),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        // Base local 1 is a plain scalar with no heap cell behind it.
        locals: vec![arandu_middle::amir::local::AmirLocal {
            id: base_local,
            ty: int,
            is_memory: false,
            symbol: None,
            span: arandu_base::Span::new(0, 0, 0),
            use_span: None,
        }],
        temps: vec![temp(0, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "main");
    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: std::mem::take(&mut pool),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };
    let result = emit_wasm(
        &program,
        &symbols,
        &interner,
        &arandu_semantics::TypeInfo::default(),
        wasm32(),
    );
    let err = result.expect_err("unaddressable borrow must be rejected");
    assert_eq!(err.code, DiagCode::ICEGEN002);
}

/// Every emitted module must export `cabi_realloc` with the correct signature:
/// `(i32, i32, i32, i32) → i32`.
#[test]
fn every_module_exports_cabi_realloc() {
    let bytes = compile_source("public func main(): i32 { return 42 }");

    let mut found_realloc = false;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        let payload = payload.expect("valid wasm");
        if let wasmparser::Payload::ExportSection(section) = payload {
            for entry in section {
                let entry = entry.expect("valid export entry");
                if entry.name == "cabi_realloc" {
                    found_realloc = true;
                }
            }
        }
    }
    assert!(found_realloc, "module must export `cabi_realloc`");
}

#[test]
fn hand_built_i32_logical_and_and_or_execute() {
    // 32-bit logical BinaryOp::And and BinaryOp::Or:
    // With non-trivial operands like 2 and 1:
    // (2 != 0) & (1 != 0) -> 1  (previously failed: 2 & 1 = 0 without normalization)
    // (2 != 0) | (0 != 0) -> 1
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let lit_two = pool.intern_int("2");
    let lit_one = pool.intern_int("1");
    let lit_zero = pool.intern_int("0");
    let two = || AmirOperand::Constant(AmirConstant::Pool(lit_two));
    let one = || AmirOperand::Constant(AmirConstant::Pool(lit_one));
    let zero = || AmirOperand::Constant(AmirConstant::Pool(lit_zero));

    let ret_t = TempId::from_usize(0);
    let a_t = TempId::from_usize(1);
    let b_t = TempId::from_usize(2);
    let r_and = TempId::from_usize(3);
    let r_or = TempId::from_usize(4);
    let temps = vec![
        temp(0, i32_ty),
        temp(1, i32_ty),
        temp(2, i32_ty),
        temp(3, i32_ty),
        temp(4, i32_ty),
    ];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: a_t,
        rhs: AmirRvalue::Use(two()),
    });
    stmts.push(AmirStmt::Assign {
        lhs: b_t,
        rhs: AmirRvalue::Use(one()),
    });
    // r_and = 2 && 1 -> must normalize to 1
    stmts.push(AmirStmt::Assign {
        lhs: r_and,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::And,
            left: AmirOperand::Copy(a_t),
            right: AmirOperand::Copy(b_t),
        },
    });
    // r_or = 2 || 0 -> must normalize to 1
    stmts.push(AmirStmt::Assign {
        lhs: r_or,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Or,
            left: AmirOperand::Copy(a_t),
            right: zero(),
        },
    });
    // ret = r_and + r_or -> 1 + 1 = 2
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Add,
            left: AmirOperand::Copy(r_and),
            right: AmirOperand::Copy(r_or),
        },
    });

    let b0 = AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 5),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![b0];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: i32_ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps,
        blocks,
        block_params: vec![],
        stmts,
        cfg,
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 2);
}

#[test]
fn hand_built_switch_int_non_consecutive_executes() {
    // Tests that non-consecutive SwitchInt branches to the correct case
    // target and not merely taking the first case because discriminant != 0.
    // bb0: disc = 20; switch disc { 10 => bb1, 20 => bb2, otherwise => bb3 }
    // bb1: ret = 100; return
    // bb2: ret = 200; return
    // bb3: ret = 300; return
    let interner = TypeInterner::new();
    let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let lit_twenty = pool.intern_int("20");
    let lit_100 = pool.intern_int("100");
    let lit_200 = pool.intern_int("200");
    let lit_300 = pool.intern_int("300");

    let ret_t = TempId::from_usize(0);
    let disc_t = TempId::from_usize(1);
    let temps = vec![temp(0, int_ty), temp(1, int_ty)];

    let mut stmts = AmirStmtTable::new();
    // stmt 0: disc = 20
    stmts.push(AmirStmt::Assign {
        lhs: disc_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_twenty))),
    });
    // stmt 1: ret = 100
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_100))),
    });
    // stmt 2: ret = 200
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_200))),
    });
    // stmt 3: ret = 300
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_300))),
    });

    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 1),
            terminator: AmirTerminator::SwitchInt {
                discriminant: AmirOperand::Copy(disc_t),
                targets: vec![
                    (10, BlockId::from_usize(1), vec![]),
                    (20, BlockId::from_usize(2), vec![]),
                ],
                otherwise: (BlockId::from_usize(3), vec![]),
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            params: DenseRange::empty(),
            statements: DenseRange::new(1, 1),
            terminator: AmirTerminator::Return,
        },
        AmirBasicBlock {
            id: BlockId::from_usize(2),
            params: DenseRange::empty(),
            statements: DenseRange::new(2, 1),
            terminator: AmirTerminator::Return,
        },
        AmirBasicBlock {
            id: BlockId::from_usize(3),
            params: DenseRange::empty(),
            statements: DenseRange::new(3, 1),
            terminator: AmirTerminator::Return,
        },
    ];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int_ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps,
        blocks,
        block_params: vec![],
        stmts,
        cfg,
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(
        run_main_i32(&bytes),
        200,
        "discriminant 20 must branch to bb2 and return 200"
    );
}

#[test]
fn hand_built_black_box_executes() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let lit_99 = pool.intern_int("99");
    let val_op = || AmirOperand::Constant(AmirConstant::Pool(lit_99));

    let ret_t = TempId::from_usize(0);
    let b_t = TempId::from_usize(1);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: b_t,
        rhs: AmirRvalue::BlackBox {
            value: val_op(),
            value_ty: int,
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(b_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 2),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 99);
}

#[test]
fn hand_built_relative_borrow_executes() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));

    let ret_t = TempId::from_usize(0);
    let r_t = TempId::from_usize(1);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: r_t,
        rhs: AmirRvalue::RelativeBorrow {
            local: arandu_middle::amir::local::LocalId::from_usize(42),
            mutable: false,
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(r_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 2),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let mut pool = AmirLiteralPool::default();
    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn hand_built_coroutine_ready_and_await_executes() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let lit_77 = pool.intern_int("77");
    let val_op = || AmirOperand::Constant(AmirConstant::Pool(lit_77));

    let ret_t = TempId::from_usize(0);
    let co_t = TempId::from_usize(1);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: co_t,
        rhs: AmirRvalue::CoroutineReady {
            value: val_op(),
            payload_ty: int,
            stack: false,
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Unary {
            op: arandu_middle::ops::UnaryOp::Await,
            operand: AmirOperand::Copy(co_t),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 2),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 77);
}

#[test]
fn hand_built_memory_intrinsics_executes() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));

    let ret_t = TempId::from_usize(0);
    let buf_t = TempId::from_usize(1);
    let read_t = TempId::from_usize(2);

    let mut pool = AmirLiteralPool::default();
    let lit_16 = pool.intern_int("16");
    let lit_42 = pool.intern_int("42");
    let sz_op = || AmirOperand::Constant(AmirConstant::Pool(lit_16));
    let val_op = || AmirOperand::Constant(AmirConstant::Pool(lit_42));

    let mut stmts = AmirStmtTable::new();
    // buf = Alloc(16)
    stmts.push(AmirStmt::Assign {
        lhs: buf_t,
        rhs: AmirRvalue::Alloc(sz_op()),
    });
    // Call ptrWrite(buf, 42)
    stmts.push(AmirStmt::Call {
        lhs: None,
        callee: AmirOperand::FunctionRef(foreign_sym(100)),
        args: smallvec::smallvec![AmirOperand::Copy(buf_t), val_op()],
        return_borrow: None,
    });
    // Call read_t = ptrRead(buf)
    stmts.push(AmirStmt::Call {
        lhs: Some(read_t),
        callee: AmirOperand::FunctionRef(foreign_sym(101)),
        args: smallvec::smallvec![AmirOperand::Copy(buf_t)],
        return_borrow: None,
    });
    // ret = read_t
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(read_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 4),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int), temp(2, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let extra = vec![
        (foreign_sym(100), "ptrWrite"),
        (foreign_sym(101), "ptrRead"),
    ];
    let bytes = emit_with_imported_symbols(func, &extra, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn hand_built_slice_intrinsics_executes() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let slice_ty = interner.intern(ArType::Slice(int));

    let ret_t = TempId::from_usize(0);
    let buf_t = TempId::from_usize(1);
    let slice_t = TempId::from_usize(2);
    let len_t = TempId::from_usize(3);

    let mut pool = AmirLiteralPool::default();
    let lit_32 = pool.intern_int("32");
    let lit_8 = pool.intern_int("8");
    let sz_op = || AmirOperand::Constant(AmirConstant::Pool(lit_32));
    let len_op = || AmirOperand::Constant(AmirConstant::Pool(lit_8));

    let mut stmts = AmirStmtTable::new();
    // buf = Alloc(32)
    stmts.push(AmirStmt::Assign {
        lhs: buf_t,
        rhs: AmirRvalue::Alloc(sz_op()),
    });
    // slice_t = sliceFromRaw(buf, 8)
    stmts.push(AmirStmt::Call {
        lhs: Some(slice_t),
        callee: AmirOperand::FunctionRef(foreign_sym(102)),
        args: smallvec::smallvec![AmirOperand::Copy(buf_t), len_op()],
        return_borrow: None,
    });
    // len_t = sliceLen(slice_t)
    stmts.push(AmirStmt::Call {
        lhs: Some(len_t),
        callee: AmirOperand::FunctionRef(foreign_sym(103)),
        args: smallvec::smallvec![AmirOperand::Copy(slice_t)],
        return_borrow: None,
    });
    // ret = len_t
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(len_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 4),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int), temp(2, slice_ty), temp(3, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let extra = vec![
        (foreign_sym(102), "sliceFromRaw"),
        (foreign_sym(103), "sliceLen"),
    ];
    let bytes = emit_with_imported_symbols(func, &extra, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 8);
}

#[test]
fn hand_built_slice_subslice_intrinsic_call_executes() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let slice_ty = interner.intern(ArType::Slice(int));
    let mut pool = AmirLiteralPool::default();
    let lit_len = pool.intern_int("10");
    let lit_start = pool.intern_int("2");
    let lit_sub_len = pool.intern_int("5");
    let len_op = || AmirOperand::Constant(AmirConstant::Pool(lit_len));
    let start_op = || AmirOperand::Constant(AmirConstant::Pool(lit_start));
    let sub_len_op = || AmirOperand::Constant(AmirConstant::Pool(lit_sub_len));

    let ret_t = TempId::from_usize(0);
    let buf_t = TempId::from_usize(1);
    let slice_t = TempId::from_usize(2);
    let subslice_t = TempId::from_usize(3);
    let res_len_t = TempId::from_usize(4);

    let mut stmts = AmirStmtTable::new();
    // buf_t = alloc(40)
    stmts.push(AmirStmt::Assign {
        lhs: buf_t,
        rhs: AmirRvalue::Alloc(AmirOperand::Constant(AmirConstant::Pool(
            pool.intern_int("40"),
        ))),
    });
    // slice_t = sliceFromRaw(buf_t, 10)
    stmts.push(AmirStmt::Call {
        lhs: Some(slice_t),
        callee: AmirOperand::FunctionRef(foreign_sym(102)),
        args: smallvec::smallvec![AmirOperand::Copy(buf_t), len_op()],
        return_borrow: None,
    });
    // subslice_t = sliceSubslice(slice_t, 2, 5)
    stmts.push(AmirStmt::Call {
        lhs: Some(subslice_t),
        callee: AmirOperand::FunctionRef(foreign_sym(104)),
        args: smallvec::smallvec![AmirOperand::Copy(slice_t), start_op(), sub_len_op()],
        return_borrow: None,
    });
    // res_len_t = sliceLen(subslice_t)
    stmts.push(AmirStmt::Call {
        lhs: Some(res_len_t),
        callee: AmirOperand::FunctionRef(foreign_sym(103)),
        args: smallvec::smallvec![AmirOperand::Copy(subslice_t)],
        return_borrow: None,
    });
    // ret = res_len_t (5)
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(res_len_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 5),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![
            temp(0, int),
            temp(1, int),
            temp(2, slice_ty),
            temp(3, slice_ty),
            temp(4, int),
        ],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let extra = vec![
        (foreign_sym(102), "sliceFromRaw"),
        (foreign_sym(103), "sliceLen"),
        (foreign_sym(104), "sliceSubslice"),
    ];
    let bytes = emit_with_imported_symbols(func, &extra, &interner, &mut pool);
    assert_eq!(run_main_i32(&bytes), 5);
}

#[test]
fn switch_int_with_block_params_executes() {
    // SwitchInt on discriminant with block arguments passed to target blocks
    // bb0: switch discriminant { 0 => bb1(42), 1 => bb1(99) } otherwise => bb1(0)
    // bb1(param): return param
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let lit_zero = pool.intern_int("0");
    let lit_forty_two = pool.intern_int("42");
    let lit_ninety_nine = pool.intern_int("99");

    let ret_t = TempId::from_usize(0);
    let p0 = TempId::from_usize(1);

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(p0)),
    });

    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::SwitchInt {
                discriminant: AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("1"))),
                targets: vec![
                    (
                        0,
                        BlockId::from_usize(1),
                        vec![AmirOperand::Constant(AmirConstant::Pool(lit_forty_two))],
                    ),
                    (
                        1,
                        BlockId::from_usize(1),
                        vec![AmirOperand::Constant(AmirConstant::Pool(lit_ninety_nine))],
                    ),
                ],
                otherwise: (
                    BlockId::from_usize(1),
                    vec![AmirOperand::Constant(AmirConstant::Pool(lit_zero))],
                ),
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            params: DenseRange::new(0, 1),
            statements: DenseRange::new(0, 1),
            terminator: AmirTerminator::Return,
        },
    ];

    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, int)],
        blocks: blocks.clone(),
        block_params: vec![arandu_middle::amir::BlockParam {
            id: p0,
            local: arandu_middle::amir::local::LocalId::from_usize(0),
            ty: int,
            from: None,
            moved: false,
        }],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    // Case 1 passes 99 to bb1
    assert_eq!(run_main_i32(&bytes), 99);
}
