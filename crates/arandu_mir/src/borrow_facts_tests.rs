#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::Span;
use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirRvalue, AmirStmt,
    AmirStmtTable, AmirTemp, AmirTerminator, TempId,
};
use crate::cfg::compute_cfg_edges;
use crate::layout::DenseRange;
use crate::ops::UnaryOp;
use crate::types::{
    ArType, BorrowKind, BorrowPath, BorrowPathSegment, BorrowSource, Primitive,
    ReturnBorrowDependency, ReturnBorrowSummary, TypeInterner,
};
use smallvec::smallvec;

fn intern_ty(ty: ArType) -> crate::types::TypeId {
    TypeInterner::new().intern(ty)
}

fn local(i: usize, ty: crate::types::TypeId) -> AmirLocal {
    AmirLocal {
        id: LocalId::from_usize(i),
        ty,
        is_memory: true,
        symbol: None,
        span: Span::new(0, 0, 0),
        use_span: None,
    }
}

fn temp(i: usize, ty: crate::types::TypeId) -> AmirTemp {
    AmirTemp {
        id: TempId::from_usize(i),
        ty,
        is_copy: true,
        is_nullable: false,
        span: Span::new(0, 0, 0),
    }
}

fn place(l: usize) -> AmirPlace {
    AmirPlace {
        local: LocalId::from_usize(l),
        projections: smallvec![],
    }
}

/// `t0 = &s0` with no further use → loan dies; OUT not borrowed (F2.2).
#[test]
fn dead_ref_ends_loan_at_block_out() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    assert_eq!(facts.borrow_site_counts[0], 1);
    assert_eq!(facts.loans.len(), 1);
    // Dead ref: not live-out → place not borrowed at OUT.
    assert!(!facts.block_out[0].maybe_borrowed(LocalId::from_usize(0)));
    assert!(!facts.maybe_shared_at_entry(BlockId::from_usize(0), LocalId::from_usize(0)));
}

/// `t0 = &s0; t1 = *t0` → loan active while t0 live; ends after last use.
#[test]
fn live_ref_use_keeps_loan_through_use() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 2),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int), temp(1, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    // After last use of t0, not live-out → OUT clean (debt paid).
    assert!(!facts.block_out[0].maybe_borrowed(LocalId::from_usize(0)));
    // Loan was opened.
    assert_eq!(facts.loans.len(), 1);
    assert!(facts.loans[0].holder_temps.contains(TempId::from_usize(0)));
}

/// Loan propagates across edge when holder is used in successor.
#[test]
fn borrow_propagates_to_successor_when_ref_live() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    // bb0: t0 = &s0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    // bb1: t1 = *t0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });

    let bb0 = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: BlockId::from_usize(1),
            args: vec![],
        },
    };
    let bb1 = AmirBasicBlock {
        id: BlockId::from_usize(1),
        statements: DenseRange::new(1, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![bb0, bb1];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int), temp(1, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    // t0 live across edge → bb0 OUT and bb1 IN have shared loan of s0.
    assert!(facts.block_out[0].maybe_shared(LocalId::from_usize(0)));
    assert!(facts.maybe_shared_at_entry(BlockId::from_usize(1), LocalId::from_usize(0)));
    // After use in bb1, OUT clean.
    assert!(!facts.block_out[1].maybe_borrowed(LocalId::from_usize(0)));
}

#[test]
fn borrow_mut_marks_exclusive_while_live() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::BorrowMut(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 2),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, intern_ty(ArType::RefMut(int))), temp(1, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let facts = analyze_borrow_facts(&func);
    assert_eq!(facts.loans[0].kind, LoanKind::Exclusive);
    assert!(!facts.block_out[0].maybe_borrowed(LocalId::from_usize(0)));
}

/// `let p = &n` via Store propagates holder to local.
#[test]
fn store_to_ref_local_propagates_holder() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    // t0 = &s0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    // s1 = t0  (p = &n)
    stmts.push(AmirStmt::Store {
        lhs: place(1),
        rhs: AmirOperand::Copy(TempId::from_usize(0)),
    });
    // t1 = load s1; use in next block
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Load(place(1)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(1)),
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
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int), local(1, ref_int)],
        temps: vec![temp(0, ref_int), temp(1, ref_int), temp(2, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let facts = analyze_borrow_facts(&func);
    assert!(
        facts.loans[0]
            .holder_locals
            .contains(LocalId::from_usize(1)),
        "ref local s1 should hold the loan"
    );
    assert!(!facts.block_out[0].maybe_borrowed(LocalId::from_usize(0)));
}

#[test]
fn is_borrowed_at_entry_matches_block_in() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let bb0 = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: BlockId::from_usize(1),
            args: vec![],
        },
    };
    let bb1 = AmirBasicBlock {
        id: BlockId::from_usize(1),
        statements: DenseRange::new(1, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![bb0, bb1];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int), temp(1, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let facts = analyze_borrow_facts(&func);
    let pt = ProgramPoint {
        block: BlockId::from_usize(1),
        stmt_index: 0,
    };
    assert!(is_borrowed_at(&facts, LocalId::from_usize(0), pt));
    assert!(facts.maybe_shared_at_entry(BlockId::from_usize(1), LocalId::from_usize(0)));
}

#[test]
fn tuple_carrier_and_projection_preserve_structural_holder() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let ref_int = interner.intern(ArType::Ref(int));
    let tuple_ty = interner.intern(ArType::tuple(&[ref_int, int], &interner));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Tuple {
            items: vec![
                AmirOperand::Copy(TempId::from_usize(0)),
                AmirOperand::Constant(crate::amir::AmirConstant::Nil),
            ],
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::FieldAccess {
            base: AmirOperand::Copy(TempId::from_usize(1)),
            field: 0,
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int), temp(1, tuple_ty), temp(2, ref_int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    let loan = &facts.loans[0];
    assert!(loan.holder_temp_paths[1].contains(&HolderPath(vec![HolderProjection::Slot(0)])));
    assert!(loan.holder_temp_paths[2].contains(&HolderPath::default()));
}

#[test]
fn projected_store_and_load_preserve_holder_path() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let field = crate::SymbolId::new(7, 9);
    let mut projected = place(1);
    projected
        .projections
        .push(crate::amir::AmirProjection::Field(field));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Store {
        lhs: projected.clone(),
        rhs: AmirOperand::Copy(TempId::from_usize(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Load(projected),
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int), local(1, ref_int)],
        temps: vec![temp(0, ref_int), temp(1, ref_int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    let loan = &facts.loans[0];
    assert!(loan.holder_locals.contains(LocalId::from_usize(1)));
    assert!(loan.holder_temp_paths[1].contains(&HolderPath::default()));
}

#[test]
fn projected_reborrow_keeps_original_owner_loan_through_reference_slot() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let reference = interner.intern(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Store {
        lhs: place(1),
        rhs: AmirOperand::Copy(TempId::from_usize(0)),
    });
    let mut pointee = place(1);
    pointee.projections.push(crate::amir::AmirProjection::Deref);
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(pointee),
    });
    stmts.push(AmirStmt::Store {
        lhs: place(2),
        rhs: AmirOperand::Copy(TempId::from_usize(1)),
    });
    stmts.push(AmirStmt::Destroy(place(2)));
    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 5),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int), local(1, reference), local(2, reference)],
        temps: vec![temp(0, reference), temp(1, reference)],
        cfg: compute_cfg_edges(&blocks),
        blocks,
        block_params: Vec::new(),
        stmts,
    };
    let facts = analyze_borrow_facts(&func);
    assert_eq!(facts.loans[0].place_local, LocalId::from_usize(0));
    assert!(facts.loans[0].holder_temp_paths[1].contains(&HolderPath::default()));
    assert!(facts.loans[0].holder_local_paths[2].contains(&HolderPath::default()));
}

#[test]
fn enum_payload_round_trip_preserves_holder() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::EnumConstruct {
            variant_tag: 3,
            payload: Some(AmirOperand::Copy(TempId::from_usize(0))),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::EnumPayload {
            value: AmirOperand::Copy(TempId::from_usize(1)),
            variant: crate::SymbolId::new(0, 3),
            variant_tag: 3,
            index: 0,
            field_ty: ref_int,
            tuple_ty: None,
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int), temp(1, ref_int), temp(2, ref_int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    assert!(facts.loans[0].holder_temp_paths[2].contains(&HolderPath::default()));
}

#[test]
fn overwrite_kills_only_the_destination_holder_state() {
    let int = intern_ty(ArType::Primitive(Primitive::Int));
    let ref_int = intern_ty(ArType::Ref(int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Store {
        lhs: place(1),
        rhs: AmirOperand::Copy(TempId::from_usize(0)),
    });
    stmts.push(AmirStmt::Store {
        lhs: place(1),
        rhs: AmirOperand::Constant(crate::amir::AmirConstant::Nil),
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int), local(1, ref_int)],
        temps: vec![temp(0, ref_int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    assert!(facts.local_holders_at[0][2][0].contains(LocalId::from_usize(1)));
    assert!(!facts.local_holders_at[0][3][0].contains(LocalId::from_usize(1)));
}

#[test]
fn call_summary_composes_result_and_parameter_paths() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let ref_int = interner.intern(ArType::Ref(int));
    let tuple_ty = interner.intern(ArType::tuple(&[ref_int], &interner));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Call {
        lhs: Some(TempId::from_usize(1)),
        callee: AmirOperand::FunctionRef(crate::SymbolId::new(0, 9)),
        args: smallvec![AmirOperand::Copy(TempId::from_usize(0))],
        return_borrow: Some(ReturnBorrowSummary {
            dependencies: vec![ReturnBorrowDependency {
                result_path: BorrowPath(smallvec![BorrowPathSegment::Tuple(0)]),
                sources: vec![BorrowSource {
                    parameter_index: 0,
                    parameter_path: BorrowPath::root(),
                }],
                kind: BorrowKind::Shared,
            }],
        }),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::FieldAccess {
            base: AmirOperand::Copy(TempId::from_usize(1)),
            field: 0,
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_int), temp(1, tuple_ty), temp(2, ref_int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };

    let facts = analyze_borrow_facts(&func);
    assert!(facts.loans[0].holder_temp_paths[2].contains(&HolderPath::default()));
}
