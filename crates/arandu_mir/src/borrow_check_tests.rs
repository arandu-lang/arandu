#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::SymbolId;
use crate::amir::{AmirBasicBlock, AmirLocal, AmirPlace, AmirStmtTable, AmirTemp, AmirTerminator};
use crate::cfg::compute_cfg_edges;
use crate::layout::DenseRange;
use crate::ops::UnaryOp;
use crate::types::{ArType, Primitive, TypeInterner};
use smallvec::smallvec;

fn intern(ty: ArType) -> crate::types::TypeId {
    TypeInterner::new().intern(ty)
}

fn empty_symbols() -> SymbolTable {
    SymbolTable::new(0)
}

fn place(l: usize) -> AmirPlace {
    AmirPlace {
        local: LocalId::from_usize(l),
        projections: smallvec![],
    }
}

fn local(i: usize, ty: crate::types::TypeId) -> AmirLocal {
    AmirLocal {
        id: LocalId::from_usize(i),
        ty,
        is_memory: true,
        symbol: None,
        span: Span::new(0, 0, 1 + i as u32),
        use_span: None,
    }
}

fn temp(i: usize, ty: crate::types::TypeId) -> AmirTemp {
    AmirTemp {
        id: TempId::from_usize(i),
        ty,
        is_copy: true,
        is_nullable: false,
        span: Span::new(0, 10 + i as u32, 11 + i as u32),
    }
}

#[test]
fn place_overlap_distinguishes_fields_and_keeps_aliasing_conservative() {
    let root = LocalId::from_usize(0);
    let left = SymbolId::new(0, 10);
    let right = SymbolId::new(0, 11);
    let left_path = [AmirProjection::Field(left)];
    let right_path = [AmirProjection::Field(right)];
    let nested_left = [AmirProjection::Field(left), AmirProjection::Field(right)];
    let deref = [AmirProjection::Deref];

    assert!(!places_may_overlap(root, &left_path, root, &right_path));
    assert!(places_may_overlap(root, &[], root, &left_path));
    assert!(places_may_overlap(root, &left_path, root, &nested_left));
    assert!(places_may_overlap(root, &deref, root, &deref));
    assert!(!places_may_overlap(
        root,
        &left_path,
        LocalId::from_usize(1),
        &left_path,
    ));
}

/// `&mut x` then `&x` while first loan live → O003.
#[test]
fn o003_shared_while_exclusive() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::BorrowMut(place(0)),
    });
    // Keep t0 live by using it after the second borrow attempt... actually
    // second borrow at stmt 1 while t0 still live-out if used later.
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
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
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![
            temp(0, intern(ArType::RefMut(int))),
            temp(1, intern(ArType::Ref(int))),
            temp(2, int),
        ],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
        "expected O003, got {diags:?}"
    );
}

#[test]
fn aggregate_guard_keeps_owner_borrowed_inside_one_block_until_drop() {
    let interner = TypeInterner::new();
    let result = interner.intern(ArType::Primitive(Primitive::Bool));
    let owner = interner.intern(ArType::named(SymbolId::new(0, 90), &[], &interner));
    let borrowed = interner.intern(ArType::Ref(owner));
    let guard = interner.intern(ArType::tuple(&[borrowed], &interner));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Tuple {
            items: vec![AmirOperand::Copy(TempId::from_usize(1))],
        },
    });
    stmts.push(AmirStmt::Store {
        lhs: place(1),
        rhs: AmirOperand::Move(TempId::from_usize(2)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(3),
        rhs: AmirRvalue::Load(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(4),
        rhs: AmirRvalue::Use(AmirOperand::Move(TempId::from_usize(3))),
    });
    stmts.push(AmirStmt::Destroy(place(1)));
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Use(AmirOperand::Constant(crate::amir::AmirConstant::Bool(
            false,
        ))),
    });
    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 7),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }];
    let mut moving = temp(3, owner);
    moving.is_copy = false;
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: result,
        receiver: None,
        params: vec![],
        locals: vec![local(0, owner), local(1, guard)],
        temps: vec![
            temp(0, result),
            temp(1, borrowed),
            temp(2, guard),
            moving,
            temp(4, owner),
        ],
        cfg: compute_cfg_edges(&blocks),
        blocks,
        block_params: Vec::new(),
        stmts,
    };
    let facts = analyze_borrow_facts(&func);
    // The aggregate is live only in the block interior: neither boundary may
    // stand in for the point-specific live set in the borrow checker.
    assert!(
        !facts
            .local_live
            .live_in(BlockId::from_usize(0))
            .contains(LocalId::from_usize(1))
    );
    assert!(
        !facts
            .local_live
            .live_out(BlockId::from_usize(0))
            .contains(LocalId::from_usize(1))
    );
    assert!(
        facts
            .local_live
            .live_before(BlockId::from_usize(0), 4)
            .unwrap()
            .contains(LocalId::from_usize(1))
    );
    let diagnostics = check_borrows(&func, &empty_symbols());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::O002MoveWhileBorrowed),
        "{diagnostics:?}"
    );
}

/// Move of local while `&` live → O002.
#[test]
fn o002_move_while_borrowed() {
    let int = intern(ArType::Primitive(Primitive::Int));
    // Non-copy local of a "struct" so Move matters — use Named non-copy.
    let named = intern(ArType::Named(
        SymbolId::new(0, 1),
        crate::hir::IndexRange::empty(),
    ));
    let mut stmts = AmirStmtTable::new();
    // t0 = &s0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    // t1 = Load s0 (non-copy)
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Load(place(0)),
    });
    // consume Move(t1) while t0 still live
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Use(AmirOperand::Move(TempId::from_usize(1))),
    });
    // keep t0 live
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(3),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
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
    let mut t1 = temp(1, named);
    t1.is_copy = false;
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, named)],
        temps: vec![
            temp(0, intern(ArType::Ref(named))),
            t1,
            {
                let mut t = temp(2, named);
                t.is_copy = false;
                t
            },
            temp(3, named),
        ],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O002MoveWhileBorrowed),
        "expected O002, got {diags:?}"
    );
}

/// Destroy while borrow live → O006.
#[test]
fn o006_destroy_while_borrowed() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Destroy(place(0)));
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
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
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, intern(ArType::Ref(int))), temp(1, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O006DestroyWhileBorrowed),
        "expected O006, got {diags:?}"
    );
}

/// Shared + shared is OK.
#[test]
fn shared_shared_ok() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(3),
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
    let ref_ty = intern(ArType::Ref(int));
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_ty), temp(1, ref_ty), temp(2, int), temp(3, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags.is_empty(),
        "shared+shared should be allowed, got {diags:?}"
    );
}

/// Mirror CLI: &mut then & then call using both holders.
#[test]
fn o003_two_loans_then_call() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Store {
        lhs: place(0),
        rhs: AmirOperand::Constant(crate::amir::AmirConstant::Pool(
            crate::literal_pool::LiteralId(0),
        )),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::BorrowMut(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Call {
        lhs: Some(TempId::from_usize(2)),
        callee: AmirOperand::FunctionRef(SymbolId::new(0, 99)),
        args: smallvec::smallvec![
            AmirOperand::Copy(TempId::from_usize(0)),
            AmirOperand::Copy(TempId::from_usize(1)),
        ],
        return_borrow: None,
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
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![
            temp(0, intern(ArType::RefMut(int))),
            temp(1, intern(ArType::Ref(int))),
            temp(2, int),
        ],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
        "expected O003, got {diags:?}"
    );
}

/// Mutating a projected field `s0.f` while `s0` is borrowed → O003.
#[test]
fn o003_store_to_projected_field_while_borrowed() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    // t0 = &s0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    // Store s0.f = 42 (projected place) while t0 is still live!
    let mut projected_place = place(0);
    projected_place
        .projections
        .push(crate::amir::AmirProjection::Field(SymbolId::new(0, 10)));
    stmts.push(AmirStmt::Store {
        lhs: projected_place,
        rhs: AmirOperand::Constant(crate::amir::AmirConstant::Bool(true)),
    });
    // Use t0 after store
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
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
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, intern(ArType::Ref(int))), temp(1, int)],
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
        "expected O003 for store to field while borrowed, got {diags:?}"
    );
}

#[test]
fn scalar_read_releases_owner_loan_but_reference_return_keeps_it() {
    let interner = TypeInterner::new();
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let reference = interner.intern(ArType::Ref(int));
    for returns_reference in [false, true] {
        let result = if returns_reference { reference } else { int };
        let mut statements = AmirStmtTable::new();
        statements.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        statements.push(AmirStmt::Store {
            lhs: place(1),
            rhs: AmirOperand::Copy(TempId::from_usize(1)),
        });
        let loaded = if returns_reference {
            place(1)
        } else {
            AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![AmirProjection::Deref],
            }
        };
        statements.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Load(loaded),
        });
        statements.push(AmirStmt::Destroy(place(0)));
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 4),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let function = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: result,
            receiver: None,
            params: vec![],
            locals: vec![local(0, int), local(1, reference)],
            temps: vec![temp(0, result), temp(1, reference)],
            cfg: compute_cfg_edges(&blocks),
            blocks,
            block_params: vec![],
            stmts: statements,
        };
        let diagnostics = check_borrows_with_types(&function, &empty_symbols(), &interner);
        assert_eq!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::O006DestroyWhileBorrowed),
            returns_reference,
            "{diagnostics:?}"
        );
        let escapes = crate::escape_analysis::find_escapes(&function, &interner);
        assert_eq!(
            escapes
                .iter()
                .any(|event| event.kind == crate::escape_analysis::EscapeKind::Return),
            returns_reference,
            "{escapes:?}"
        );
    }
}
