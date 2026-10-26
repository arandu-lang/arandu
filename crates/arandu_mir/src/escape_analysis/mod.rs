//! F2.3 — Escape analysis + O004 generational-fallback notes (G2 fused).
//!
//! ## Decision rule (design gold)
//!
//! ```text
//! if live_range(ref) is bound inside this function's CFG
//!    (does not escape via return / heap / closure):
//!     → static borrow window (F2.2); no O004
//! else:
//!     → generational / escape path:
//!         • always emit O004 (inspectable — never silent magic)
//!         • return of `ref local` → also O010 (hard error; cannot gen-fix a stack frame)
//!         • with @NoFallback / --no-generational-fallback → O004 is Error (G2)
//! ```
//!
//! G2 is **not** a silent global “strict mode”: it only promotes O004 notes to
//! errors in scopes that opt in (function attribute or CLI flag).
//!
//! Market context (Vale-style gen refs): objects carry a generation; refs
//! remember it; free bumps generation. Arandu keeps that as a **controlled
//! fallback** after static OSSA fails — stack-first by default, inspectable
//! when the compiler must leave the pure static world.

mod check;
mod places;
mod types;
mod walk;

pub use check::{check_escapes, check_escapes_by_block, check_escapes_with_type_info};
pub use types::{EscapeCheckOptions, EscapeEvent, EscapeKind};
pub use walk::{find_escapes, find_escapes_with_type_info};

#[cfg(test)]
use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId,
};
#[cfg(test)]
use crate::diagnostics::DiagCode;
#[cfg(test)]
use crate::{Span, SymbolTable};
#[cfg(test)]
use arandu_middle::types::ReturnBorrowSummary;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::SymbolId;
    use crate::amir::{
        AmirBasicBlock, AmirLocal, AmirPlace, AmirProjection, AmirStmtTable, AmirTemp,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::types::{ArType, Primitive, TypeInterner};
    use smallvec::smallvec;

    fn place(l: usize) -> AmirPlace {
        AmirPlace {
            local: LocalId::from_usize(l),
            projections: smallvec![],
        }
    }

    fn empty_symbols() -> SymbolTable {
        SymbolTable::new(0)
    }

    #[test]
    fn return_ref_to_local_is_o010() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        // t0 = &s0  (return slot is t0? use assign to return temp 0)
        // Actually: t1 = &s0; _0 = use t1
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
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
            symbol: SymbolId::new(0, 0),
            return_type: ref_int,
            receiver: None,
            params: vec![],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 10, 11),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 12, 13),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions::default(),
        );
        assert!(
            diags
                .iter()
                .any(|d| d.code == DiagCode::O010EscapeOfBorrowedValue),
            "expected O010, got {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.code == DiagCode::O004GenerationalFallback),
            "expected O004 note, got {diags:?}"
        );
        assert!(
            diags
                .iter()
                .filter(|d| d.code == DiagCode::O004GenerationalFallback)
                .all(|d| d.severity == crate::diagnostics::Severity::Note),
            "O004 default must be Note, not silent Error"
        );
    }

    #[test]
    fn no_fallback_promotes_o004_to_error() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
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
            symbol: SymbolId::new(0, 0),
            return_type: ref_int,
            receiver: None,
            params: vec![],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 10, 11),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 12, 13),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions {
                no_fallback: true,
                ..EscapeCheckOptions::default()
            },
        );
        let o004: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::O004GenerationalFallback)
            .collect();
        assert!(!o004.is_empty());
        assert!(
            o004.iter()
                .all(|d| d.severity == crate::diagnostics::Severity::Error),
            "G2 must promote O004 to Error, got {o004:?}"
        );
    }

    #[test]
    fn return_ref_to_param_remains_rejected_without_interprocedural_provenance() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        // t1 = &s0 (where s0 is param 0); _0 = use t1
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
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
            symbol: SymbolId::new(0, 0),
            return_type: ref_int,
            receiver: None,
            // 1 parameter (TempId(2)) corresponding to local 0
            params: vec![TempId::from_usize(2)],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 10, 11),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 12, 13),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 5, 6),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions::default(),
        );
        assert_eq!(
            diags
                .iter()
                .filter(|diag| diag.code == DiagCode::O010EscapeOfBorrowedValue)
                .count(),
            1,
            "borrowed parameter return must fail closed until callers inherit its loan: {diags:?}"
        );

        let declared = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions {
                no_fallback: false,
                return_borrow: Some(ReturnBorrowSummary::direct(
                    0,
                    arandu_middle::types::BorrowKind::Shared,
                )),
            },
        );
        assert!(
            declared.is_empty(),
            "a declared parameter dependency should make the return safe: {declared:?}"
        );
    }

    #[test]
    fn escape_via_struct_literal_detected_as_heap_store() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let struct_ty = interner.intern(ArType::named(SymbolId::new(0, 1), &[], &interner));
        let ref_struct_ty = interner.intern(ArType::RefMut(struct_ty));
        let void = interner.intern(ArType::Void);

        let mut stmts = AmirStmtTable::new();
        // t1 = &local0
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        // t2 = StructLiteral { ref: t1 }
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: SymbolId::new(0, 1),
                fields: vec![(
                    smol_str::SmolStr::new("ptr"),
                    AmirOperand::Copy(TempId::from_usize(1)),
                )],
            },
        });
        // Store through a reference can outlive this stack frame.
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![AmirProjection::Deref],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        // The same aggregate in a stack local is not an escape.
        stmts.push(AmirStmt::Store {
            lhs: place(2),
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
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
            return_type: void,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: ref_struct_ty,
                    is_memory: false,
                    symbol: None,
                    span: Span::new(0, 2, 3),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(2),
                    ty: struct_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 3, 4),
                    use_span: None,
                },
            ],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: void,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: struct_ty,
                    is_copy: false,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        let diags = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions {
                no_fallback: true,
                ..EscapeCheckOptions::default()
            },
        );

        let o004: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::O004GenerationalFallback)
            .collect();
        assert_eq!(
            o004.len(),
            1,
            "only the store through an external indirection escapes; a stack-local aggregate does not: {diags:?}"
        );
    }

    #[test]
    fn reborrow_return_names_original_stack_owner_not_reference_slot() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let reference = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(place(0)),
        });
        stmts.push(AmirStmt::Store {
            lhs: place(1),
            rhs: AmirOperand::Copy(TempId::from_usize(1)),
        });
        let mut pointee = place(1);
        pointee.projections.push(AmirProjection::Deref);
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Borrow(pointee),
        });
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 3),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: reference,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 1, 2),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: reference,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 3, 4),
                    use_span: None,
                },
            ],
            temps: (0..2)
                .map(|i| AmirTemp {
                    id: TempId::from_usize(i),
                    ty: reference,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 5, 6),
                })
                .collect(),
            cfg: compute_cfg_edges(&blocks),
            blocks,
            block_params: Vec::new(),
            stmts,
        };
        let events = find_escapes(&func, &interner);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EscapeKind::Return);
        assert_eq!(events[0].place_local, LocalId::from_usize(0));
        let diagnostics = check_escapes(
            &func,
            &empty_symbols(),
            &interner,
            EscapeCheckOptions::default(),
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::O010EscapeOfBorrowedValue)
        );
    }

    fn slice_projection_return(stack_owner: bool, indexed: bool) -> (AmirFunc, TypeInterner) {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let reference = interner.intern(ArType::Ref(int));
        let slice = interner.intern(ArType::Slice(int));
        let owner_type = if stack_owner { int } else { reference };
        let result_type = if indexed {
            reference
        } else {
            interner.intern(ArType::Ref(slice))
        };
        let mut owner = place(0);
        if !stack_owner {
            owner.projections.push(AmirProjection::Deref);
        }
        let mut element = place(1);
        if indexed {
            element.projections.push(AmirProjection::IndexConstant(0));
        }
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(owner),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::SliceView {
                owner: AmirOperand::Copy(TempId::from_usize(1)),
                data: AmirOperand::Constant(crate::amir::AmirConstant::Nil),
                len: AmirOperand::Constant(crate::amir::AmirConstant::Pool(
                    crate::literal_pool::LiteralId(0),
                )),
            },
        });
        stmts.push(AmirStmt::Store {
            lhs: place(1),
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(3),
            rhs: AmirRvalue::Borrow(element),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(3))),
        });
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, stmts.len()),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: result_type,
            receiver: None,
            params: vec![],
            locals: [owner_type, slice]
                .into_iter()
                .enumerate()
                .map(|(index, ty)| AmirLocal {
                    id: LocalId::from_usize(index),
                    ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 1, 2),
                    use_span: None,
                })
                .collect(),
            temps: [result_type, reference, slice, result_type]
                .into_iter()
                .enumerate()
                .map(|(index, ty)| AmirTemp {
                    id: TempId::from_usize(index),
                    ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 3, 4),
                })
                .collect(),
            cfg: compute_cfg_edges(&blocks),
            blocks,
            block_params: Vec::new(),
            stmts,
        };
        (func, interner)
    }

    #[test]
    fn returning_element_of_caller_owned_slice_does_not_escape_local_descriptor() {
        let (func, interner) = slice_projection_return(false, true);
        assert!(find_escapes(&func, &interner).is_empty());
    }

    #[test]
    fn returning_slice_element_retains_original_stack_owner() {
        let (func, interner) = slice_projection_return(true, true);
        let events = find_escapes(&func, &interner);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EscapeKind::Return);
        assert_eq!(events[0].place_local, LocalId::from_usize(0));
    }

    #[test]
    fn returning_reference_to_slice_descriptor_still_escapes_local_storage() {
        let (func, interner) = slice_projection_return(false, false);
        let events = find_escapes(&func, &interner);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EscapeKind::Return);
        assert_eq!(events[0].place_local, LocalId::from_usize(1));
    }
}
