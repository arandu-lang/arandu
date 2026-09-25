//! AMIR optimization entry points.
//!
//! Thin, API-stable wrappers over [`crate::pass_manager`], which owns the
//! pipeline composition (`O0`/`O1`/`O2`/`Os`), per-pass metrics and the
//! bounded fixpoint driver with its ICE guardrail.

use crate::amir::{AmirFunc, AmirProgram};
use crate::pass_manager::{OptLevel, PassManager};
use crate::types::TypeInterner;
use crate::{Diagnostic, SymbolTable};

/// Validates AMIR before and after optimization, preventing malformed IR from
/// reaching a mutating pass or backend.
pub fn optimize_amir_checked(
    program: &mut AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> Result<(), Diagnostic> {
    optimize_amir_checked_with_level(program, symbols, interner, OptLevel::O1)
}

/// Validates AMIR before and after running the selected optimization pipeline.
pub fn optimize_amir_checked_with_level(
    program: &mut AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    level: OptLevel,
) -> Result<(), Diagnostic> {
    if let Some(issue) = crate::amir_validate::validate_amir_program(program, symbols, interner)
        .into_iter()
        .next()
    {
        return Err(issue);
    }
    if level != OptLevel::O0 {
        crate::inlining::inline_leaf_functions(program);
        if let Some(issue) = crate::amir_validate::validate_amir_program(program, symbols, interner)
            .into_iter()
            .next()
        {
            return Err(issue);
        }
    }
    PassManager::for_level(level).run_program_checked(program, symbols, interner)?;
    if let Some(issue) = crate::amir_validate::validate_amir_program(program, symbols, interner)
        .into_iter()
        .next()
    {
        return Err(issue);
    }
    Ok(())
}

/// Runs the default AMIR optimization pipeline ([`OptLevel::O1`]) on all
/// functions in `program`.
pub fn optimize_amir(program: &mut AmirProgram) -> Result<(), Diagnostic> {
    optimize_amir_with_level(program, OptLevel::O1)
}

/// Runs the AMIR optimization pipeline selected by `level` on all functions in
/// `program`.
///
/// Each function is independently optimized in a fixpoint loop; see
/// [`crate::pass_manager`] for pipeline composition and metrics.
pub fn optimize_amir_with_level(
    program: &mut AmirProgram,
    level: OptLevel,
) -> Result<(), Diagnostic> {
    if level != OptLevel::O0 {
        crate::inlining::inline_leaf_functions(program);
    }
    PassManager::for_level(level).run_program(program)?;
    Ok(())
}

/// Runs the default ([`OptLevel::O1`]) per-function optimization loop until
/// convergence.
///
/// Pass order at O1+: SCCP → mark-sweep DCE → CFG simplification. Each round
/// feeds the next; the loop terminates when no pass reports any change.
pub fn optimize_amir_func(
    func: &mut AmirFunc,
    literal_pool: &mut crate::literal_pool::AmirLiteralPool,
) -> Result<(), Diagnostic> {
    PassManager::for_level(OptLevel::O1).run_function(func, literal_pool)?;
    Ok(())
}

#[cfg(test)]
mod tests {

    fn sccp(func: &mut AmirFunc, pool: &mut AmirLiteralPool) -> bool {
        let bump = bumpalo::Bump::new();
        crate::sccp::sccp(func, pool, &bump)
    }

    fn intern_ty(ty: crate::types::ArType) -> crate::types::TypeId {
        // Fresh interner per call is OK in unit tests (pre-interns primitives).
        crate::types::TypeInterner::new().intern(ty)
    }
    use super::*;
    use crate::amir::program::extend_block_range;
    use crate::amir::{
        AmirBasicBlock, AmirConstant, AmirOperand, AmirRvalue, AmirStmt, AmirStmtTable, AmirTemp,
        AmirTerminator, BlockId, TempId,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::literal_pool::AmirLiteralEntry;
    use crate::literal_pool::AmirLiteralPool;
    use crate::ops::{BinaryOp, UnaryOp};
    use crate::passes::type_checker::types::{ArType, Primitive};

    fn int_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        }
    }

    fn bool_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Bool)),
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        }
    }

    fn func(statements: Vec<AmirStmt>, temps: Vec<AmirTemp>) -> AmirFunc {
        let mut stmts = AmirStmtTable::new();
        let mut range = DenseRange::empty();
        for stmt in statements {
            let instr = stmts.push(stmt);
            extend_block_range(&mut range, instr);
        }
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: range,
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps,
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        }
    }

    #[test]
    fn folds_integer_binary_and_comparison() {
        let mut pool = AmirLiteralPool::default();
        let two = AmirConstant::Pool(pool.intern_int("2"));
        let three = AmirConstant::Pool(pool.intern_int("3"));
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Constant(two),
                        right: AmirOperand::Constant(three),
                    },
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Gt,
                        left: AmirOperand::Copy(TempId::from_usize(0)),
                        right: AmirOperand::Constant(three),
                    },
                },
            ],
            vec![int_temp(0), bool_temp(1)],
        );

        sccp(&mut f, &mut pool);

        let folded_stmt = f
            .block_stmts(BlockId::from_usize(0))
            .nth(1)
            .expect("expected folded comparison statement");
        assert!(matches!(
            folded_stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                ..
            }
        ));
    }

    #[test]
    fn fold_not_bool() {
        let mut pool = AmirLiteralPool::default();
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(0),
                rhs: AmirRvalue::Unary {
                    op: UnaryOp::Not,
                    operand: AmirOperand::Constant(AmirConstant::Bool(false)),
                },
            }],
            vec![bool_temp(0)],
        );
        sccp(&mut f, &mut pool);
        let stmt = f.block_stmts(BlockId::from_usize(0)).next().unwrap();
        assert!(matches!(
            stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                ..
            }
        ));
    }

    #[test]
    fn fold_neg_int() {
        let mut pool = AmirLiteralPool::default();
        let five = AmirConstant::Pool(pool.intern_int("5"));
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(0),
                rhs: AmirRvalue::Unary {
                    op: UnaryOp::Neg,
                    operand: AmirOperand::Constant(five),
                },
            }],
            vec![int_temp(0)],
        );
        sccp(&mut f, &mut pool);
        let stmt = f.block_stmts(BlockId::from_usize(0)).next().unwrap();
        if let AmirStmt::Assign {
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(id))),
            ..
        } = stmt
        {
            assert_eq!(
                pool.get(*id),
                &AmirLiteralEntry::Int(smol_str::SmolStr::new_static("-5"))
            );
        } else {
            panic!("expected folded neg");
        }
    }

    #[test]
    fn fold_int_mul() {
        let mut pool = AmirLiteralPool::default();
        let a = AmirConstant::Pool(pool.intern_int("7"));
        let b = AmirConstant::Pool(pool.intern_int("6"));
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(0),
                rhs: AmirRvalue::Binary {
                    op: BinaryOp::Mul,
                    left: AmirOperand::Constant(a),
                    right: AmirOperand::Constant(b),
                },
            }],
            vec![int_temp(0)],
        );
        sccp(&mut f, &mut pool);
        let stmt = f.block_stmts(BlockId::from_usize(0)).next().unwrap();
        if let AmirStmt::Assign {
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(id))),
            ..
        } = stmt
        {
            assert_eq!(
                pool.get(*id),
                &AmirLiteralEntry::Int(smol_str::SmolStr::new_static("42"))
            );
        } else {
            panic!("expected folded mul");
        }
    }

    #[test]
    fn fold_bool_and_or() {
        let mut pool = AmirLiteralPool::default();
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::And,
                        left: AmirOperand::Constant(AmirConstant::Bool(true)),
                        right: AmirOperand::Constant(AmirConstant::Bool(false)),
                    },
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Or,
                        left: AmirOperand::Constant(AmirConstant::Bool(false)),
                        right: AmirOperand::Constant(AmirConstant::Bool(true)),
                    },
                },
            ],
            vec![bool_temp(0), bool_temp(1)],
        );
        sccp(&mut f, &mut pool);
        let mut stmts = f.block_stmts(BlockId::from_usize(0));
        let s0 = stmts.next().unwrap();
        let s1 = stmts.next().unwrap();
        assert!(matches!(
            s0,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
                ..
            }
        ));
        assert!(matches!(
            s1,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                ..
            }
        ));
    }

    #[test]
    fn sccp_dead_branch_proven_by_constant_condition() {
        // Diamond: if (false) { bb1 } else { bb2 }
        // SCCP alone should turn Branch(false, bb1, bb2) → Goto(bb2)
        // without requiring DCE or SimplifyCFG.
        let mut st = AmirStmtTable::new();
        st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
        });
        st.push(AmirStmt::Store {
            lhs: crate::amir::AmirPlace {
                local: crate::amir::LocalId::from_usize(0),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
        });
        st.push(AmirStmt::Store {
            lhs: crate::amir::AmirPlace {
                local: crate::amir::LocalId::from_usize(1),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
        });
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![
                crate::amir::AmirLocal {
                    id: crate::amir::LocalId::from_usize(0),
                    ty: intern_ty(ArType::Primitive(Primitive::Bool)),
                    is_memory: false,
                    symbol: None,
                    span: arandu_lexer::Span::new(0, 0, 0),
                    use_span: None,
                },
                crate::amir::AmirLocal {
                    id: crate::amir::LocalId::from_usize(1),
                    ty: intern_ty(ArType::Primitive(Primitive::Bool)),
                    is_memory: false,
                    symbol: None,
                    span: arandu_lexer::Span::new(0, 0, 0),
                    use_span: None,
                },
            ],
            temps: vec![bool_temp(0)],
            blocks: vec![
                AmirBasicBlock {
                    id: BlockId::from_usize(0),
                    statements: DenseRange::new(0, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Branch {
                        condition: AmirOperand::Copy(TempId::from_usize(0)),
                        if_true: BlockId::from_usize(1),
                        true_args: Vec::new(),
                        if_false: BlockId::from_usize(2),
                        false_args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(1),
                    statements: DenseRange::new(1, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(2),
                    statements: DenseRange::new(2, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
            ],
            block_params: Vec::new(),

            stmts: st,
            cfg: compute_cfg_edges(&[]),
        };
        func.cfg = compute_cfg_edges(&func.blocks);

        let mut pool = AmirLiteralPool::default();
        assert!(sccp(&mut func, &mut pool));

        // The Branch should now be Goto(bb2) — the false branch.
        assert!(matches!(
            func.block(BlockId::from_usize(0)).terminator,
            AmirTerminator::Goto { target, .. } if target == BlockId::from_usize(2)
        ));
    }

    #[test]
    fn dce_removes_unused_pure_assigns_and_keeps_alloc() {
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(2),
                    rhs: AmirRvalue::Alloc(AmirOperand::Constant(AmirConstant::Bool(true))),
                },
            ],
            vec![bool_temp(0), bool_temp(1), bool_temp(2)],
        );

        optimize_amir_func(&mut f, &mut crate::literal_pool::AmirLiteralPool::default()).unwrap();

        assert_eq!(f.blocks[0].statements.len, 1);
        let stmt = f
            .block_stmts(BlockId::from_usize(0))
            .next()
            .expect("expected one statement");
        assert!(matches!(
            stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Alloc(_),
                ..
            }
        ));
    }

    #[test]
    fn dce_removes_unused_binary() {
        let mut pool = crate::literal_pool::AmirLiteralPool::default();
        let a = AmirConstant::Pool(pool.intern_int("3"));
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Binary {
                    op: BinaryOp::Add,
                    left: AmirOperand::Constant(a),
                    right: AmirOperand::Constant(a),
                },
            }],
            vec![int_temp(0), int_temp(1)],
        );
        optimize_amir_func(&mut f, &mut pool).unwrap();
        assert_eq!(f.blocks[0].statements.len, 0);
    }

    #[test]
    fn dce_keeps_call_side_effect() {
        let mut f = func(
            vec![AmirStmt::Call {
                lhs: Some(TempId::from_usize(0)),
                callee: AmirOperand::FunctionRef(crate::SymbolId::new(0, 1)),
                args: smallvec::smallvec![],
                return_borrow: None,
            }],
            vec![bool_temp(0)],
        );
        optimize_amir_func(&mut f, &mut crate::literal_pool::AmirLiteralPool::default()).unwrap();
        assert_eq!(f.blocks[0].statements.len, 1);
    }

    #[test]
    fn whole_pipeline_dead_branch_elimination() {
        // Diamond where the branch condition is constant true:
        //   bb0: t0 = Bool(true); Branch(t0, bb1, bb2)
        //   bb1: Store; Goto bb3
        //   bb2: Store; Goto bb3
        //   bb3: Return
        // After SCCP: Branch -> Goto(bb1), bb2 unreachable.
        // After SimplifyCFG: bb2 removed, bb1 merged with bb0+bb3.
        let mut st = AmirStmtTable::new();
        st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        st.push(AmirStmt::Store {
            lhs: crate::amir::AmirPlace {
                local: crate::amir::LocalId::from_usize(0),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
        });
        st.push(AmirStmt::Store {
            lhs: crate::amir::AmirPlace {
                local: crate::amir::LocalId::from_usize(1),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
        });
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![
                crate::amir::AmirLocal {
                    id: crate::amir::LocalId::from_usize(0),
                    ty: intern_ty(ArType::Primitive(Primitive::Bool)),
                    is_memory: false,
                    symbol: None,
                    span: arandu_lexer::Span::new(0, 0, 0),
                    use_span: None,
                },
                crate::amir::AmirLocal {
                    id: crate::amir::LocalId::from_usize(1),
                    ty: intern_ty(ArType::Primitive(Primitive::Bool)),
                    is_memory: false,
                    symbol: None,
                    span: arandu_lexer::Span::new(0, 0, 0),
                    use_span: None,
                },
            ],
            temps: vec![bool_temp(0)],
            blocks: vec![
                AmirBasicBlock {
                    id: BlockId::from_usize(0),
                    statements: DenseRange::new(0, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Branch {
                        condition: AmirOperand::Copy(TempId::from_usize(0)),
                        if_true: BlockId::from_usize(1),
                        true_args: Vec::new(),
                        if_false: BlockId::from_usize(2),
                        false_args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(1),
                    statements: DenseRange::new(1, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: BlockId::from_usize(3),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(2),
                    statements: DenseRange::new(2, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: BlockId::from_usize(3),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(3),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
            ],
            block_params: Vec::new(),

            stmts: st,
            cfg: compute_cfg_edges(&[]),
        };
        func.cfg = compute_cfg_edges(&func.blocks);

        optimize_amir_func(
            &mut func,
            &mut crate::literal_pool::AmirLiteralPool::default(),
        )
        .unwrap();

        // After optimization: only 1 block (merged bb0 + bb1 + bb3), with 2 stmts + Return
        assert_eq!(func.blocks.len(), 1);
        assert_eq!(func.blocks[0].statements.len, 2);
        assert!(matches!(func.blocks[0].terminator, AmirTerminator::Return));
    }

    #[test]
    fn gvn_eliminates_redundant_binary_expressions() {
        let mut f = func(
            vec![
                // _1 = add _2, _3
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Copy(TempId::from_usize(2)),
                        right: AmirOperand::Copy(TempId::from_usize(3)),
                    },
                },
                // _0 = add _3, _2 (commutative duplicate!)
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Copy(TempId::from_usize(3)),
                        right: AmirOperand::Copy(TempId::from_usize(2)),
                    },
                },
            ],
            vec![int_temp(0), int_temp(1), int_temp(2), int_temp(3)],
        );

        assert!(crate::gvn::gvn(&mut f));
        // Check that second assign was rewritten to Use(_1)
        if let AmirStmt::Assign { lhs, rhs } = f.stmt(crate::amir::InstrId::from_usize(1)) {
            assert_eq!(lhs.as_usize(), 0);
            assert!(matches!(
                rhs,
                AmirRvalue::Use(AmirOperand::Copy(t)) if t.as_usize() == 1
            ));
        } else {
            panic!("expected Assign");
        }
    }

    #[test]
    fn gvn_keeps_same_field_access_when_result_types_differ() {
        let interner = crate::types::TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let err_ty = interner.intern(ArType::Err);
        let result_ty = interner.intern(ArType::Result(int_ty, err_ty));
        let temp = |id, ty| AmirTemp {
            id: TempId::from_usize(id),
            ty,
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        };
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::FieldAccess {
                        base: AmirOperand::Copy(TempId::from_usize(2)),
                        field: arandu_middle::amir::ENUM_PAYLOAD_FIELD,
                    },
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::FieldAccess {
                        base: AmirOperand::Copy(TempId::from_usize(2)),
                        field: arandu_middle::amir::ENUM_PAYLOAD_FIELD,
                    },
                },
            ],
            vec![temp(0, err_ty), temp(1, int_ty), temp(2, result_ty)],
        );

        assert!(!crate::gvn::gvn(&mut f));
        assert!(matches!(
            f.stmt(crate::amir::InstrId::from_usize(1)),
            AmirStmt::Assign {
                rhs: AmirRvalue::FieldAccess { .. },
                ..
            }
        ));
    }

    #[test]
    fn sroa_resolves_field_access_from_struct_literal() {
        let mut f = func(
            vec![
                // _1 = StructLiteral { fields: [("x", _2), ("y", _0)] }
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::StructLiteral {
                        struct_symbol: crate::SymbolId::new(0, 0),
                        fields: vec![
                            ("x".into(), AmirOperand::Copy(TempId::from_usize(2))),
                            ("y".into(), AmirOperand::Copy(TempId::from_usize(0))),
                        ],
                    },
                },
                // _0 = _1.0
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::FieldAccess {
                        base: AmirOperand::Copy(TempId::from_usize(1)),
                        field: 0,
                    },
                },
            ],
            vec![int_temp(0), int_temp(1), int_temp(2)],
        );

        assert!(crate::sroa::sroa(&mut f));
        // Check that field access was resolved to Use(_2)
        if let AmirStmt::Assign { lhs, rhs } = f.stmt(crate::amir::InstrId::from_usize(1)) {
            assert_eq!(lhs.as_usize(), 0);
            assert!(matches!(
                rhs,
                AmirRvalue::Use(AmirOperand::Copy(t)) if t.as_usize() == 2
            ));
        } else {
            panic!("expected Assign");
        }
    }

    #[test]
    fn sroa_resolves_tuple_field_access() {
        let mut f = func(
            vec![
                // _1 = Tuple { items: [_2, _0] }
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Tuple {
                        items: vec![
                            AmirOperand::Copy(TempId::from_usize(2)),
                            AmirOperand::Copy(TempId::from_usize(0)),
                        ],
                    },
                },
                // _0 = _1.1
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::FieldAccess {
                        base: AmirOperand::Copy(TempId::from_usize(1)),
                        field: 1,
                    },
                },
            ],
            vec![int_temp(0), int_temp(1), int_temp(2)],
        );

        assert!(crate::sroa::sroa(&mut f));
        if let AmirStmt::Assign { lhs, rhs } = f.stmt(crate::amir::InstrId::from_usize(1)) {
            assert_eq!(lhs.as_usize(), 0);
            assert!(matches!(
                rhs,
                AmirRvalue::Use(AmirOperand::Copy(t)) if t.as_usize() == 0
            ));
        } else {
            panic!("expected Assign");
        }
    }
}
