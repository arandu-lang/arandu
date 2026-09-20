//! End-to-end execution tests for GenRef / GenArena operations on WebAssembly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_backend_wasm::emit_wasm;
use arandu_middle::amir::AmirProgram;
use arandu_middle::amir::block::{AmirBasicBlock, BlockId};
use arandu_middle::amir::local::{AmirTemp, TempId};
use arandu_middle::amir::program::AmirFunc;
use arandu_middle::amir::stmt::{AmirStmt, AmirStmtTable, AmirTerminator};
use arandu_middle::amir::value::{AmirConstant, AmirOperand, AmirRvalue, GenArenaDomain};
use arandu_middle::cfg::compute_cfg_edges;
use arandu_middle::layout::{DataLayout, DenseRange};
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::symbol_table::{Symbol, SymbolId, SymbolKind};
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
use arandu_semantics::SymbolTable;

fn wasm32() -> DataLayout {
    DataLayout::ptr_width(4)
}

fn foreign_sym(local_id: u32) -> SymbolId {
    SymbolId::new(999, local_id)
}

fn register_imported_sym(symbols: &mut SymbolTable, id: SymbolId, name: &str) {
    symbols.imported_symbols.insert(
        id,
        Symbol {
            id,
            name: name.into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );
}

fn temp(id: usize, ty: TypeId) -> AmirTemp {
    AmirTemp {
        id: TempId::from_usize(id),
        ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_base::Span::new(0, 0, 0),
    }
}

fn emit_one(func: AmirFunc, interner: &TypeInterner, pool: &mut AmirLiteralPool) -> Vec<u8> {
    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "main");
    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: std::mem::take(pool),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
    };
    emit_wasm(
        &program,
        &symbols,
        interner,
        &arandu_semantics::TypeInfo::default(),
        wasm32(),
    )
    .expect("hand-built genref program must emit")
}

fn run_main_i32(bytes: &[u8]) -> i32 {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("module must compile");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance =
        wasmtime::Instance::new(&mut store, &module, &[]).expect("module must instantiate");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .expect("`main` must have type () -> i32");
    main.call(&mut store, ()).expect("`main` must execute")
}

fn run_main_traps(bytes: &[u8]) -> bool {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("module must compile");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance =
        wasmtime::Instance::new(&mut store, &module, &[]).expect("module must instantiate");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .expect("`main` must have type () -> i32");
    main.call(&mut store, ()).is_err()
}

#[test]
fn genref_insert_and_get_scalar() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_42 = pool.intern_int("42");

    let ret_t = TempId::from_usize(0); // result i32
    let val_t = TempId::from_usize(1); // literal 42
    let gen_t = TempId::from_usize(2); // GenRef

    let temps = vec![temp(0, i32_ty), temp(1, i32_ty), temp(2, gen_ty)];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: val_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_42))),
    });
    stmts.push(AmirStmt::Assign {
        lhs: gen_t,
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(val_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(gen_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 3),
        terminator: AmirTerminator::Return,
    }];
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
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn genref_insert_set_and_get() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_10 = pool.intern_int("10");
    let lit_99 = pool.intern_int("99");

    let ret_t = TempId::from_usize(0);
    let val1_t = TempId::from_usize(1);
    let gen1_t = TempId::from_usize(2);
    let val2_t = TempId::from_usize(3);
    let gen2_t = TempId::from_usize(4);

    let temps = vec![
        temp(0, i32_ty),
        temp(1, i32_ty),
        temp(2, gen_ty),
        temp(3, i32_ty),
        temp(4, gen_ty),
    ];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: val1_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_10))),
    });
    stmts.push(AmirStmt::Assign {
        lhs: gen1_t,
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(val1_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: val2_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_99))),
    });
    stmts.push(AmirStmt::Assign {
        lhs: gen2_t,
        rhs: AmirRvalue::GenSet {
            gen_ref: AmirOperand::Copy(gen1_t),
            value: AmirOperand::Copy(val2_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(gen2_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 5),
        terminator: AmirTerminator::Return,
    }];
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
    assert_eq!(run_main_i32(&bytes), 99);
}

#[test]
fn genref_upsert_zero_and_existing() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_77 = pool.intern_int("77");
    let lit_88 = pool.intern_int("88");

    let ret_t = TempId::from_usize(0);
    let zero_gen_t = TempId::from_usize(1);
    let val1_t = TempId::from_usize(2);
    let gen1_t = TempId::from_usize(3);
    let val2_t = TempId::from_usize(4);
    let gen2_t = TempId::from_usize(5);

    let temps = vec![
        temp(0, i32_ty),
        temp(1, gen_ty),
        temp(2, i32_ty),
        temp(3, gen_ty),
        temp(4, i32_ty),
        temp(5, gen_ty),
    ];

    let mut stmts = AmirStmtTable::new();
    // zero_gen_t is uninitialized / zero fat pointer by default
    stmts.push(AmirStmt::Assign {
        lhs: val1_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_77))),
    });
    // Upsert on zero handle -> allocates new
    stmts.push(AmirStmt::Assign {
        lhs: gen1_t,
        rhs: AmirRvalue::GenUpsert {
            gen_ref: AmirOperand::Copy(zero_gen_t),
            value: AmirOperand::Copy(val1_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: val2_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_88))),
    });
    // Upsert on existing handle -> updates value
    stmts.push(AmirStmt::Assign {
        lhs: gen2_t,
        rhs: AmirRvalue::GenUpsert {
            gen_ref: AmirOperand::Copy(gen1_t),
            value: AmirOperand::Copy(val2_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(gen2_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 5),
        terminator: AmirTerminator::Return,
    }];
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
    assert_eq!(run_main_i32(&bytes), 88);
}

#[test]
fn genref_remove_returns_payload() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_123 = pool.intern_int("123");

    let ret_t = TempId::from_usize(0);
    let val_t = TempId::from_usize(1);
    let gen_t = TempId::from_usize(2);

    let temps = vec![temp(0, i32_ty), temp(1, i32_ty), temp(2, gen_ty)];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: val_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_123))),
    });
    stmts.push(AmirStmt::Assign {
        lhs: gen_t,
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(val_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    // Remove moves payload out and frees cell
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenRemove {
            gen_ref: AmirOperand::Copy(gen_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 3),
        terminator: AmirTerminator::Return,
    }];
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
    assert_eq!(run_main_i32(&bytes), 123);
}

#[test]
fn genref_get_zero_handle_traps() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();

    let ret_t = TempId::from_usize(0);
    let zero_gen_t = TempId::from_usize(1);

    let temps = vec![temp(0, i32_ty), temp(1, gen_ty)];

    let mut stmts = AmirStmtTable::new();
    // zero_gen_t is 0 (null handle)
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(zero_gen_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 1),
        terminator: AmirTerminator::Return,
    }];
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
    assert!(run_main_traps(&bytes), "GenGet on zero handle must trap");
}
