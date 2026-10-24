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

#[test]
fn inlining_respects_remaining_budget_and_structural_ceiling() {
    let ty = intern_int();
    let temp = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };
    let mut pool = AmirLiteralPool::default();
    let value = pool.intern_int("42");
    let leaf = make_func(
        1,
        vec![(
            vec![
                AmirStmt::Assign {
                    lhs: TempId(0),
                    rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(value))),
                };
                24
            ],
            AmirTerminator::Return,
        )],
        vec![temp.clone()],
        vec![],
    );
    let call = AmirStmt::Call {
        lhs: Some(TempId(0)),
        callee: AmirOperand::FunctionRef(SymbolId::new(0, 1)),
        args: smallvec::smallvec![],
        return_borrow: None,
    };
    let mut program = AmirProgram {
        funcs: vec![
            make_func(
                2,
                vec![(vec![call.clone(); 3], AmirTerminator::Return)],
                vec![temp.clone()],
                vec![],
            ),
            leaf.clone(),
        ],
        literal_pool: pool,
        extern_funcs: FxHashMap::default(),
        debug_bindings: vec![],
        debug_blocks: vec![],
    };
    assert_eq!(inline_leaf_functions(&mut program), 2);
    assert_eq!(
        program.funcs[0]
            .stmts
            .payloads
            .iter()
            .filter(|stmt| matches!(stmt, AmirStmt::Call { .. }))
            .count(),
        1
    );

    let mut statements = vec![AmirStmt::Nop; MAX_CALLER_IR_SIZE - 4];
    statements.push(call);
    let caller = make_func(
        2,
        vec![(statements, AmirTerminator::Return)],
        vec![temp],
        vec![],
    );
    program.funcs = vec![caller, leaf];
    let pristine_size = program.funcs[0].stmts.len();
    assert_eq!(inline_leaf_functions(&mut program), 0);
    assert_eq!(program.funcs[0].stmts.len(), pristine_size);
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
fn wrappers_become_inlinable_without_resetting_the_call_site_budget() {
    let ty = intern_int();
    let result = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };
    let wrapper = |symbol, callee| {
        make_func(
            symbol,
            vec![(
                vec![AmirStmt::Call {
                    lhs: Some(TempId(0)),
                    callee: AmirOperand::FunctionRef(SymbolId::new(0, callee)),
                    args: smallvec::smallvec![],
                    return_borrow: None,
                }],
                AmirTerminator::Return,
            )],
            vec![result.clone()],
            vec![],
        )
    };
    let mut pool = AmirLiteralPool::default();
    let value = pool.intern_int("42");
    let leaf = make_func(
        3,
        vec![(
            vec![AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(value))),
            }],
            AmirTerminator::Return,
        )],
        vec![result.clone()],
        vec![],
    );
    for reversed in [false, true] {
        let mut funcs = vec![wrapper(1, 2), wrapper(2, 3), leaf.clone()];
        if reversed {
            funcs.reverse();
        }
        let mut program = AmirProgram {
            funcs,
            literal_pool: pool.clone(),
            extern_funcs: FxHashMap::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        // The exact count depends on how deep each round splices: a wrapper is now
        // eligible as a non-leaf, so a chain may resolve in a different order
        // than when only leaves qualified. What must hold is that every call is
        // gone afterwards and that a second pass adds nothing.
        assert!(inline_leaf_functions(&mut program) >= 2);
        for func in &program.funcs {
            for block in &func.blocks {
                assert!(
                    func.block_stmts(block.id)
                        .all(|stmt| !matches!(stmt, AmirStmt::Call { .. })),
                    "wrapper chain must fully inline without resetting the budget"
                );
            }
        }
        assert_eq!(inline_leaf_functions(&mut program), 0);
    }
}

/// Regression: the per-caller budget must scale with caller size.
///
/// A caller holding more than `MIN_INLINES_PER_CALLER` eligible sites used to
/// be truncated at 32, so `ch`/`maj`/constant getters beyond that point stayed
/// as real calls. That is what left SHA-256's compression body with 400
/// `callq` and zero `ror` instructions.
#[test]
fn large_caller_inlines_more_than_the_floor_budget() {
    let ty = intern_int();
    let mut pool = AmirLiteralPool::default();
    let c1 = pool.intern_int("1");

    // Leaf: t0 = 1
    let leaf_temp = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };
    let leaf = make_func(
        1,
        vec![(
            vec![AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(c1))),
            }],
            AmirTerminator::Return,
        )],
        vec![leaf_temp],
        vec![],
    );

    // Caller with 40 call sites — above the old hard cap of 32.
    let sites = 40usize;
    let mut temps = vec![AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    }];
    for i in 0..sites {
        temps.push(AmirTemp {
            id: TempId::from_usize(i + 1),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        });
    }
    let caller_stmts: Vec<AmirStmt> = (0..sites)
        .map(|i| AmirStmt::Call {
            lhs: Some(TempId::from_usize(i + 1)),
            callee: AmirOperand::FunctionRef(SymbolId::new(0, 1)),
            args: smallvec::smallvec![],
            return_borrow: None,
        })
        .collect();
    let caller = make_func(
        2,
        vec![(caller_stmts, AmirTerminator::Return)],
        temps,
        vec![],
    );

    let mut program = AmirProgram {
        funcs: vec![caller, leaf],
        literal_pool: pool,
        extern_funcs: FxHashMap::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    let inlined = inline_leaf_functions(&mut program);
    assert_eq!(
        inlined, sites,
        "every eligible site in a caller of this size must be inlined"
    );
    for block in &program.funcs[0].blocks {
        assert!(
            program.funcs[0]
                .block_stmts(block.id)
                .all(|stmt| !matches!(stmt, AmirStmt::Call { .. })),
            "no call to an inlinable leaf may survive"
        );
    }
}

/// The caller must spend its budget cheapest-callee-first so trivial constant
/// getters are removed before a multi-statement helper consumes the budget.
#[test]
fn cheapest_callee_is_inlined_before_a_costlier_one() {
    let ty = intern_int();
    let mut pool = AmirLiteralPool::default();
    let c7 = pool.intern_int("7");

    let base = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };

    // Cheap leaf: t0 = 7
    let cheap = make_func(
        1,
        vec![(
            vec![AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(c7))),
            }],
            AmirTerminator::Return,
        )],
        vec![base.clone()],
        vec![],
    );

    // Costlier leaf: t1 = t0 + t0 + t0 (cost 3 vs 1)
    let mut costly_temps = vec![base.clone()];
    costly_temps.push(AmirTemp {
        id: TempId(1),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    });
    let costly = make_func(
        2,
        vec![(
            vec![
                AmirStmt::Assign {
                    lhs: TempId(0),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Copy(TempId(0)),
                        right: AmirOperand::Copy(TempId(0)),
                    },
                },
                AmirStmt::Assign {
                    lhs: TempId(1),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Copy(TempId(0)),
                        right: AmirOperand::Copy(TempId(1)),
                    },
                },
            ],
            AmirTerminator::Return,
        )],
        costly_temps,
        vec![],
    );

    // Caller calls the costly leaf first, then the cheap one.
    let caller_temps = vec![
        base.clone(),
        AmirTemp {
            id: TempId(1),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        },
        AmirTemp {
            id: TempId(2),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        },
    ];
    let caller = make_func(
        3,
        vec![(
            vec![
                AmirStmt::Call {
                    lhs: Some(TempId(1)),
                    callee: AmirOperand::FunctionRef(SymbolId::new(0, 2)),
                    args: smallvec::smallvec![],
                    return_borrow: None,
                },
                AmirStmt::Call {
                    lhs: Some(TempId(2)),
                    callee: AmirOperand::FunctionRef(SymbolId::new(0, 1)),
                    args: smallvec::smallvec![],
                    return_borrow: None,
                },
            ],
            AmirTerminator::Return,
        )],
        caller_temps,
        vec![],
    );

    let mut program = AmirProgram {
        funcs: vec![caller, costly, cheap],
        literal_pool: pool,
        extern_funcs: FxHashMap::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    assert_eq!(inline_leaf_functions(&mut program), 2);

    // The cheap leaf's body (the bare constant) must appear before the costly
    // leaf's two adds, proving cost ordering rather than source order.
    let stmts: Vec<String> = program.funcs[0]
        .blocks
        .iter()
        .flat_map(|b| program.funcs[0].block_stmts(b.id))
        .map(|s| format!("{s:?}"))
        .collect();
    let cheap_at = stmts
        .iter()
        .position(|s| s.contains("Constant"))
        .expect("cheap leaf body inlined");
    let costly_at = stmts
        .iter()
        .position(|s| s.contains("Add"))
        .expect("costly leaf body inlined");
    assert!(
        cheap_at < costly_at,
        "cheapest callee must be inlined first, got cheap@{cheap_at} costly@{costly_at}"
    );
}

/// Builds `bb0(preheader) -> bb1(header) -> bb2(body) -> bb1`, plus `bb3` exit.
///
/// `body_extra` supplies the body statements. The header takes two block
/// params (`limit`, `i`) fed from the preheader and the latch.
fn build_loop_ir(ty: crate::types::TypeId, body_extra: Vec<AmirStmt>) -> AmirFunc {
    let mut stmts = AmirStmtTable::new();
    let mut blocks = Vec::new();
    let mut pool = AmirLiteralPool::default();
    let one = pool.intern_int("1");

    // bb0: jump into the header passing the loop limit and zero.
    blocks.push(AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::empty(),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: BlockId::from_usize(1),
            args: vec![AmirOperand::Copy(TempId(0)), AmirOperand::Copy(TempId(1))],
        },
    });

    // bb1 (header): while i < limit
    let cond = stmts.push(AmirStmt::Assign {
        lhs: TempId(11),
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Lt,
            left: AmirOperand::Copy(TempId(11)),
            right: AmirOperand::Copy(TempId(10)),
        },
    });
    let mut r1 = DenseRange::empty();
    extend_block_range(&mut r1, cond);
    blocks.push(AmirBasicBlock {
        id: BlockId::from_usize(1),
        statements: r1,
        params: DenseRange::new(0, 2),
        terminator: AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId(11)),
            if_true: BlockId::from_usize(2),
            true_args: vec![],
            if_false: BlockId::from_usize(3),
            false_args: vec![],
        },
    });

    // bb2 (body): i = i + 1, then whatever the test supplied.
    let bump = stmts.push(AmirStmt::Assign {
        lhs: TempId(4),
        rhs: AmirRvalue::Binary {
            op: BinaryOp::Add,
            left: AmirOperand::Copy(TempId(11)),
            right: AmirOperand::Constant(AmirConstant::Pool(one)),
        },
    });
    let mut r2 = DenseRange::empty();
    extend_block_range(&mut r2, bump);
    for s in body_extra {
        let id = stmts.push(s);
        extend_block_range(&mut r2, id);
    }
    blocks.push(AmirBasicBlock {
        id: BlockId::from_usize(2),
        statements: r2,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: BlockId::from_usize(1),
            args: vec![AmirOperand::Copy(TempId(10)), AmirOperand::Copy(TempId(4))],
        },
    });

    // bb3 (exit): return the accumulator.
    let ret = stmts.push(AmirStmt::Assign {
        lhs: TempId(0),
        rhs: AmirRvalue::Use(AmirOperand::Copy(TempId(5))),
    });
    let mut r3 = DenseRange::empty();
    extend_block_range(&mut r3, ret);
    blocks.push(AmirBasicBlock {
        id: BlockId::from_usize(3),
        statements: r3,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    });

    let temps: Vec<AmirTemp> = (0..12)
        .map(|i| AmirTemp {
            id: TempId::from_usize(i),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        })
        .collect();

    let cfg = compute_cfg_edges(&blocks);
    AmirFunc {
        symbol: SymbolId::new(0, 1),
        return_type: ty,
        receiver: None,
        params: vec![TempId(0), TempId(1)],
        locals: Vec::new(),
        temps,
        blocks,
        block_params: vec![
            crate::amir::BlockParam {
                id: TempId(10),
                local: crate::amir::LocalId::from_usize(0),
                ty,
                from: None,
                moved: false,
            },
            crate::amir::BlockParam {
                id: TempId(11),
                local: crate::amir::LocalId::from_usize(1),
                ty,
                from: None,
                moved: false,
            },
        ],
        stmts,
        cfg,
    }
}

fn has_mul(func: &AmirFunc, bid: BlockId) -> bool {
    func.block_stmts(bid).any(|s| {
        matches!(
            s,
            AmirStmt::Assign {
                rhs: AmirRvalue::Binary {
                    op: BinaryOp::Mul,
                    ..
                },
                ..
            }
        )
    })
}

/// A loop whose multiplicand does not change must have the multiply hoisted
/// into the preheader, so the body keeps no per-iteration multiply.
#[test]
fn invariant_multiply_is_hoisted_out_of_the_loop() {
    let ty = intern_int();
    let mut pool = AmirLiteralPool::default();
    let two = pool.intern_int("2");

    // t3 = t1 * 2  (t1 is a parameter, therefore invariant)
    let mut func = build_loop_ir(
        ty,
        vec![AmirStmt::Assign {
            lhs: TempId(3),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Mul,
                left: AmirOperand::Copy(TempId(1)),
                right: AmirOperand::Constant(AmirConstant::Pool(two)),
            },
        }],
    );
    // The multiply must not start in the body, or the test proves nothing.
    assert!(has_mul(&func, BlockId::from_usize(2)));

    assert!(
        crate::licm::licm(&mut func),
        "the invariant multiply must be reported as hoisted"
    );
    assert!(
        !has_mul(&func, BlockId::from_usize(1)) && !has_mul(&func, BlockId::from_usize(2)),
        "multiply must not remain inside the loop"
    );
    assert!(
        has_mul(&func, BlockId::from_usize(0)),
        "multiply must be hoisted into the preheader"
    );
}

/// A multiply that depends on the loop variable must stay in the body.
#[test]
fn loop_variant_multiply_is_not_hoisted() {
    let ty = intern_int();
    let mut pool = AmirLiteralPool::default();
    let two = pool.intern_int("2");

    // t3 = i * 2  where i is the header block param — varies per iteration.
    let mut func = build_loop_ir(
        ty,
        vec![AmirStmt::Assign {
            lhs: TempId(3),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Mul,
                left: AmirOperand::Copy(TempId(11)),
                right: AmirOperand::Constant(AmirConstant::Pool(two)),
            },
        }],
    );
    assert!(has_mul(&func, BlockId::from_usize(2)));

    let _ = crate::licm::licm(&mut func);

    assert!(
        !has_mul(&func, BlockId::from_usize(0)),
        "a loop-variant multiply must never be hoisted"
    );
    assert!(
        has_mul(&func, BlockId::from_usize(2)),
        "the loop-variant multiply must stay in the body"
    );
}

/// LICM must never move a potentially-trapping load into a preheader.
#[test]
fn load_is_never_hoisted() {
    let ty = intern_int();
    let mut func = build_loop_ir(
        ty,
        vec![AmirStmt::Assign {
            lhs: TempId(3),
            rhs: AmirRvalue::Load(crate::amir::AmirPlace {
                local: crate::amir::LocalId::from_usize(0),
                projections: smallvec::SmallVec::new(),
            }),
        }],
    );
    let _ = crate::licm::licm(&mut func);
    let hoisted_load = func.block_stmts(BlockId::from_usize(0)).any(|s| {
        matches!(
            s,
            AmirStmt::Assign {
                rhs: AmirRvalue::Load(_),
                ..
            }
        )
    });
    assert!(
        !hoisted_load,
        "a load may trap and must not be speculated into the preheader"
    );
}

#[test]
fn trapping_and_address_operations_are_never_speculated() {
    for op in [
        BinaryOp::Div,
        BinaryOp::Mod,
        BinaryOp::ShiftLeft,
        BinaryOp::ShiftRight,
    ] {
        let mut function = build_loop_ir(
            intern_int(),
            vec![AmirStmt::Assign {
                lhs: TempId(3),
                rhs: AmirRvalue::Binary {
                    op,
                    left: AmirOperand::Copy(TempId(1)),
                    right: AmirOperand::Copy(TempId(0)),
                },
            }],
        );
        assert!(!crate::licm::licm(&mut function), "{op:?}");
    }
    let mut function = build_loop_ir(
        intern_int(),
        vec![AmirStmt::Assign {
            lhs: TempId(3),
            rhs: AmirRvalue::Unary {
                op: crate::ops::UnaryOp::Deref,
                operand: AmirOperand::Copy(TempId(1)),
            },
        }],
    );
    assert!(!crate::licm::licm(&mut function));
}

#[test]
fn invariant_header_parameter_is_not_available_in_preheader() {
    let mut function = build_loop_ir(
        intern_int(),
        vec![AmirStmt::Assign {
            lhs: TempId(3),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Mul,
                left: AmirOperand::Copy(TempId(10)),
                right: AmirOperand::Copy(TempId(1)),
            },
        }],
    );
    assert!(!crate::licm::licm(&mut function));
}

#[test]
fn identical_return_cleanup_is_shared_without_reordering_it() {
    let ty = intern_int();
    let temp = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };
    let cleanup = vec![
        AmirStmt::Free(AmirOperand::Copy(TempId(0))),
        AmirStmt::Free(AmirOperand::Copy(TempId(1))),
    ];
    let mut parameter = temp.clone();
    parameter.id = TempId(1);
    let mut function = make_func(
        1,
        vec![
            (
                Vec::new(),
                AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId(1),
                    true_args: Vec::new(),
                    if_false: BlockId(2),
                    false_args: Vec::new(),
                },
            ),
            (cleanup.clone(), AmirTerminator::Return),
            (cleanup, AmirTerminator::Return),
        ],
        vec![temp, parameter],
        vec![TempId(1)],
    );
    assert!(crate::tail_merge::merge_return_tails(&mut function));
    assert_eq!(function.stmts.len(), 2);
    assert_eq!(
        function
            .blocks
            .iter()
            .filter(|block| matches!(block.terminator, AmirTerminator::Return))
            .count(),
        1
    );
    assert_eq!(function.predecessors(BlockId(3)), &[BlockId(1), BlockId(2)]);
    assert!(!crate::tail_merge::merge_return_tails(&mut function));
    assert!(matches!(
        function.block_stmts(BlockId(3)).next(),
        Some(AmirStmt::Free(AmirOperand::Copy(TempId(0))))
    ));
}

/// A function without loops is untouched.
#[test]
fn straight_line_function_is_unchanged() {
    let ty = intern_int();
    let func = make_func(
        1,
        vec![(
            vec![AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Binary {
                    op: BinaryOp::Add,
                    left: AmirOperand::Copy(TempId(0)),
                    right: AmirOperand::Copy(TempId(0)),
                },
            }],
            AmirTerminator::Return,
        )],
        vec![AmirTemp {
            id: TempId(0),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        }],
        vec![TempId(0)],
    );
    let mut func = func;
    assert!(!crate::licm::licm(&mut func));
}

/// A wrapper whose body is just calls must be inlinable even though it is not a
/// leaf. This is the `beU32Buf` → `bufGet` shape from SHA-256 that left 16
/// `callq` instructions in the compression body.
#[test]
fn nonleaf_wrapper_is_inlinable_when_small() {
    let ty = intern_int();
    let mut pool = AmirLiteralPool::default();
    let c5 = pool.intern_int("5");

    let base = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };

    // Inner helper: t0 = 5
    let inner = make_func(
        1,
        vec![(
            vec![AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(c5))),
            }],
            AmirTerminator::Return,
        )],
        vec![base.clone()],
        vec![],
    );

    // Wrapper: calls the inner helper twice. Not a leaf.
    let mut wrapper_temps = vec![base.clone()];
    for i in 1..3 {
        wrapper_temps.push(AmirTemp {
            id: TempId::from_usize(i),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        });
    }
    let wrapper = make_func(
        2,
        vec![(
            vec![
                AmirStmt::Call {
                    lhs: Some(TempId(1)),
                    callee: AmirOperand::FunctionRef(SymbolId::new(0, 1)),
                    args: smallvec::smallvec![],
                    return_borrow: None,
                },
                AmirStmt::Call {
                    lhs: Some(TempId(2)),
                    callee: AmirOperand::FunctionRef(SymbolId::new(0, 1)),
                    args: smallvec::smallvec![],
                    return_borrow: None,
                },
            ],
            AmirTerminator::Return,
        )],
        wrapper_temps,
        vec![],
    );

    // The leaf-only evaluator must still reject it.
    assert_eq!(cost::evaluate_leaf_inlining(&wrapper, 25), None);
    // The non-leaf evaluator must accept it, with the calls charged as cost.
    let cost = cost::evaluate_nonleaf_inlining(&wrapper, 25)
        .expect("a two-call wrapper is a valid inline candidate");
    assert!(
        cost >= 4,
        "nested calls must weigh on the budget, got {cost}"
    );

    let mut caller_temps = vec![base.clone()];
    for i in 1..4 {
        caller_temps.push(AmirTemp {
            id: TempId::from_usize(i),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        });
    }
    let caller = make_func(
        3,
        vec![(
            vec![AmirStmt::Call {
                lhs: Some(TempId(1)),
                callee: AmirOperand::FunctionRef(SymbolId::new(0, 2)),
                args: smallvec::smallvec![],
                return_borrow: None,
            }],
            AmirTerminator::Return,
        )],
        caller_temps,
        vec![],
    );

    let mut program = AmirProgram {
        funcs: vec![caller, wrapper, inner],
        literal_pool: pool,
        extern_funcs: FxHashMap::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    assert!(inline_leaf_functions(&mut program) > 0);
    // Nothing may remain: the wrapper and its two inner calls all fold away.
    for func in &program.funcs {
        for block in &func.blocks {
            assert!(
                func.block_stmts(block.id)
                    .all(|stmt| !matches!(stmt, AmirStmt::Call { .. })),
                "wrapper and nested calls must all inline away"
            );
        }
    }
}

/// A wrapper around more calls than [`MAX_NONLEAF_CALLS`] stays rejected, so a
/// large dispatcher is never pasted into every caller.
#[test]
fn wide_wrapper_is_not_inlinable() {
    let ty = intern_int();
    let base = AmirTemp {
        id: TempId(0),
        ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };
    let calls = cost::MAX_NONLEAF_CALLS + 1;
    let mut temps = vec![base.clone()];
    for i in 1..=calls {
        temps.push(AmirTemp {
            id: TempId::from_usize(i),
            ty,
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        });
    }
    let stmts: Vec<AmirStmt> = (0..calls)
        .map(|i| AmirStmt::Call {
            lhs: Some(TempId::from_usize(i + 1)),
            callee: AmirOperand::FunctionRef(SymbolId::new(0, 99)),
            args: smallvec::smallvec![],
            return_borrow: None,
        })
        .collect();
    let wide = make_func(1, vec![(stmts, AmirTerminator::Return)], temps, vec![]);

    assert_eq!(cost::evaluate_nonleaf_inlining(&wide, 1000), None);
}

#[test]
fn test_leaf_cost_evaluation() {
    let int_ty = intern_int();
    let temp0 = AmirTemp {
        id: TempId(0),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
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
        span: crate::Span::new(0, 0, 0),
    };
    let callee_temp1 = AmirTemp {
        id: TempId(1),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
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
        span: crate::Span::new(0, 0, 0),
    };
    let caller_temp1 = AmirTemp {
        id: TempId(1),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
    };
    let caller_temp2 = AmirTemp {
        id: TempId(2),
        ty: int_ty,
        is_copy: true,
        is_nullable: false,
        span: crate::Span::new(0, 0, 0),
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
        .run_program_with_types(&mut program, &TypeInterner::new())
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
