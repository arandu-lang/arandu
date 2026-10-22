use crate::amir::{AmirConstant, AmirFunc};
use crate::literal_pool::AmirLiteralPool;

mod fold;
mod lattice;

use lattice::{analyse, apply};

#[cfg(test)]
use crate::amir::{AmirOperand, AmirRvalue, AmirTerminator};
#[cfg(test)]
use crate::ops::BinaryOp;
#[cfg(test)]
use fold::{const_as_i128, fold_binary, normalize_constant};
#[cfg(test)]
use lattice::jump_args_to;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LatticeVal {
    Undefined,
    Constant(AmirConstant),
    Overdefined,
}

/// Sparse Conditional Constant Propagation (Wegman & Zadeck 1991).
///
/// Propagates constants across basic blocks and eliminates dead branches
/// by jointly modeling value constness and CFG reachability in a single
/// fixpoint iteration.  This subsumes intra-block constant folding and
/// adds cross-block propagation + branch pruning.
///
/// **Block params (SSA φ):** for each param temp, lattice is the meet of the
/// corresponding jump-arg operands from **reachable** predecessors. Without
/// this, loop headers and join blocks stay `Undefined` forever and never
/// refine conditions on block-param values.
pub(super) fn sccp(
    func: &mut AmirFunc,
    pool: &mut AmirLiteralPool,
    bump: &bumpalo::Bump,
    interner: Option<&crate::types::TypeInterner>,
) -> bool {
    let n_temps = func.temps.len();
    let n_blocks = func.blocks.len();
    if n_temps == 0 || n_blocks == 0 {
        return false;
    }

    // Phase 1 – analyse lattice + reachability to fixpoint
    let (lattice, sccp_reachable) = analyse(func, pool, bump, interner);

    // Phase 2 – apply results to the function
    apply(func, pool, &lattice, &sccp_reachable, bump)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::program::extend_block_range;
    use crate::amir::{
        AmirBasicBlock, AmirLocal, AmirPlace, AmirStmt, AmirStmtTable, AmirTemp, BlockId,
        BlockParam, LocalId, TempId,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::passes::type_checker::types::{ArType, Primitive};

    fn intern_ty(ty: ArType) -> crate::types::TypeId {
        crate::types::TypeInterner::new().intern(ty)
    }

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

    #[test]
    fn negative_right_shift_is_not_folded_without_signedness() {
        let mut pool = AmirLiteralPool::default();
        let minus_one = AmirConstant::Pool(pool.intern_int("-1"));
        let one = AmirConstant::Pool(pool.intern_int("1"));

        assert_eq!(
            fold_binary(BinaryOp::ShiftRight, minus_one, one, &mut pool),
            None
        );
    }

    #[test]
    fn fixed_width_constants_keep_assignment_conversion_and_unknown_layout_fails_closed() {
        let interner = crate::types::TypeInterner::new();
        let mut pool = AmirLiteralPool::default();
        for (primitive, input, expected) in [
            (crate::types::Primitive::U8, "260", 4),
            (crate::types::Primitive::I8, "255", -1),
            (crate::types::Primitive::Int, "2147483648", -2147483648),
        ] {
            let value = AmirConstant::Pool(pool.intern_int(input));
            let ty = interner.intern(crate::types::ArType::Primitive(primitive));
            let normalized = normalize_constant(value, ty, Some(&interner), &mut pool).unwrap();
            assert_eq!(const_as_i128(&normalized, &pool), Some(expected));
            assert!(normalize_constant(value, ty, None, &mut pool).is_none());
        }
        let value = AmirConstant::Pool(pool.intern_int("1"));
        let pointer_sized = interner.intern(crate::types::ArType::Primitive(
            crate::types::Primitive::USize,
        ));
        assert!(normalize_constant(value, pointer_sized, Some(&interner), &mut pool).is_none());
    }

    #[test]
    fn duplicate_edges_with_different_arguments_never_select_just_the_first() {
        let target = BlockId(1);
        let yes = vec![AmirOperand::Constant(AmirConstant::Bool(true))];
        let no = vec![AmirOperand::Constant(AmirConstant::Bool(false))];
        let branch = AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId(0)),
            if_true: target,
            true_args: yes.clone(),
            if_false: target,
            false_args: no.clone(),
        };
        assert!(jump_args_to(&branch, target).is_none());
        let switch = AmirTerminator::SwitchInt {
            discriminant: AmirOperand::Copy(TempId(0)),
            targets: vec![(0, target, yes.clone())],
            otherwise: (target, no),
        };
        assert!(jump_args_to(&switch, target).is_none());
        let equal = AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId(0)),
            if_true: target,
            true_args: yes.clone(),
            if_false: target,
            false_args: yes.clone(),
        };
        assert_eq!(jump_args_to(&equal, target), Some(yes.as_slice()));
    }

    #[test]
    fn call_result_contribution_keeps_join_overdefined() {
        let mut pool = AmirLiteralPool::default();
        let bool_ty = intern_ty(ArType::Primitive(Primitive::Bool));
        let mut stmts = AmirStmtTable::new();
        let call = stmts.push(AmirStmt::Call {
            lhs: Some(TempId::from_usize(1)),
            callee: AmirOperand::FunctionRef(crate::SymbolId::new(0, 1)),
            args: smallvec::smallvec![],
            return_borrow: None,
        });
        let mut call_range = DenseRange::empty();
        extend_block_range(&mut call_range, call);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
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
                statements: call_range,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Constant(AmirConstant::Bool(false))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: DenseRange::empty(),
                params: DenseRange::new(0, 1),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(2)),
                    if_true: BlockId::from_usize(4),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(5),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(4),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
            AmirBasicBlock {
                id: BlockId::from_usize(5),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: bool_ty,
                is_memory: false,
                symbol: None,
                span: arandu_lexer::Span::new(0, 0, 0),
                use_span: None,
            }],
            temps: vec![bool_temp(0), bool_temp(1), bool_temp(2)],
            blocks,
            block_params: vec![BlockParam {
                id: TempId::from_usize(2),
                local: LocalId::from_usize(0),
                ty: bool_ty,
                from: None,
                moved: false,
            }],
            stmts,
            cfg,
        };

        let bump = bumpalo::Bump::new();
        let _ = sccp(
            &mut func,
            &mut pool,
            &bump,
            Some(&crate::types::TypeInterner::new()),
        );
        assert!(matches!(
            func.block(BlockId::from_usize(3)).terminator,
            AmirTerminator::Branch { .. }
        ));
    }

    /// Join: both preds pass the same constant into a block param → param is constant.
    #[test]
    fn block_param_meet_same_constant_from_two_preds() {
        let mut pool = AmirLiteralPool::default();
        let five = AmirConstant::Pool(pool.intern_int("5"));

        let mut stmts = AmirStmtTable::new();
        // bb2: t2 = use(param t1)  — should fold to 5 after phi meet
        let a = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let mut range2 = DenseRange::empty();
        extend_block_range(&mut range2, a);

        let int_ty = intern_ty(ArType::Primitive(Primitive::Int));
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: vec![AmirOperand::Constant(five)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(2),
                    args: vec![AmirOperand::Constant(five)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: range2,
                params: DenseRange::new(0, 1),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int_ty,
                is_memory: false,
                symbol: None,
                span: arandu_lexer::Span::new(0, 0, 0),
                use_span: None,
            }],
            temps: vec![int_temp(0), int_temp(1), int_temp(2)],
            blocks,

            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: int_ty,
                from: None,
                moved: false,
            }],

            stmts,
            cfg,
        };

        let bump = bumpalo::Bump::new();
        assert!(sccp(
            &mut func,
            &mut pool,
            &bump,
            Some(&crate::types::TypeInterner::new())
        ));
        let stmt = func.block_stmts(BlockId::from_usize(2)).next().unwrap();
        match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(c)),
                ..
            } => {
                assert_eq!(const_as_i128(c, &pool), Some(5));
            }
            other => panic!("expected folded assign, got {other:?}"),
        }
    }

    /// Different constants from two reachable preds → Overdefined (not a wrong fold).
    #[test]
    fn block_param_meet_different_constants_is_overdefined() {
        let mut pool = AmirLiteralPool::default();
        let one = AmirConstant::Pool(pool.intern_int("1"));
        let two = AmirConstant::Pool(pool.intern_int("2"));
        let int_ty = intern_ty(ArType::Primitive(Primitive::Int));

        let mut stmts = AmirStmtTable::new();
        let a = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let s = stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        let mut range_join = DenseRange::empty();
        extend_block_range(&mut range_join, a);
        extend_block_range(&mut range_join, s);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    // Undefined condition → both arms reachable
                    condition: AmirOperand::Copy(TempId::from_usize(0)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Constant(one)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Constant(two)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: range_join,
                params: DenseRange::new(0, 1),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int_ty,
                is_memory: false,
                symbol: None,
                span: arandu_lexer::Span::new(0, 0, 0),
                use_span: None,
            }],
            temps: vec![int_temp(0), int_temp(1), int_temp(2)],
            blocks,

            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: int_ty,
                from: None,
                moved: false,
            }],

            stmts,
            cfg,
        };
        let bump = bumpalo::Bump::new();
        let _ = sccp(
            &mut func,
            &mut pool,
            &bump,
            Some(&crate::types::TypeInterner::new()),
        );
        let stmt = func.block_stmts(BlockId::from_usize(3)).next().unwrap();
        match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(_)),
                ..
            } => panic!("phi with disagreeing preds must not fold to a constant"),
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Copy(t)),
                ..
            } => assert_eq!(t.as_usize(), 1),
            other => panic!("unexpected {other:?}"),
        }
    }
}
