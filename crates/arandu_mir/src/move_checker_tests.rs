fn intern_ty(ty: crate::types::ArType) -> crate::types::TypeId {
    // Fresh interner per call is OK in unit tests (pre-interns primitives).
    crate::types::TypeInterner::new().intern(ty)
}
use super::*;
use crate::amir::program::extend_block_range;
use crate::amir::{AmirBasicBlock, AmirLocal, AmirStmtTable, AmirTemp, BlockId};
use crate::layout::DenseRange;
use crate::passes::type_checker::types::{ArType, Primitive};
use smallvec::smallvec;

fn non_copy_ty() -> ArType {
    ArType::Named(crate::SymbolId::new(0, 0), crate::hir::IndexRange::empty())
}

fn int_ty() -> ArType {
    ArType::Primitive(Primitive::Int)
}

fn local(id: usize, ty: ArType) -> AmirLocal {
    let is_memory = !ty.is_copy_v01() && !matches!(ty, ArType::Primitive(_));
    AmirLocal {
        id: LocalId::from_usize(id),
        ty: intern_ty(ty),
        is_memory,
        symbol: None,
        span: Span::new(0, 0, 0),
        use_span: None,
    }
}

fn temp(id: usize, ty: ArType) -> AmirTemp {
    let is_copy = ty.is_copy_v01();
    let is_nullable = matches!(ty, crate::types::ArType::Nullable(_));
    AmirTemp {
        id: TempId::from_usize(id),
        ty: intern_ty(ty),
        is_copy,
        is_nullable,
        span: Span::new(0, 0, 0),
    }
}

fn place(local: usize) -> AmirPlace {
    AmirPlace {
        local: LocalId::from_usize(local),
        projections: smallvec![],
    }
}

fn block(statements: Vec<AmirStmt>, stmts: &mut AmirStmtTable) -> AmirBasicBlock {
    let mut range = DenseRange::empty();
    for stmt in statements {
        let instr = stmts.push(stmt);
        extend_block_range(&mut range, instr);
    }
    AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: range,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }
}

fn make_func(
    blocks: Vec<AmirBasicBlock>,
    locals: Vec<AmirLocal>,
    temps: Vec<AmirTemp>,
    stmts: AmirStmtTable,
) -> AmirFunc {
    let cfg = crate::cfg::compute_cfg_edges(&blocks);
    AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: intern_ty(ArType::Void),
        receiver: None,
        params: Vec::new(),
        locals,
        temps,
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    }
}

#[test]
fn duplicate_destroy_reports_double_free() {
    let mut stmts = AmirStmtTable::new();
    let func = make_func(
        vec![block(
            vec![AmirStmt::Destroy(place(0)), AmirStmt::Destroy(place(0))],
            &mut stmts,
        )],
        vec![local(0, non_copy_ty())],
        Vec::new(),
        stmts,
    );
    let symbols = SymbolTable::new(0);
    let diags = check_moves(&func, &symbols);

    assert!(
        diags
            .iter()
            .any(|diag| diag.code == DiagCode::O005DoubleFree)
    );
}

#[test]
fn recursive_field_drop_then_root_storage_cleanup_is_not_double_free() {
    let mut field = place(0);
    field
        .projections
        .push(crate::amir::AmirProjection::Field(crate::SymbolId::new(
            0, 1,
        )));
    let mut stmts = AmirStmtTable::new();
    let func = make_func(
        vec![block(
            vec![AmirStmt::Destroy(field), AmirStmt::Destroy(place(0))],
            &mut stmts,
        )],
        vec![local(0, non_copy_ty())],
        Vec::new(),
        stmts,
    );
    let symbols = SymbolTable::new(0);

    assert!(check_moves(&func, &symbols).is_empty());
}

#[test]
fn local_diag_span_prefers_use_over_decl() {
    let symbols = SymbolTable::new(0);
    let mut stmts = AmirStmtTable::new();
    let func = make_func(
        vec![block(
            vec![AmirStmt::Destroy(place(0)), AmirStmt::Destroy(place(0))],
            &mut stmts,
        )],
        {
            let mut l = local(0, non_copy_ty());
            l.span = Span::new(0, 1, 2);
            l.use_span = Some(Span::new(0, 10, 15));
            vec![l]
        },
        Vec::new(),
        stmts,
    );
    let diags = check_moves(&func, &symbols);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::O005DoubleFree)
        .expect("O005");
    assert_eq!(d.span, Span::new(0, 10, 15));
}

#[test]
fn available_local_no_error() {
    let mut stmts = AmirStmtTable::new();
    let func = make_func(
        vec![block(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(0),
                rhs: AmirRvalue::Load(place(0)),
            }],
            &mut stmts,
        )],
        vec![local(0, non_copy_ty())],
        vec![temp(0, non_copy_ty())],
        stmts,
    );
    let symbols = SymbolTable::new(0);
    assert!(check_moves(&func, &symbols).is_empty());
}

#[test]
fn use_after_move_reports_error() {
    let mut stmts = AmirStmtTable::new();
    let func = make_func(
        vec![block(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Load(place(0)),
                },
                AmirStmt::Destroy(place(0)),
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Load(place(0)),
                },
            ],
            &mut stmts,
        )],
        vec![local(0, non_copy_ty())],
        vec![temp(0, non_copy_ty())],
        stmts,
    );
    let symbols = SymbolTable::new(0);
    let diags = check_moves(&func, &symbols);
    assert!(diags.iter().any(|d| d.code == DiagCode::O001UseAfterMove));
}

#[test]
fn move_on_one_branch_maybe_moved() {
    let mut stmts = AmirStmtTable::new();
    let b0 = BlockId::from_usize(0);
    let b1 = BlockId::from_usize(1);
    let b2 = BlockId::from_usize(2);
    let b3 = BlockId::from_usize(3);

    let mut range0 = DenseRange::empty();
    extend_block_range(
        &mut range0,
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Load(place(0)),
        }),
    );
    let block0 = AmirBasicBlock {
        id: b0,
        statements: range0,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId::from_usize(0)),
            if_true: b1,
            true_args: Vec::new(),
            if_false: b2,
            false_args: Vec::new(),
        },
    };

    let mut range1 = DenseRange::empty();
    extend_block_range(&mut range1, stmts.push(AmirStmt::Destroy(place(0))));
    let block1 = AmirBasicBlock {
        id: b1,
        statements: range1,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: b3,
            args: Vec::new(),
        },
    };

    let block2 = AmirBasicBlock {
        id: b2,
        statements: DenseRange::empty(),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: b3,
            args: Vec::new(),
        },
    };

    let mut range3 = DenseRange::empty();
    extend_block_range(
        &mut range3,
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Load(place(0)),
        }),
    );
    let block3 = AmirBasicBlock {
        id: b3,
        statements: range3,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };

    let func = make_func(
        vec![block0, block1, block2, block3],
        vec![local(0, non_copy_ty())],
        vec![temp(0, non_copy_ty())],
        stmts,
    );
    let symbols = SymbolTable::new(0);
    let diags = check_moves(&func, &symbols);
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O007InconsistentMoveBetweenBranches)
    );
}

#[test]
fn copy_type_move_does_not_mark_origin_moved() {
    let mut stmts = AmirStmtTable::new();
    let func = make_func(
        vec![block(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Load(place(0)),
                },
                AmirStmt::Store {
                    lhs: place(1),
                    rhs: AmirOperand::Copy(TempId::from_usize(0)),
                },
            ],
            &mut stmts,
        )],
        vec![local(0, int_ty())],
        vec![temp(0, int_ty())],
        stmts,
    );
    let symbols = SymbolTable::new(0);

    assert!(check_moves(&func, &symbols).is_empty());
}

#[test]
fn branch_arms_both_moving_same_local_do_not_conflict() {
    // bb0 branches on cond; true_args moves local0, false_args moves local0
    // Since true and false are mutually exclusive paths, this must NOT report O001.
    let mut stmts = AmirStmtTable::new();
    let r0 = stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Load(place(0)),
    });
    let r_cond = stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Load(place(1)),
    });
    let mut range0 = DenseRange::empty();
    extend_block_range(&mut range0, r0);
    extend_block_range(&mut range0, r_cond);

    let block0 = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: range0,
        params: DenseRange::empty(),
        terminator: AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId::from_usize(1)),
            if_true: BlockId::from_usize(1),
            if_false: BlockId::from_usize(2),
            true_args: vec![AmirOperand::Move(TempId::from_usize(0))],
            false_args: vec![AmirOperand::Move(TempId::from_usize(0))],
        },
    };

    let block1 = AmirBasicBlock {
        id: BlockId::from_usize(1),
        statements: DenseRange::empty(),
        params: DenseRange::new(0, 1),
        terminator: AmirTerminator::Return,
    };

    let block2 = AmirBasicBlock {
        id: BlockId::from_usize(2),
        statements: DenseRange::empty(),
        params: DenseRange::new(1, 1),
        terminator: AmirTerminator::Return,
    };

    let mut func = make_func(
        vec![block0, block1, block2],
        vec![
            local(0, non_copy_ty()),
            local(1, int_ty()),
            local(2, non_copy_ty()),
            local(3, non_copy_ty()),
        ],
        vec![
            temp(0, non_copy_ty()),
            temp(1, int_ty()),
            temp(2, non_copy_ty()),
            temp(3, non_copy_ty()),
        ],
        stmts,
    );
    func.block_params = vec![
        crate::amir::BlockParam {
            id: TempId::from_usize(2),
            local: LocalId::from_usize(2),
            ty: intern_ty(non_copy_ty()),
            from: None,
            moved: true,
        },
        crate::amir::BlockParam {
            id: TempId::from_usize(3),
            local: LocalId::from_usize(3),
            ty: intern_ty(non_copy_ty()),
            from: None,
            moved: true,
        },
    ];
    let symbols = SymbolTable::new(0);
    let diags = check_moves(&func, &symbols);
    assert!(
        diags.is_empty(),
        "expected no move errors when both branch arms move to their target, got: {:?}",
        diags
    );
}
