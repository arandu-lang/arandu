use super::*;
use crate::SymbolId;
use crate::amir::program::extend_block_range;
use crate::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt,
    AmirStmtTable, AmirTemp, AmirTerminator, BlockId, InstrId, TempId,
};
use crate::cfg::compute_cfg_edges;
use crate::layout::DenseRange;
use crate::literal_pool::AmirLiteralPool;
use crate::ops::BinaryOp;
use crate::pass_manager::{OptLevel, PassManager};
use crate::types::{ArType, Primitive, TypeInterner};

fn intern_int() -> crate::types::TypeId {
    TypeInterner::new().intern(ArType::Primitive(Primitive::Int))
}

fn make_func(
    sym_id: usize,
    blocks: Vec<(Vec<AmirStmt>, AmirTerminator)>,
    temps: Vec<AmirTemp>,
    params: Vec<TempId>,
) -> AmirFunc {
    let mut stmts = AmirStmtTable::new();
    let mut amir_blocks = Vec::with_capacity(blocks.len());

    for (b_idx, (stmt_list, term)) in blocks.into_iter().enumerate() {
        let mut range = DenseRange::empty();
        for stmt in stmt_list {
            let instr = stmts.push(stmt);
            extend_block_range(&mut range, instr);
        }
        amir_blocks.push(AmirBasicBlock {
            id: BlockId::from_usize(b_idx),
            statements: range,
            params: DenseRange::empty(),
            terminator: term,
        });
    }

    let cfg = compute_cfg_edges(&amir_blocks);
    AmirFunc {
        symbol: SymbolId::new(0, sym_id as u32),
        return_type: intern_int(),
        receiver: None,
        params,
        locals: Vec::new(),
        temps,
        blocks: amir_blocks,
        block_params: Vec::new(),
        stmts,
        cfg,
    }
}

#[test]
fn test_leaf_cost_evaluation() {
    let int_ty = intern_int();
    let temp0 = AmirTemp {
        id: TempId(0),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_lexer::Span::new(0, 0, 0),
    };

    // 1. Simple leaf returning constant: cost = 0
    let leaf = make_func(
        1,
        vec![(vec![], AmirTerminator::Return)],
        vec![temp0.clone()],
        vec![],
    );
    assert_eq!(cost::evaluate_leaf_inlining(&leaf, 25), Some(0));

    // 2. Leaf with Call: ineligible
    let with_call = make_func(
        2,
        vec![(
            vec![AmirStmt::Call {
                lhs: Some(TempId(0)),
                callee: AmirOperand::FunctionRef(SymbolId::new(0, 99)),
                args: smallvec::smallvec![],
                return_borrow: None,
            }],
            AmirTerminator::Return,
        )],
        vec![temp0.clone()],
        vec![],
    );
    assert_eq!(cost::evaluate_leaf_inlining(&with_call, 25), None);

    // 3. Leaf with cycle (loop): ineligible
    let cyclic = make_func(
        3,
        vec![
            (
                vec![],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![],
                },
            ),
            (
                vec![],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(0),
                    args: vec![],
                },
            ),
        ],
        vec![temp0],
        vec![],
    );
    assert_eq!(cost::evaluate_leaf_inlining(&cyclic, 25), None);
}

#[test]
fn test_leaf_inlining_and_sccp_folding() {
    let int_ty = intern_int();
    let mut pool = AmirLiteralPool::default();
    let c10 = pool.intern_int("10");
    let c5 = pool.intern_int("5");

    // Callee: add_ten(x) -> x + 10
    // temps: _0 (return), _1 (param x)
    let callee_temp0 = AmirTemp {
        id: TempId(0),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_lexer::Span::new(0, 0, 0),
    };
    let callee_temp1 = AmirTemp {
        id: TempId(1),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_lexer::Span::new(0, 0, 0),
    };
    let callee_sym = SymbolId::new(0, 10);
    let mut callee = make_func(
        10,
        vec![(
            vec![AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Binary {
                    op: BinaryOp::Add,
                    left: AmirOperand::Copy(TempId(1)),
                    right: AmirOperand::Constant(AmirConstant::Pool(c10)),
                },
            }],
            AmirTerminator::Return,
        )],
        vec![callee_temp0, callee_temp1],
        vec![TempId(1)],
    );
    callee.symbol = callee_sym;

    // Caller: main() -> add_ten(5)
    // temps: _0 (return), _1 (arg), _2 (result of call)
    let caller_temp0 = AmirTemp {
        id: TempId(0),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_lexer::Span::new(0, 0, 0),
    };
    let caller_temp1 = AmirTemp {
        id: TempId(1),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_lexer::Span::new(0, 0, 0),
    };
    let caller_temp2 = AmirTemp {
        id: TempId(2),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_lexer::Span::new(0, 0, 0),
    };

    let caller_sym = SymbolId::new(0, 20);
    let mut caller = make_func(
        20,
        vec![(
            vec![
                AmirStmt::Assign {
                    lhs: TempId(1),
                    rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(c5))),
                },
                AmirStmt::Call {
                    lhs: Some(TempId(2)),
                    callee: AmirOperand::FunctionRef(callee_sym),
                    args: smallvec::smallvec![AmirOperand::Copy(TempId(1))],
                    return_borrow: None,
                },
                AmirStmt::Assign {
                    lhs: TempId(0),
                    rhs: AmirRvalue::Use(AmirOperand::Copy(TempId(2))),
                },
            ],
            AmirTerminator::Return,
        )],
        vec![caller_temp0, caller_temp1, caller_temp2],
        vec![],
    );
    caller.symbol = caller_sym;

    let mut program = AmirProgram {
        funcs: vec![caller, callee],
        literal_pool: pool,
        extern_funcs: rustc_hash::FxHashMap::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    // Run leaf inlining
    let inlined = inline_leaf_functions(&mut program);
    assert_eq!(inlined, 1, "Expected exactly 1 call site inlined");

    // After inlining, caller must NOT contain any Call statements
    let caller_after = &program.funcs[0];
    for b in &caller_after.blocks {
        for id in b.statements.iter_ids::<InstrId>() {
            if let Some(stmt) = caller_after.try_stmt(id) {
                assert!(
                    !matches!(stmt, AmirStmt::Call { .. }),
                    "Call was not eliminated by inlining"
                );
            }
        }
    }

    // Now run PassManager at O1 to verify SCCP + DCE + CFG simplification clean it up completely
    PassManager::for_level(OptLevel::O1)
        .run_program(&mut program)
        .unwrap();

    // Verify caller returns constant 15!
    let caller_optimized = &program.funcs[0];
    let mut found_const_15 = false;
    for b in &caller_optimized.blocks {
        for id in b.statements.iter_ids::<InstrId>() {
            if let Some(AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(lit_id))),
            }) = caller_optimized.try_stmt(id)
                && *lhs == TempId(0)
            {
                let entry = program.literal_pool.get(*lit_id);
                if let crate::literal_pool::AmirLiteralEntry::Int(s) = entry
                    && s == "15"
                {
                    found_const_15 = true;
                }
            }
        }
    }
    assert!(
        found_const_15,
        "Expected caller to fold add_ten(5) directly into 15"
    );
}
