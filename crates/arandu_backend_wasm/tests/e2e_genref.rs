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
            visibility: arandu_middle::Visibility::Public,
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
        debug_blocks: Vec::new(),
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
fn generic_free_invalidates_genref_before_ordinary_or_generational_reuse() {
    for (recycle, read_fresh) in [(0, false), (1, false), (2, false), (2, true)] {
        let interner = TypeInterner::new();
        let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
        let gen_ty = interner.intern(ArType::GenRef);
        let byte_ty = interner.intern(ArType::Primitive(Primitive::U8));
        let ptr_ty = interner.intern(ArType::Ptr(byte_ty));
        let mut pool = AmirLiteralPool::default();
        let original = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("7")));
        let fresh = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("99")));
        let size = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("4")));
        let old_gen = TempId::from_usize(1);
        let new_gen = TempId::from_usize(3);
        let mut statements = AmirStmtTable::new();
        statements.push(AmirStmt::Assign {
            lhs: old_gen,
            rhs: AmirRvalue::GenInsert {
                value: original,
                payload_ty: i32_ty,
                arena: GenArenaDomain::CompilerManaged,
                origin: arandu_base::Span::new(0, 0, 0),
            },
        });
        statements.push(AmirStmt::Free(AmirOperand::Copy(old_gen)));
        if recycle == 1 {
            statements.push(AmirStmt::Assign {
                lhs: TempId::from_usize(2),
                rhs: AmirRvalue::Alloc(size),
            });
        } else if recycle == 2 {
            statements.push(AmirStmt::Assign {
                lhs: new_gen,
                rhs: AmirRvalue::GenInsert {
                    value: fresh,
                    payload_ty: i32_ty,
                    arena: GenArenaDomain::CompilerManaged,
                    origin: arandu_base::Span::new(0, 0, 0),
                },
            });
        }
        statements.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::GenGet {
                gen_ref: AmirOperand::Copy(if read_fresh { new_gen } else { old_gen }),
                payload_ty: i32_ty,
                arena: GenArenaDomain::CompilerManaged,
                origin: arandu_base::Span::new(0, 0, 0),
            },
        });
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, statements.len()),
            terminator: AmirTerminator::Return,
        }];
        let func = AmirFunc {
            symbol: foreign_sym(1),
            return_type: i32_ty,
            receiver: None,
            params: vec![],
            locals: vec![],
            temps: vec![
                temp(0, i32_ty),
                temp(1, gen_ty),
                temp(2, ptr_ty),
                temp(3, gen_ty),
            ],
            cfg: compute_cfg_edges(&blocks),
            blocks,
            block_params: vec![],
            stmts: statements,
        };
        let bytes = emit_one(func, &interner, &mut pool);
        if read_fresh {
            assert_eq!(
                run_main_i32(&bytes),
                99,
                "a new generation has a valid odd tag"
            );
        } else {
            assert!(
                run_main_traps(&bytes),
                "generic Free must invalidate stale handle, recycle={recycle}"
            );
        }
    }
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

    // Exercise the last representable generation without billions of allocations.
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    memory
        .write(&mut store, 0x1fe8, &(i32::MAX as u32 - 1).to_le_bytes())
        .unwrap();
    let main = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .unwrap();
    assert_eq!(main.call(&mut store, ()).unwrap(), 42);
    assert!(
        main.call(&mut store, ()).is_err(),
        "generation exhaustion must trap"
    );
    let mut counter = [0; 4];
    memory.read(&store, 0x1fe8, &mut counter).unwrap();
    assert_eq!(u32::from_le_bytes(counter), i32::MAX as u32);
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

#[test]
fn genref_use_after_remove_traps() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_42 = pool.intern_int("42");

    let ret_t = TempId::from_usize(0);
    let val_t = TempId::from_usize(1);
    let gen_t = TempId::from_usize(2);
    let removed_t = TempId::from_usize(3);

    let temps = vec![
        temp(0, i32_ty),
        temp(1, i32_ty),
        temp(2, gen_ty),
        temp(3, i32_ty),
    ];

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
        lhs: removed_t,
        rhs: AmirRvalue::GenRemove {
            gen_ref: AmirOperand::Copy(gen_t),
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
        statements: DenseRange::new(0, 4),
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
    assert!(
        run_main_traps(&bytes),
        "GenGet on a removed GenRef handle must trap"
    );
}

#[test]
fn genref_stale_copy_after_recycle_traps() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_11 = pool.intern_int("11");
    let lit_22 = pool.intern_int("22");

    let ret_t = TempId::from_usize(0);
    let val1_t = TempId::from_usize(1);
    let gen1_t = TempId::from_usize(2);
    let removed_t = TempId::from_usize(3);
    let val2_t = TempId::from_usize(4);
    let gen2_t = TempId::from_usize(5);

    let temps = vec![
        temp(0, i32_ty),
        temp(1, i32_ty),
        temp(2, gen_ty),
        temp(3, i32_ty),
        temp(4, i32_ty),
        temp(5, gen_ty),
    ];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: val1_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_11))),
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
        lhs: removed_t,
        rhs: AmirRvalue::GenRemove {
            gen_ref: AmirOperand::Copy(gen1_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: val2_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_22))),
    });
    // Re-inserting reuses the same allocator cell, but with generation 2.
    stmts.push(AmirStmt::Assign {
        lhs: gen2_t,
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(val2_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    // Accessing the stale handle `gen1_t` (generation 1) must still trap.
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(gen1_t),
            payload_ty: i32_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 6),
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
    assert!(
        run_main_traps(&bytes),
        "GenGet on a stale GenRef handle after cell reuse must trap"
    );
}

#[test]
fn genref_remove_f64_returns_payload_and_blackbox_preserves_value() {
    let interner = TypeInterner::new();
    let f64_ty = interner.intern(ArType::Primitive(Primitive::F64));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut pool = AmirLiteralPool::default();
    let lit_f64 = pool.intern_float("12.5");

    let ret_t = TempId::from_usize(0);
    let val_t = TempId::from_usize(1);
    let boxed_t = TempId::from_usize(2);
    let gen_t = TempId::from_usize(3);

    let temps = vec![
        temp(0, f64_ty),
        temp(1, f64_ty),
        temp(2, f64_ty),
        temp(3, gen_ty),
    ];

    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: val_t,
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_f64))),
    });
    stmts.push(AmirStmt::Assign {
        lhs: boxed_t,
        rhs: AmirRvalue::BlackBox {
            value: AmirOperand::Copy(val_t),
            value_ty: f64_ty,
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: gen_t,
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(boxed_t),
            payload_ty: f64_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::GenRemove {
            gen_ref: AmirOperand::Copy(gen_t),
            payload_ty: f64_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: arandu_base::Span::new(0, 0, 0),
        },
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 4),
        terminator: AmirTerminator::Return,
    }];
    let cfg = compute_cfg_edges(&blocks);

    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: f64_ty,
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
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must compile");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance =
        wasmtime::Instance::new(&mut store, &module, &[]).expect("module must instantiate");
    let main = instance
        .get_typed_func::<(), f64>(&mut store, "main")
        .expect("`main` must have type () -> f64");
    let res = main.call(&mut store, ()).expect("`main` must execute");
    assert_eq!(res, 12.5);
}
