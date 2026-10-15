#![allow(clippy::expect_used)]

use super::*;
use arandu_middle::amir::*;
use arandu_middle::ctfe::ConstAggregate;
use arandu_middle::layout::{DataLayout, DenseRange};
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
use arandu_middle::{Span, SymbolId};
use std::sync::Arc;

struct Units(Vec<Arc<CtfeFunction>>);
impl FunctionProvider for Units {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        self.0
            .iter()
            .find(|unit| unit.function().symbol == symbol)
            .map(Arc::clone)
            .ok_or(EvalErrorKind::MissingFunction(symbol))
    }
}

fn budget() -> Budget {
    Budget {
        fuel: 10_000,
        frames: 16,
        values: 128,
    }
}

fn function(symbol: SymbolId, temp_count: usize) -> (AmirFunc, TypeInterner, AmirLiteralPool) {
    let types = TypeInterner::new();
    let int = types.intern(ArType::Primitive(Primitive::Int));
    let function = AmirFunc {
        symbol,
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: (0..temp_count)
            .map(|index| AmirTemp {
                id: TempId::from_usize(index),
                ty: int,
                is_copy: true,
                is_nullable: false,
                span: Span::new(symbol.file_id, 1, 2),
            })
            .collect(),
        blocks: vec![block(0)],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg: Default::default(),
    };
    (function, types, AmirLiteralPool::default())
}

fn block(index: usize) -> AmirBasicBlock {
    AmirBasicBlock {
        id: BlockId::from_usize(index),
        params: DenseRange::empty(),
        statements: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }
}

fn unit(function: AmirFunc, types: &TypeInterner, pool: AmirLiteralPool) -> Arc<CtfeFunction> {
    Arc::new(
        CtfeFunction::new(function, pool, types, DataLayout::ptr_width(8)).expect("scalar unit"),
    )
}

fn value(value: i128) -> ConstValue {
    ConstValue::Integer(
        ConstInt::new(
            IntegerType::new(Primitive::Int, DataLayout::ptr_width(8)).expect("int"),
            value,
        )
        .expect("integer"),
    )
}

#[test]
fn calls_share_exact_fuel_and_recover_frame_slots_after_return() {
    let root = SymbolId::new(1, 0);
    let child = SymbolId::new(1, 1);
    let (mut a, types, pool) = function(root, 1);
    a.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Call {
            lhs: Some(TempId(0)),
            callee: AmirOperand::FunctionRef(child),
            args: Default::default(),
            return_borrow: None,
        },
    );
    let (mut b, child_types, mut literals) = function(child, 1);
    let constant = AmirOperand::Constant(AmirConstant::Pool(literals.intern_int("42")));
    b.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Use(constant),
        },
    );
    let units = Units(vec![unit(a, &types, pool), unit(b, &child_types, literals)]);
    let mut exact = budget();
    exact.fuel = 20;
    exact.frames = 2;
    exact.values = 4; // Two admitted handles plus two live frame slots.
    assert_eq!(evaluate(&units, root, &[], exact, || false), Ok(value(42)));
    exact.fuel = 19;
    assert_eq!(
        evaluate(&units, root, &[], exact, || false)
            .expect_err("one step short")
            .kind,
        EvalErrorKind::FuelExhausted
    );
    exact = budget();
    exact.frames = 1;
    assert_eq!(
        evaluate(&units, root, &[], exact, || false)
            .expect_err("frame bound")
            .kind,
        EvalErrorKind::FrameLimit
    );
    exact = budget();
    exact.values = 3;
    assert_eq!(
        evaluate(&units, root, &[], exact, || false)
            .expect_err("combined slots")
            .kind,
        EvalErrorKind::ValueLimit
    );
    // Sequential calls reuse the same two handles and two frame slots.
    let (mut a, types, pool) = function(root, 1);
    for lhs in [None, Some(TempId(0))] {
        a.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Call {
                lhs,
                callee: AmirOperand::FunctionRef(child),
                args: Default::default(),
                return_borrow: None,
            },
        );
    }
    let units = Units(vec![unit(a, &types, pool), Arc::clone(&units.0[1])]);
    exact = budget();
    exact.frames = 2;
    exact.values = 4;
    assert_eq!(evaluate(&units, root, &[], exact, || false), Ok(value(42)));
}

#[test]
fn backedge_arguments_are_read_simultaneously_and_scratch_counts_toward_budget() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut literals) = function(root, 8);
    let int = f.return_type;
    f.temps[7].ty = types.intern(ArType::Primitive(Primitive::Bool));
    f.params = vec![TempId(3), TempId(4)];
    f.blocks.extend([block(1), block(2)]);
    f.block_params = [1, 2, 5]
        .into_iter()
        .map(|index| BlockParam {
            id: TempId(index),
            local: LocalId(index),
            ty: int,
            from: None,
            moved: false,
        })
        .collect();
    f.blocks[1].params = DenseRange::new(0, 3);
    let one = AmirOperand::Constant(AmirConstant::Pool(literals.intern_int("1")));
    let zero = AmirOperand::Constant(AmirConstant::Pool(literals.intern_int("0")));
    f.blocks[0].terminator = AmirTerminator::Goto {
        target: BlockId(1),
        args: vec![
            AmirOperand::Copy(TempId(3)),
            AmirOperand::Copy(TempId(4)),
            one,
        ],
    };
    f.append_stmt_to_block(
        BlockId(1),
        AmirStmt::Assign {
            lhs: TempId(6),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Sub,
                left: AmirOperand::Copy(TempId(5)),
                right: one,
            },
        },
    );
    f.append_stmt_to_block(
        BlockId(1),
        AmirStmt::Assign {
            lhs: TempId(7),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Gt,
                left: AmirOperand::Copy(TempId(5)),
                right: zero,
            },
        },
    );
    f.blocks[1].terminator = AmirTerminator::Branch {
        condition: AmirOperand::Copy(TempId(7)),
        if_true: BlockId(1),
        true_args: vec![
            AmirOperand::Copy(TempId(2)),
            AmirOperand::Copy(TempId(1)),
            AmirOperand::Copy(TempId(6)),
        ],
        if_false: BlockId(2),
        false_args: vec![],
    };
    f.append_stmt_to_block(
        BlockId(2),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Sub,
                left: AmirOperand::Copy(TempId(1)),
                right: AmirOperand::Copy(TempId(2)),
            },
        },
    );
    let units = Units(vec![unit(f, &types, literals)]);
    let mut limited = budget();
    limited.values = 12;
    assert_eq!(
        evaluate(&units, root, &[value(10), value(20)], limited, || false),
        Ok(value(10))
    );
    limited.values = 11;
    assert_eq!(
        evaluate(&units, root, &[value(10), value(20)], limited, || false)
            .expect_err("edge scratch")
            .kind,
        EvalErrorKind::ValueLimit
    );
}

#[test]
fn malformed_dense_ids_and_missing_values_return_errors_without_panics() {
    let root = SymbolId::new(1, 0);
    let (f, types, pool) = function(root, 1);
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        )
        .expect_err("return register uninitialized")
        .kind,
        EvalErrorKind::Uninitialized
    );
    let (mut f, types, pool) = function(root, 1);
    f.blocks[0].statements = DenseRange {
        start: u32::MAX,
        len: u32::MAX,
    };
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        )
        .expect_err("invalid statement range")
        .kind,
        EvalErrorKind::InvalidIr
    );
    let (mut f, types, pool) = function(root, 1);
    f.blocks[0].terminator = AmirTerminator::Goto {
        target: BlockId(u32::MAX),
        args: vec![],
    };
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        )
        .expect_err("invalid block")
        .kind,
        EvalErrorKind::InvalidIr
    );
    let (mut f, types, pool) = function(root, 1);
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(u32::MAX),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId(u32::MAX))),
        },
    );
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        )
        .expect_err("invalid temp")
        .kind,
        EvalErrorKind::InvalidIr
    );
}

#[test]
fn forbidden_effect_in_an_unentered_block_is_rejected_before_execution() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut pool) = function(root, 1);
    let forty_two = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Use(forty_two),
        },
    );
    f.blocks.push(block(1));
    f.append_stmt_to_block(BlockId(1), AmirStmt::Free(forty_two));
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        )
        .expect_err("unreachable effect")
        .kind,
        EvalErrorKind::UnsupportedOperation
    );
}

#[test]
fn untaken_calls_are_inspected_transitively_with_source_order_trace() {
    let root = SymbolId::new(1, 0);
    let child = SymbolId::new(2, 0);
    let leaf = SymbolId::new(3, 0);
    let mut units = Vec::new();
    for (symbol, callee) in [(root, Some(child)), (child, Some(leaf)), (leaf, None)] {
        let (mut f, types, mut pool) = function(symbol, 1);
        f.temps[0].span = Span::new(symbol.file_id, 20, 25);
        let constant = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(constant),
            },
        );
        f.blocks.push(block(1)); // Not a successor of the entry block.
        if let Some(callee) = callee {
            f.append_stmt_to_block(
                BlockId(1),
                AmirStmt::Call {
                    lhs: Some(TempId(0)),
                    callee: AmirOperand::FunctionRef(callee),
                    args: Default::default(),
                    return_borrow: None,
                },
            );
        } else {
            f.append_stmt_to_block(BlockId(1), AmirStmt::Free(constant));
        }
        units.push(unit(f, &types, pool));
    }
    let error =
        evaluate(&Units(units), root, &[], budget(), || false).expect_err("transitive effect");
    assert_eq!(error.kind, EvalErrorKind::UnsupportedOperation);
    assert_eq!(error.function, leaf);
    assert_eq!(error.block, BlockId(1));
    assert_eq!(
        error
            .trace
            .iter()
            .map(|site| site.function)
            .collect::<Vec<_>>(),
        vec![root, child]
    );
    assert!(
        error
            .trace
            .iter()
            .all(|site| site.block == BlockId(1) && site.span.start == 20)
    );
    assert!(!error.trace_truncated);
}

#[test]
fn global_reads_are_forbidden_even_in_untaken_blocks() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, pool) = function(root, 2);
    f.temps[1].span = Span::new(1, 30, 40);
    f.blocks.push(block(1));
    f.append_stmt_to_block(
        BlockId(1),
        AmirStmt::Assign {
            lhs: TempId(1),
            rhs: AmirRvalue::Use(AmirOperand::GlobalRef(SymbolId::new(1, 99))),
        },
    );
    let error = evaluate(
        &Units(vec![unit(f, &types, pool)]),
        root,
        &[],
        budget(),
        || false,
    )
    .expect_err("global effect");
    assert_eq!(error.kind, EvalErrorKind::UnsupportedOperation);
    assert_eq!(error.span, Span::new(1, 30, 40));
}

#[test]
fn admission_charges_large_calls_and_untaken_transfer_tables() {
    let root = SymbolId::new(1, 0);
    let child = SymbolId::new(1, 1);
    for transfer in 0..3 {
        let (mut f, types, mut pool) = function(root, 1);
        let constant = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(constant),
            },
        );
        f.blocks.push(block(1));
        match transfer {
            0 => {
                f.append_stmt_to_block(
                    BlockId(1),
                    AmirStmt::Call {
                        lhs: Some(TempId(0)),
                        callee: AmirOperand::FunctionRef(child),
                        args: vec![constant; 10_000].into(),
                        return_borrow: None,
                    },
                );
            }
            1 => {
                f.blocks[1].terminator = AmirTerminator::Goto {
                    target: BlockId(0),
                    args: vec![constant; 10_000],
                };
            }
            2 => {
                f.blocks[1].terminator = AmirTerminator::SwitchInt {
                    discriminant: constant,
                    targets: (0..10_000).map(|tag| (tag, BlockId(0), vec![])).collect(),
                    otherwise: (BlockId(0), vec![]),
                };
            }
            _ => unreachable!("test cases are bounded"),
        }
        let mut limited = budget();
        limited.fuel = 32;
        let mut polls = 0;
        let error = evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            limited,
            || {
                polls += 1;
                false
            },
        )
        .expect_err("admission bound");
        assert_eq!(
            error.kind,
            EvalErrorKind::FuelExhausted,
            "transfer {transfer}"
        );
        assert!(polls <= 34, "admission work must be proportional to fuel");
    }
}

#[test]
fn expression_root_does_not_shadow_its_source_owner() {
    let owner = SymbolId::new(1, 0);
    let (mut expression, types, pool) = function(owner, 1);
    expression.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Call {
            lhs: Some(TempId(0)),
            callee: AmirOperand::FunctionRef(owner),
            args: Default::default(),
            return_borrow: None,
        },
    );
    let expression = unit(expression, &types, pool);
    let (mut definition, types, mut pool) = function(owner, 1);
    let constant = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
    definition.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Use(constant),
        },
    );
    let units = Units(vec![unit(definition, &types, pool)]);
    assert_eq!(
        evaluate_unit(&units, expression, &[], budget(), || false),
        Ok(value(42))
    );
}

#[test]
fn recursive_admission_deduplicates_units_and_bounds_runtime_trace() {
    use std::cell::Cell;
    struct Counted {
        units: Units,
        lookups: Cell<usize>,
    }
    impl FunctionProvider for Counted {
        fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
            self.lookups.set(self.lookups.get() + 1);
            self.units.function(symbol)
        }
    }
    let root = SymbolId::new(1, 0);
    let (mut f, types, pool) = function(root, 1);
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Call {
            lhs: Some(TempId(0)),
            callee: AmirOperand::FunctionRef(root),
            args: Default::default(),
            return_borrow: None,
        },
    );
    let provider = Counted {
        units: Units(vec![unit(f, &types, pool)]),
        lookups: Cell::new(0),
    };
    let mut policy = budget();
    policy.frames = 64;
    let error = evaluate(&provider, root, &[], policy, || false).expect_err("bounded recursion");
    assert_eq!(error.kind, EvalErrorKind::FrameLimit);
    assert_eq!(provider.lookups.get(), 1);
    assert_eq!(error.trace.len(), 32);
    assert!(error.trace_truncated);
}

#[test]
fn arithmetic_failure_points_to_the_operation_and_retains_callers() {
    let root = SymbolId::new(1, 0);
    let child = SymbolId::new(2, 0);
    let (mut f, types, pool) = function(root, 1);
    f.temps[0].span = Span::new(1, 10, 15);
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Call {
            lhs: Some(TempId(0)),
            callee: AmirOperand::FunctionRef(child),
            args: Default::default(),
            return_borrow: None,
        },
    );
    let (mut g, child_types, mut literals) = function(child, 2);
    g.temps[1].span = Span::new(2, 30, 35);
    let one = AmirOperand::Constant(AmirConstant::Pool(literals.intern_int("1")));
    let zero = AmirOperand::Constant(AmirConstant::Pool(literals.intern_int("0")));
    g.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(1),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Div,
                left: one,
                right: zero,
            },
        },
    );
    let units = Units(vec![unit(f, &types, pool), unit(g, &child_types, literals)]);
    let error = evaluate(&units, root, &[], budget(), || false).expect_err("division by zero");
    assert_eq!(
        error.kind,
        EvalErrorKind::Arithmetic(ScalarEvalError::DivisionByZero)
    );
    assert_eq!(error.span, Span::new(2, 30, 35));
    assert_eq!(error.trace.len(), 1);
    assert_eq!(error.trace[0].span, Span::new(1, 10, 15));
}

#[test]
fn checked_arithmetic_failure_retains_the_function_and_span() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut pool) = function(root, 2);
    f.params = vec![TempId(1)];
    let one = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("1")));
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Div,
                left: one,
                right: AmirOperand::Copy(TempId(1)),
            },
        },
    );
    let error = evaluate(
        &Units(vec![unit(f, &types, pool)]),
        root,
        &[value(0)],
        budget(),
        || false,
    )
    .expect_err("division by zero");
    assert_eq!(
        error.kind,
        EvalErrorKind::Arithmetic(ScalarEvalError::DivisionByZero)
    );
    assert_eq!(error.function, root);
    assert_eq!(error.span, Span::new(1, 1, 2));
}

#[test]
fn scalar_locals_are_initialized_and_cleared_by_storage_boundaries() {
    let root = SymbolId::new(1, 0);
    for boundary in [
        None,
        Some(AmirStmt::StorageDead(LocalId(0))),
        Some(AmirStmt::StorageLive(LocalId(0))),
    ] {
        let (mut f, types, mut pool) = function(root, 1);
        f.locals.push(AmirLocal {
            id: LocalId(0),
            ty: f.return_type,
            is_memory: false,
            symbol: None,
            span: Span::new(1, 1, 2),
            use_span: None,
        });
        let place = AmirPlace {
            local: LocalId(0),
            projections: Default::default(),
        };
        f.append_stmt_to_block(BlockId(0), AmirStmt::StorageLive(LocalId(0)));
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Store {
                lhs: place.clone(),
                rhs: AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42"))),
            },
        );
        let cleared = boundary.is_some();
        if let Some(boundary) = boundary {
            f.append_stmt_to_block(BlockId(0), boundary);
        }
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Load(place),
            },
        );
        let result = evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false,
        );
        if cleared {
            assert_eq!(
                result.expect_err("storage boundary clears the value").kind,
                EvalErrorKind::Uninitialized
            );
        } else {
            assert_eq!(result, Ok(value(42)));
        }
    }
}

#[test]
fn switch_selects_only_the_matching_edge_or_otherwise() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, pool) = function(root, 2);
    f.params = vec![TempId(1)];
    f.blocks.extend([block(1), block(2)]);
    f.block_params.push(BlockParam {
        id: TempId(0),
        local: LocalId(0),
        ty: f.return_type,
        from: None,
        moved: false,
    });
    for index in [1, 2] {
        f.blocks[index].params = DenseRange::new(0, 1);
    }
    f.blocks[0].terminator = AmirTerminator::SwitchInt {
        discriminant: AmirOperand::Copy(TempId(1)),
        targets: vec![(7, BlockId(1), vec![AmirOperand::Copy(TempId(1))])],
        // This argument is uninitialized and must not be read on the first edge.
        otherwise: (BlockId(2), vec![AmirOperand::Copy(TempId(0))]),
    };
    let units = Units(vec![unit(f, &types, pool)]);
    assert_eq!(
        evaluate(&units, root, &[value(7)], budget(), || false),
        Ok(value(7))
    );
    assert_eq!(
        evaluate(&units, root, &[value(8)], budget(), || false)
            .expect_err("otherwise argument")
            .kind,
        EvalErrorKind::Uninitialized
    );
}

#[test]
fn bool_void_and_target_mismatch_are_handled_without_host_assumptions() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, pool) = function(root, 1);
    let boolean = types.intern(ArType::Primitive(Primitive::Bool));
    f.return_type = boolean;
    f.temps[0].ty = boolean;
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        },
    );
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        ),
        Ok(ConstValue::Bool(true))
    );
    let (mut f, types, pool) = function(root, 0);
    f.return_type = types.intern(ArType::Void);
    assert_eq!(
        evaluate(
            &Units(vec![unit(f, &types, pool)]),
            root,
            &[],
            budget(),
            || false
        ),
        Ok(ConstValue::Void)
    );

    let child = SymbolId::new(1, 1);
    let (mut f, types, pool) = function(root, 1);
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Call {
            lhs: Some(TempId(0)),
            callee: AmirOperand::FunctionRef(child),
            args: Default::default(),
            return_borrow: None,
        },
    );
    let (g, child_types, child_pool) = function(child, 1);
    let child = Arc::new(
        CtfeFunction::new(g, child_pool, &child_types, DataLayout::ptr_width(4))
            .expect("ptr4 unit"),
    );
    let units = Units(vec![unit(f, &types, pool), child]);
    assert_eq!(
        evaluate(&units, root, &[], budget(), || false)
            .expect_err("different callee target")
            .kind,
        EvalErrorKind::TargetMismatch
    );
}

#[test]
fn repeated_literal_decoding_is_charged_and_can_be_cancelled_mid_spelling() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut pool) = function(root, 1);
    let spelling = "0000000000000042";
    let constant = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int(spelling)));
    for _ in 0..2 {
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(constant),
            },
        );
    }
    let units = Units(vec![unit(f, &types, pool)]);
    let mut exact = budget();
    // Root lookup, block/statements/temp admission, two instructions, return,
    // and three scans of the spelling (admission plus each decoding).
    exact.fuel = 11 + 3 * spelling.len() as u64;
    assert_eq!(evaluate(&units, root, &[], exact, || false), Ok(value(42)));
    exact.fuel -= 1;
    assert_eq!(
        evaluate(&units, root, &[], exact, || false)
            .expect_err("one step short")
            .kind,
        EvalErrorKind::FuelExhausted
    );
    let mut polls = 0;
    assert_eq!(
        evaluate(&units, root, &[], budget(), || {
            polls += 1;
            polls == 5 + spelling.len() + 5
        })
        .expect_err("cancel during first decoding")
        .kind,
        EvalErrorKind::Cancelled
    );
    assert!(polls > spelling.len());
}

#[test]
fn array_calls_keep_their_own_types_and_project_checked_values() {
    let root = SymbolId::new(1, 0);
    let child = SymbolId::new(2, 0);
    let (mut caller, types, pool) = function(root, 2);
    let array = types.intern(ArType::Array(2, caller.return_type));
    caller.temps[1].ty = array;
    caller.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Call {
            lhs: Some(TempId(1)),
            callee: AmirOperand::FunctionRef(child),
            args: Default::default(),
            return_borrow: None,
        },
    );
    caller.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::IndexAccess {
                base: AmirOperand::Copy(TempId(1)),
                index: AmirOperand::Constant(AmirConstant::Pool(
                    arandu_middle::literal_pool::LiteralId(0),
                )),
            },
        },
    );
    let mut pool = pool;
    pool.intern_int("1");
    let (mut callee, child_types, mut child_pool) = function(child, 1);
    let int = callee.return_type;
    // Give the same array a different interner index than the caller's.
    child_types.intern(ArType::Primitive(Primitive::Bool));
    callee.return_type = child_types.intern(ArType::Array(2, int));
    callee.temps[0].ty = callee.return_type;
    let twenty = AmirOperand::Constant(AmirConstant::Pool(child_pool.intern_int("20")));
    let answer = AmirOperand::Constant(AmirConstant::Pool(child_pool.intern_int("42")));
    callee.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Array {
                items: vec![twenty, answer],
            },
        },
    );
    let units = Units(vec![
        unit(caller, &types, pool),
        unit(callee, &child_types, child_pool),
    ]);
    assert_eq!(
        evaluate(&units, root, &[], budget(), || false),
        Ok(value(42))
    );
    let mut limited = budget();
    limited.values = 6; // five live slots/handles plus two construction slots.
    assert_eq!(
        evaluate(&units, root, &[], limited, || false)
            .expect_err("aggregate scratch")
            .kind,
        EvalErrorKind::ValueLimit
    );
    let mut polls = 0;
    assert_eq!(
        evaluate(&units, root, &[], budget(), || {
            polls += 1;
            polls == 28
        })
        .expect_err("cancel aggregate construction/admission")
        .kind,
        EvalErrorKind::Cancelled
    );
}

#[test]
fn frozen_strings_bytes_and_subviews_keep_unicode_nul_and_bounds() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut pool) = function(root, 4);
    let byte = types.intern(ArType::Primitive(Primitive::Byte));
    let str_ty = types.intern(ArType::Primitive(Primitive::Str));
    let bytes = types.intern(ArType::Slice(byte));
    f.return_type = bytes;
    f.temps[0].ty = bytes;
    f.temps[1].ty = str_ty;
    f.temps[2].ty = bytes;
    f.temps[3].ty = types.intern(ArType::Primitive(Primitive::USize));
    let literal = AmirOperand::Constant(AmirConstant::Pool(pool.intern_str("Olá\0🦀")));
    let start = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("2")));
    let len = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("3")));
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(1),
            rhs: AmirRvalue::Use(literal),
        },
    );
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(2),
            rhs: AmirRvalue::StrBytes {
                source: AmirOperand::Copy(TempId(1)),
            },
        },
    );
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::SliceSubslice {
                slice: AmirOperand::Copy(TempId(2)),
                start,
                len,
            },
        },
    );
    let units = Units(vec![unit(f, &types, pool)]);
    let expected = arandu_middle::ctfe::ConstString::new("Olá\0🦀")
        .bytes()
        .view(2, 3)
        .expect("view");
    assert_eq!(
        evaluate(&units, root, &[], budget(), || false),
        Ok(ConstValue::Bytes(expected))
    );
}

#[test]
fn string_backing_bytes_are_bounded_independently_of_slots_and_fuel() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut pool) = function(root, 1);
    f.return_type = types.intern(ArType::Primitive(Primitive::Str));
    f.temps[0].ty = f.return_type;
    let text = "x".repeat(4096);
    let literal = AmirOperand::Constant(AmirConstant::Pool(pool.intern_str(text)));
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Use(literal),
        },
    );
    let units = Units(vec![unit(f, &types, pool)]);
    let mut limited = budget();
    limited.fuel = 20_000;
    limited.values = 2; // one admitted handle and one temp, no room for backing.
    assert_eq!(
        evaluate(&units, root, &[], limited, || false)
            .expect_err("byte ceiling")
            .kind,
        EvalErrorKind::ValueLimit
    );
    limited.values = 128;
    assert!(evaluate(&units, root, &[], limited, || false).is_ok());
}

#[test]
fn repeated_string_backing_allocations_share_the_cumulative_byte_ceiling() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, mut pool) = function(root, 1);
    f.return_type = types.intern(ArType::Primitive(Primitive::Str));
    f.temps[0].ty = f.return_type;
    let literal = AmirOperand::Constant(AmirConstant::Pool(pool.intern_str("x".repeat(128))));
    for _ in 0..5 {
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(literal),
            },
        );
    }
    let units = Units(vec![unit(f, &types, pool)]);
    let mut limited = budget();
    limited.fuel = 20_000;
    limited.values = 4;
    assert_eq!(
        evaluate(&units, root, &[], limited, || false)
            .expect_err("redecoding cannot reset the shared byte ceiling")
            .kind,
        EvalErrorKind::ValueLimit
    );
    limited.values = 8;
    assert!(evaluate(&units, root, &[], limited, || false).is_ok());
}

#[test]
fn nominal_products_require_copy_proof_no_destructor_and_safe_fields() {
    use arandu_middle::layout::{
        EnumPayloadShape, StructFieldInfo, StructFields, StructLayoutProvider,
    };
    struct Pod {
        fields: StructFields,
        copy: Option<bool>,
        destructor: Option<SymbolId>,
    }
    impl StructLayoutProvider for Pod {
        fn get_struct_fields(&self, _: SymbolId) -> Option<&StructFields> {
            Some(&self.fields)
        }
        fn get_generic_params(&self, _: SymbolId) -> Option<&[SymbolId]> {
            Some(&[])
        }
        fn get_enum_variants(&self, _: SymbolId) -> Option<Vec<EnumPayloadShape>> {
            None
        }
        fn is_copy_type(&self, _: arandu_middle::types::TypeId) -> Option<bool> {
            self.copy
        }
        fn destructor_for_type(&self, _: arandu_middle::types::TypeId) -> Option<SymbolId> {
            self.destructor
        }
    }
    let root = SymbolId::new(1, 0);
    let nominal = SymbolId::new(2, 1);
    let (mut f, types, mut pool) = function(root, 1);
    let int = f.return_type;
    let named = types.intern(ArType::named(nominal, &[], &types));
    f.return_type = named;
    f.temps[0].ty = named;
    let literal = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: nominal,
                fields: vec![("answer".into(), literal)],
            },
        },
    );
    let mut provider = Pod {
        fields: StructFields::from_entries([StructFieldInfo {
            name: "answer".into(),
            symbol: Some(SymbolId::new(2, 2)),
            ty: int,
            index: 0,
        }]),
        copy: None,
        destructor: None,
    };
    assert!(matches!(
        CtfeFunction::new_with_provider(
            f.clone(),
            pool.clone(),
            &types,
            DataLayout::ptr_width(8),
            &provider
        ),
        Err(EvalErrorKind::UnsupportedType(_))
    ));
    provider.copy = Some(false);
    assert!(matches!(
        CtfeFunction::new_with_provider(
            f.clone(),
            pool.clone(),
            &types,
            DataLayout::ptr_width(8),
            &provider
        ),
        Err(EvalErrorKind::UnsupportedType(_))
    ));
    provider.copy = Some(true);
    let unit = Arc::new(
        CtfeFunction::new_with_provider(
            f.clone(),
            pool.clone(),
            &types,
            DataLayout::ptr_width(8),
            &provider,
        )
        .expect("proven POD"),
    );
    let actual =
        evaluate(&Units(vec![unit]), root, &[], budget(), || false).expect("closed product");
    let expected = ConstValue::Aggregate(
        arandu_middle::ctfe::ConstAggregate::new(
            arandu_middle::types::TypeShape::Named(nominal, vec![]),
            vec![value(42)],
        )
        .expect("product"),
    );
    assert_eq!(actual, expected);
    provider.destructor = Some(SymbolId::new(2, 3));
    assert!(matches!(
        CtfeFunction::new_with_provider(
            f.clone(),
            pool.clone(),
            &types,
            DataLayout::ptr_width(8),
            &provider
        ),
        Err(EvalErrorKind::UnsupportedType(_))
    ));
    provider.destructor = None;
    provider.fields.fields[0].ty = types.intern(ArType::Ref(int));
    assert!(matches!(
        CtfeFunction::new_with_provider(f, pool, &types, DataLayout::ptr_width(8), &provider),
        Err(EvalErrorKind::UnsupportedType(_))
    ));
}

#[test]
fn nominal_enums_validate_every_variant_even_when_the_active_variant_is_unit() {
    use arandu_middle::layout::{EnumPayloadShape, StructFields, StructLayoutProvider};
    struct Metadata {
        variants: Vec<EnumPayloadShape>,
        symbols: Vec<Option<SymbolId>>,
        copy: Option<bool>,
        destructor: Option<SymbolId>,
    }
    impl StructLayoutProvider for Metadata {
        fn get_struct_fields(&self, _: SymbolId) -> Option<&StructFields> {
            None
        }
        fn get_generic_params(&self, _: SymbolId) -> Option<&[SymbolId]> {
            Some(&[])
        }
        fn get_enum_variants(&self, _: SymbolId) -> Option<Vec<EnumPayloadShape>> {
            Some(self.variants.clone())
        }
        fn get_enum_variant_symbol(&self, _: SymbolId, tag: usize) -> Option<SymbolId> {
            self.symbols.get(tag).copied().flatten()
        }
        fn is_copy_type(&self, _: TypeId) -> Option<bool> {
            self.copy
        }
        fn destructor_for_type(&self, _: TypeId) -> Option<SymbolId> {
            self.destructor
        }
    }
    let root = SymbolId::new(1, 0);
    let nominal = SymbolId::new(2, 0);
    let (mut f, types, pool) = function(root, 1);
    let int = f.return_type;
    let named = types.intern(ArType::named(nominal, &[], &types));
    f.return_type = named;
    f.temps[0].ty = named;
    f.append_stmt_to_block(
        BlockId(0),
        AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::EnumConstruct {
                variant_tag: 0,
                payload: None,
            },
        },
    );
    let mut metadata = Metadata {
        variants: vec![
            EnumPayloadShape { payload_ty: None },
            EnumPayloadShape {
                payload_ty: Some(int),
            },
        ],
        symbols: vec![Some(SymbolId::new(2, 1)), Some(SymbolId::new(2, 2))],
        copy: Some(true),
        destructor: None,
    };
    let build = |metadata: &Metadata| {
        CtfeFunction::new_with_provider(
            f.clone(),
            pool.clone(),
            &types,
            DataLayout::ptr_width(8),
            metadata,
        )
    };
    let unit = Arc::new(build(&metadata).expect("complete nominal metadata"));
    let actual = evaluate(&Units(vec![unit]), root, &[], budget(), || false).expect("unit variant");
    assert_eq!(
        actual,
        ConstValue::Aggregate(
            ConstAggregate::enumeration(
                arandu_middle::types::TypeShape::Named(nominal, Vec::new()),
                arandu_middle::ctfe::ConstVariant {
                    tag: 0,
                    symbol: metadata.symbols[0]
                },
                None,
            )
            .expect("nominal unit variant")
        )
    );
    metadata.symbols[1] = None;
    assert!(matches!(
        build(&metadata),
        Err(EvalErrorKind::UnsupportedType(_))
    ));
    metadata.symbols[1] = Some(SymbolId::new(2, 2));
    for copy in [None, Some(false)] {
        metadata.copy = copy;
        assert!(matches!(
            build(&metadata),
            Err(EvalErrorKind::UnsupportedType(_))
        ));
    }
    metadata.copy = Some(true);
    metadata.destructor = Some(SymbolId::new(2, 3));
    assert!(matches!(
        build(&metadata),
        Err(EvalErrorKind::UnsupportedType(_))
    ));
    metadata.destructor = None;
    for resource in [
        ArType::Ref(int),
        ArType::Ptr(int),
        ArType::ConstParam(SymbolId::new(2, 4)),
    ] {
        metadata.variants[1].payload_ty = Some(types.intern(resource));
        assert!(matches!(
            build(&metadata),
            Err(EvalErrorKind::UnsupportedType(_))
        ));
    }
}

#[test]
fn enum_construction_and_projection_reject_malformed_amir() {
    let root = SymbolId::new(1, 0);
    for (tag, payload, expected) in [
        (2, None, EvalErrorKind::InvalidIr),
        (
            0,
            Some(AmirOperand::Constant(AmirConstant::Bool(false))),
            EvalErrorKind::TypeMismatch,
        ),
        (1, None, EvalErrorKind::TypeMismatch),
        (
            1,
            Some(AmirOperand::Constant(AmirConstant::Bool(false))),
            EvalErrorKind::TypeMismatch,
        ),
    ] {
        let (mut f, types, pool) = function(root, 1);
        let option = types.intern(ArType::Option(f.return_type));
        f.return_type = option;
        f.temps[0].ty = option;
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::EnumConstruct {
                    variant_tag: tag,
                    payload,
                },
            },
        );
        assert_eq!(
            evaluate(
                &Units(vec![unit(f, &types, pool)]),
                root,
                &[],
                budget(),
                || false
            )
            .expect_err("malformed construction")
            .kind,
            expected
        );
    }
    for (tag, index, field_is_bool, expected) in [
        (0, 0, false, EvalErrorKind::InvalidIr),
        (1, 1, false, EvalErrorKind::InvalidIr),
        (1, 0, true, EvalErrorKind::TypeMismatch),
    ] {
        let (mut f, types, mut pool) = function(root, 2);
        let int = f.return_type;
        f.temps[1].ty = types.intern(ArType::Option(int));
        let payload = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(1),
                rhs: AmirRvalue::EnumConstruct {
                    variant_tag: 1,
                    payload: Some(payload),
                },
            },
        );
        f.append_stmt_to_block(
            BlockId(0),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::EnumPayload {
                    value: AmirOperand::Copy(TempId(1)),
                    variant: SymbolId::new(2, 0),
                    variant_tag: tag,
                    index,
                    field_ty: if field_is_bool {
                        types.intern(ArType::Primitive(Primitive::Bool))
                    } else {
                        int
                    },
                    tuple_ty: None,
                },
            },
        );
        assert_eq!(
            evaluate(
                &Units(vec![unit(f, &types, pool)]),
                root,
                &[],
                budget(),
                || false
            )
            .expect_err("malformed projection")
            .kind,
            expected
        );
    }
}

#[test]
fn retained_aggregate_constructors_charge_each_operand_before_execution() {
    let root = SymbolId::new(1, 0);
    let (mut f, types, pool) = function(root, 2);
    f.temps[1].ty = types.intern(ArType::Array(0, f.return_type));
    f.blocks.push(block(1));
    f.append_stmt_to_block(
        BlockId(1),
        AmirStmt::Assign {
            lhs: TempId(1),
            rhs: AmirRvalue::Array {
                items: vec![AmirOperand::Constant(AmirConstant::Bool(false)); 1024],
            },
        },
    );
    let units = Units(vec![unit(f, &types, pool)]);
    let mut limited = budget();
    limited.fuel = 32;
    assert_eq!(
        evaluate(&units, root, &[], limited, || false)
            .expect_err("unentered constructor is metered")
            .kind,
        EvalErrorKind::FuelExhausted
    );
}
