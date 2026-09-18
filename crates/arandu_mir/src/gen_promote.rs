//! F2.3.runtime — promote escaping borrows to GenInsert/GenGet.
//!
//! For [`EscapeKind::HeapStore`] (and not `@NoFallback`):
//! - `t = Borrow(p)` / `BorrowMut(p)` → `t_payload = Load(p); t = GenInsert(t_payload)`
//! - `t2 = *t` when `t` is a gen-ref temp → `t2 = GenGet(t)`
//!
//! The runtime ABI remains integer-only, but propagation is CFG-complete: the
//! loan graph is the source of truth for aliases and block parameters.

use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTemp, GenArenaDomain, TempId,
};
use crate::borrow_facts::analyze_borrow_facts;
use crate::escape_analysis::{EscapeCheckOptions, EscapeKind, find_escapes};
use crate::ops::UnaryOp;
use crate::types::{ArType, Primitive};
use arandu_lexer::Span;
use rustc_hash::{FxHashMap, FxHashSet};

/// Rewrite `func` in place when escape analysis finds heap-store candidates.
pub fn apply_gen_promotion(
    func: &mut AmirFunc,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
) {
    apply_gen_promotion_impl(func, interner, opts, |ty| {
        is_supported_scalar_payload(interner, ty)
    });
}

/// Production promotion uses typeck's structural Copy proof, allowing POD
/// aggregates without teaching this MIR pass a second set of type rules.
pub fn apply_gen_promotion_with_type_info(
    func: &mut AmirFunc,
    type_info: &arandu_typeck::TypeInfo,
    opts: EscapeCheckOptions,
) {
    apply_gen_promotion_impl(func, &type_info.type_interner, opts, |ty| {
        type_info.is_copy(ty)
    });
}

fn apply_gen_promotion_impl(
    func: &mut AmirFunc,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
    is_supported_payload: impl Fn(crate::types::TypeId) -> bool,
) {
    if opts.effective_no_fallback() || func.blocks.is_empty() {
        return;
    }

    let events = find_escapes(func, interner);
    let promote_locals: FxHashSet<_> = events
        .into_iter()
        .filter(|e| e.kind == EscapeKind::HeapStore)
        .map(|e| e.place_local)
        .filter(|local| has_supported_borrow_payload(func, interner, *local, &is_supported_payload))
        // A projected escaping reference needs a first-class (owner, path)
        // representation. Promoting a copied field would be a stale snapshot,
        // while retagging the aggregate handle with the field layout would make
        // runtime validation ambiguous. Escape analysis rejects this case.
        .filter(|local| !has_projected_borrow(func, *local))
        .collect();

    if promote_locals.is_empty() {
        return;
    }

    let gen_ty = interner.intern(ArType::GenRef);
    let facts = analyze_borrow_facts(func);
    let gen_temps: FxHashSet<TempId> = facts
        .loans
        .iter()
        .filter(|loan| promote_locals.contains(&loan.place_local))
        .flat_map(|loan| loan.holder_temps.iter())
        .collect();
    let borrow_payloads: FxHashMap<TempId, crate::types::TypeId> = func
        .blocks
        .iter()
        .flat_map(|block| func.block_stmts(block.id))
        .filter_map(|stmt| match stmt {
            AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
            } if promote_locals.contains(&place.local) => {
                borrow_payload_type(func, interner, *lhs).map(|ty| (*lhs, ty))
            }
            _ => None,
        })
        .collect();
    let canonical_locals: FxHashSet<_> = promote_locals
        .iter()
        .copied()
        .filter(|local| {
            let mut has_root_store = false;
            let mut all_borrows_are_root = true;
            for block in &func.blocks {
                for stmt in func.block_stmts(block.id) {
                    match stmt {
                        AmirStmt::Store { lhs, .. }
                            if lhs.local == *local && lhs.projections.is_empty() =>
                        {
                            has_root_store = true;
                        }
                        AmirStmt::Assign {
                            rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                            ..
                        } if place.local == *local && !place.projections.is_empty() => {
                            all_borrows_are_root = false;
                        }
                        _ => {}
                    }
                }
            }
            has_root_store && all_borrows_are_root
        })
        .collect();
    let canonical_payloads: FxHashMap<_, _> = canonical_locals
        .iter()
        .filter_map(|local| {
            func.locals
                .get(local.as_usize())
                .map(|info| (*local, info.ty))
        })
        .collect();

    // Retype the complete alias closure before rewriting. This includes
    // phi-like block parameters propagated by borrow facts, so loop/backedge
    // order cannot affect the result.
    for temp in &gen_temps {
        if let Some(info) = func.temps.get_mut(temp.as_usize())
            && matches!(
                interner.resolve(info.ty),
                ArType::Ref(_) | ArType::RefMut(_)
            )
        {
            info.ty = gen_ty;
            info.is_copy = true;
        }
    }
    for block in &func.blocks {
        for param in &mut func.block_params[block.params.as_range()] {
            if gen_temps.contains(&param.id)
                && matches!(
                    interner.resolve(param.ty),
                    ArType::Ref(_) | ArType::RefMut(_)
                )
            {
                param.ty = gen_ty;
            }
        }
    }
    for local in &canonical_locals {
        if let Some(info) = func.locals.get_mut(local.as_usize()) {
            info.ty = gen_ty;
        }
    }

    // Collect rewrites as (block_idx, stmt_index_in_block) operations.
    // We rebuild each block's statement range after expansion.

    for bi in 0..func.blocks.len() {
        let bid = func.blocks[bi].id;
        let old_ids: Vec<_> = func.block_stmt_ids(bid).collect();
        if old_ids.is_empty() && (bi != 0 || canonical_locals.is_empty()) {
            continue;
        }

        let mut new_stmts = Vec::new();
        if bi == 0 {
            let mut ordered_locals: Vec<_> = canonical_locals.iter().copied().collect();
            ordered_locals.sort_by_key(|local| local.as_usize());
            for local in ordered_locals {
                new_stmts.push(AmirStmt::Store {
                    lhs: crate::amir::AmirPlace {
                        local,
                        projections: Default::default(),
                    },
                    rhs: AmirOperand::Constant(AmirConstant::Nil),
                });
            }
        }
        for sid in old_ids {
            let Some(stmt) = func.stmts.get(sid).cloned() else {
                continue;
            };
            let was_mut_borrow = matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::BorrowMut(_),
                    ..
                }
            );
            match stmt {
                AmirStmt::Store { lhs, rhs }
                    if lhs.projections.is_empty() && canonical_locals.contains(&lhs.local) =>
                {
                    let local = lhs.local;
                    let Some(payload_ty) = canonical_payloads.get(&local).copied() else {
                        new_stmts.push(AmirStmt::Store { lhs, rhs });
                        continue;
                    };
                    let span = place_span(func, local);
                    let old_handle = alloc_temp(func, gen_ty, span);
                    let new_handle = alloc_temp(func, gen_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: old_handle,
                        rhs: AmirRvalue::Load(lhs.clone()),
                    });
                    new_stmts.push(AmirStmt::Assign {
                        lhs: new_handle,
                        rhs: AmirRvalue::GenUpsert {
                            gen_ref: AmirOperand::Copy(old_handle),
                            value: rhs,
                            payload_ty,
                            arena: GenArenaDomain::CompilerManaged,
                            origin: span,
                        },
                    });
                    new_stmts.push(AmirStmt::Store {
                        lhs,
                        rhs: AmirOperand::Copy(new_handle),
                    });
                }
                AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Load(place),
                } if place.projections.is_empty() && canonical_locals.contains(&place.local) => {
                    let local = place.local;
                    let Some(payload_ty) = canonical_payloads.get(&local).copied() else {
                        new_stmts.push(AmirStmt::Assign {
                            lhs,
                            rhs: AmirRvalue::Load(place),
                        });
                        continue;
                    };
                    let span = func.temps[lhs.as_usize()].span;
                    let handle = alloc_temp(func, gen_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: handle,
                        rhs: AmirRvalue::Load(place),
                    });
                    new_stmts.push(AmirStmt::Assign {
                        lhs,
                        rhs: AmirRvalue::GenGet {
                            gen_ref: AmirOperand::Copy(handle),
                            payload_ty,
                            arena: GenArenaDomain::CompilerManaged,
                            origin: span,
                        },
                    });
                }
                AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } if place.projections.is_empty() && canonical_locals.contains(&place.local) => {
                    new_stmts.push(AmirStmt::Assign {
                        lhs,
                        rhs: AmirRvalue::Load(place),
                    });
                }
                AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } if promote_locals.contains(&place.local)
                    && !canonical_locals.contains(&place.local) =>
                {
                    let place = place.clone();
                    let local = place.local;
                    let Some(payload_ty) = borrow_payloads.get(&lhs).copied() else {
                        new_stmts.push(AmirStmt::Assign {
                            lhs,
                            rhs: if was_mut_borrow {
                                AmirRvalue::BorrowMut(place)
                            } else {
                                AmirRvalue::Borrow(place)
                            },
                        });
                        continue;
                    };
                    let span = func
                        .temps
                        .get(lhs.as_usize())
                        .map_or_else(|| place_span(func, local), |temp| temp.span);
                    let payload_temp = alloc_temp(func, payload_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: payload_temp,
                        rhs: AmirRvalue::Load(place),
                    });
                    new_stmts.push(AmirStmt::Assign {
                        lhs,
                        rhs: AmirRvalue::GenInsert {
                            value: AmirOperand::Copy(payload_temp),
                            payload_ty,
                            arena: GenArenaDomain::CompilerManaged,
                            origin: span,
                        },
                    });
                }
                AmirStmt::Assign {
                    lhs,
                    rhs:
                        AmirRvalue::Unary {
                            op: UnaryOp::Deref,
                            operand: AmirOperand::Copy(src) | AmirOperand::Move(src),
                        },
                } if gen_temps.contains(&src) => {
                    new_stmts.push(AmirStmt::Assign {
                        lhs,
                        rhs: AmirRvalue::GenGet {
                            gen_ref: AmirOperand::Copy(src),
                            payload_ty: func.temps[lhs.as_usize()].ty,
                            arena: GenArenaDomain::CompilerManaged,
                            origin: func.temps[lhs.as_usize()].span,
                        },
                    });
                }
                other => new_stmts.push(other),
            }
        }

        // Replace block statement range with newly pushed stmts.
        let start = func.stmts.len();
        for s in new_stmts {
            func.stmts.push(s);
        }
        let len = func.stmts.len() - start;
        func.blocks[bi].statements = crate::layout::DenseRange::new(start, len);
    }
}

fn has_projected_borrow(func: &AmirFunc, local: crate::amir::LocalId) -> bool {
    func.blocks
        .iter()
        .flat_map(|block| func.block_stmts(block.id))
        .any(|stmt| {
            matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                    ..
                } if place.local == local && !place.projections.is_empty()
            )
        })
}

fn has_supported_borrow_payload(
    func: &AmirFunc,
    interner: &crate::types::TypeInterner,
    local: crate::amir::LocalId,
    is_supported_payload: &impl Fn(crate::types::TypeId) -> bool,
) -> bool {
    func.blocks
        .iter()
        .flat_map(|block| func.block_stmts(block.id))
        .any(|stmt| match stmt {
            AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
            } if place.local == local => {
                borrow_payload_type(func, interner, *lhs).is_some_and(is_supported_payload)
            }
            _ => false,
        })
}

fn borrow_payload_type(
    func: &AmirFunc,
    interner: &crate::types::TypeInterner,
    holder: TempId,
) -> Option<crate::types::TypeId> {
    let holder_ty = func.temps.get(holder.as_usize())?.ty;
    match interner.resolve(holder_ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => Some(inner),
        _ => None,
    }
}

fn is_supported_scalar_payload(
    interner: &crate::types::TypeInterner,
    payload_ty: crate::types::TypeId,
) -> bool {
    matches!(interner.resolve(payload_ty), ArType::IntLiteral)
        || matches!(
            interner.resolve(payload_ty),
            ArType::Primitive(p)
                if p.is_integer() || matches!(p, Primitive::Int | Primitive::Uint)
        )
}

fn place_span(func: &AmirFunc, local: crate::amir::LocalId) -> Span {
    func.locals
        .get(local.as_usize())
        .map(|l| l.span)
        .unwrap_or_else(|| Span::new(0, 0, 0))
}

fn alloc_temp(func: &mut AmirFunc, ty: crate::types::TypeId, span: Span) -> TempId {
    let id = TempId::from_usize(func.temps.len());
    let is_copy = true;
    func.temps.push(AmirTemp {
        id,
        ty,
        is_copy,
        is_nullable: false,
        span,
    });
    id
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::SymbolId;
    use crate::amir::{
        AmirBasicBlock, AmirConstant, AmirLocal, AmirPlace, AmirProjection, AmirStmtTable,
        AmirTerminator, BlockId, BlockParam, LocalId,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::literal_pool::LiteralId;
    use crate::types::{Primitive, TypeInterner};

    fn temp(id: usize) -> TempId {
        TempId::from_usize(id)
    }

    fn local(id: usize) -> LocalId {
        LocalId::from_usize(id)
    }

    fn amir_temp(id: usize, ty: crate::types::TypeId) -> AmirTemp {
        let offset = u32::try_from(id).unwrap();
        AmirTemp {
            id: temp(id),
            ty,
            is_copy: true,
            is_nullable: false,
            span: Span::new(0, offset, offset + 1),
        }
    }

    fn param(id: usize, local_id: usize, ty: crate::types::TypeId) -> BlockParam {
        BlockParam {
            id: temp(id),
            local: local(local_id),
            ty,
            from: None,
            moved: false,
        }
    }

    #[test]
    fn promotion_follows_aliases_diamond_join_and_backedge() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let bool_ty = interner.intern(ArType::Primitive(Primitive::Bool));
        let ref_ty = interner.intern(ArType::Ref(int_ty));
        let gen_ty = interner.intern(ArType::GenRef);
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: temp(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: local(0),
                projections: Default::default(),
            }),
        });
        stmts.push(AmirStmt::Assign {
            lhs: temp(4),
            rhs: AmirRvalue::Use(AmirOperand::Copy(temp(2))),
        });
        stmts.push(AmirStmt::Assign {
            lhs: temp(6),
            rhs: AmirRvalue::Unary {
                op: UnaryOp::Deref,
                operand: AmirOperand::Copy(temp(5)),
            },
        });
        stmts.push(AmirStmt::Assign {
            lhs: temp(7),
            rhs: AmirRvalue::Unary {
                op: UnaryOp::Deref,
                operand: AmirOperand::Copy(temp(5)),
            },
        });
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: local(1),
                projections: Default::default(),
            },
            rhs: AmirOperand::Copy(temp(5)),
        });

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::new(0, 1),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(temp(0)),
                    if_true: BlockId::from_usize(1),
                    true_args: vec![AmirOperand::Copy(temp(1))],
                    if_false: BlockId::from_usize(2),
                    false_args: vec![AmirOperand::Copy(temp(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::new(0, 1),
                statements: DenseRange::new(1, 1),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(temp(4))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::new(1, 1),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(temp(3))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::new(2, 1),
                statements: DenseRange::new(2, 3),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(temp(5))],
                },
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int_ty,
            receiver: None,
            params: Vec::new(),
            locals: vec![
                AmirLocal {
                    id: local(0),
                    symbol: None,
                    ty: int_ty,
                    is_memory: true,
                    span: Span::new(0, 10, 11),
                    use_span: None,
                },
                AmirLocal {
                    id: local(1),
                    symbol: None,
                    ty: int_ty,
                    is_memory: true,
                    span: Span::new(0, 12, 13),
                    use_span: None,
                },
            ],
            temps: vec![
                amir_temp(0, bool_ty),
                amir_temp(1, ref_ty),
                amir_temp(2, ref_ty),
                amir_temp(3, ref_ty),
                amir_temp(4, ref_ty),
                amir_temp(5, ref_ty),
                amir_temp(6, int_ty),
                amir_temp(7, int_ty),
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        func.block_params = vec![
            param(2, 0, ref_ty),
            param(3, 0, ref_ty),
            param(5, 0, ref_ty),
        ];

        let mut strict_func = func.clone();
        apply_gen_promotion(
            &mut strict_func,
            &interner,
            EscapeCheckOptions {
                no_fallback: true,
                ..EscapeCheckOptions::default()
            },
        );
        assert_eq!(strict_func.temps[1].ty, ref_ty);
        assert!(
            strict_func
                .blocks
                .iter()
                .all(
                    |block| strict_func.block_stmts(block.id).all(|stmt| !matches!(
                        stmt,
                        AmirStmt::Assign {
                            rhs: AmirRvalue::GenInsert { .. } | AmirRvalue::GenGet { .. },
                            ..
                        }
                    ))
                )
        );

        apply_gen_promotion(&mut func, &interner, EscapeCheckOptions::default());

        for holder in 1..=5 {
            assert_eq!(func.temps[holder].ty, gen_ty, "holder _{holder}");
        }
        assert_eq!(func.block_params(func.blocks[1].params)[0].ty, gen_ty);
        assert_eq!(func.block_params(func.blocks[2].params)[0].ty, gen_ty);
        assert_eq!(func.block_params(func.blocks[3].params)[0].ty, gen_ty);
        assert!(
            func.block_stmts(BlockId::from_usize(0))
                .any(|stmt| matches!(
                    stmt,
                    AmirStmt::Assign {
                        rhs: AmirRvalue::GenInsert { payload_ty, .. },
                        ..
                    } if *payload_ty == int_ty
                ))
        );
        assert_eq!(
            func.block_stmts(BlockId::from_usize(3))
                .filter(|stmt| matches!(
                    stmt,
                    AmirStmt::Assign {
                        rhs: AmirRvalue::GenGet { payload_ty, .. },
                        ..
                    } if *payload_ty == int_ty
                ))
                .count(),
            2
        );
    }

    #[test]
    fn projected_escape_is_not_lowered_to_a_stale_snapshot() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let array_ty = interner.intern(ArType::Array(4, int_ty));
        let ref_ty = interner.intern(ArType::Ref(int_ty));
        let index = AmirOperand::Constant(AmirConstant::Pool(LiteralId(0)));
        let projected_place = AmirPlace {
            local: local(0),
            projections: smallvec::smallvec![AmirProjection::Index(index)],
        };
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: temp(0),
            rhs: AmirRvalue::Borrow(projected_place),
        });
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: local(1),
                projections: Default::default(),
            },
            rhs: AmirOperand::Copy(temp(0)),
        });

        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 2),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int_ty,
            receiver: None,
            params: Vec::new(),
            locals: vec![
                AmirLocal {
                    id: local(0),
                    symbol: None,
                    ty: array_ty,
                    is_memory: true,
                    span: Span::new(0, 10, 14),
                    use_span: None,
                },
                AmirLocal {
                    id: local(1),
                    symbol: None,
                    ty: array_ty,
                    is_memory: true,
                    span: Span::new(0, 20, 21),
                    use_span: None,
                },
            ],
            temps: vec![amir_temp(0, ref_ty)],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        apply_gen_promotion(&mut func, &interner, EscapeCheckOptions::default());

        assert_eq!(func.temps[0].ty, ref_ty);
        assert!(
            func.block_stmts(BlockId::from_usize(0))
                .all(|stmt| !matches!(
                    stmt,
                    AmirStmt::Assign {
                        rhs: AmirRvalue::GenInsert { .. } | AmirRvalue::GenGet { .. },
                        ..
                    }
                ))
        );
    }

    #[test]
    fn root_owner_is_heap_lifted_once_and_mutations_preserve_alias_identity() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_ty = interner.intern(ArType::Ref(int_ty));
        let gen_ty = interner.intern(ArType::GenRef);
        let root = AmirPlace {
            local: local(0),
            projections: Default::default(),
        };
        let sink = AmirPlace {
            local: local(1),
            projections: Default::default(),
        };
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Store {
            lhs: root.clone(),
            rhs: AmirOperand::Constant(AmirConstant::Pool(LiteralId(0))),
        });
        stmts.push(AmirStmt::Assign {
            lhs: temp(0),
            rhs: AmirRvalue::Borrow(root.clone()),
        });
        stmts.push(AmirStmt::Store {
            lhs: sink,
            rhs: AmirOperand::Copy(temp(0)),
        });
        stmts.push(AmirStmt::Store {
            lhs: root.clone(),
            rhs: AmirOperand::Constant(AmirConstant::Pool(LiteralId(1))),
        });
        stmts.push(AmirStmt::Assign {
            lhs: temp(1),
            rhs: AmirRvalue::Borrow(root),
        });
        for (holder, result) in [(temp(0), temp(2)), (temp(1), temp(3))] {
            stmts.push(AmirStmt::Assign {
                lhs: result,
                rhs: AmirRvalue::Unary {
                    op: UnaryOp::Deref,
                    operand: AmirOperand::Copy(holder),
                },
            });
        }
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 7),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int_ty,
            receiver: None,
            params: Vec::new(),
            locals: vec![
                AmirLocal {
                    id: local(0),
                    symbol: None,
                    ty: int_ty,
                    is_memory: true,
                    span: Span::new(0, 1, 2),
                    use_span: None,
                },
                AmirLocal {
                    id: local(1),
                    symbol: None,
                    ty: int_ty,
                    is_memory: true,
                    span: Span::new(0, 3, 4),
                    use_span: None,
                },
            ],
            temps: vec![
                amir_temp(0, ref_ty),
                amir_temp(1, ref_ty),
                amir_temp(2, int_ty),
                amir_temp(3, int_ty),
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        apply_gen_promotion(&mut func, &interner, EscapeCheckOptions::default());

        assert_eq!(func.locals[0].ty, gen_ty);
        assert_eq!(func.temps[0].ty, gen_ty);
        assert_eq!(func.temps[1].ty, gen_ty);
        let rewritten: Vec<_> = func.block_stmts(BlockId::from_usize(0)).collect();
        assert_eq!(
            rewritten
                .iter()
                .filter(|stmt| matches!(
                    stmt,
                    AmirStmt::Assign {
                        rhs: AmirRvalue::GenUpsert { .. },
                        ..
                    }
                ))
                .count(),
            2
        );
        assert_eq!(
            rewritten
                .iter()
                .filter(|stmt| matches!(
                    stmt,
                    AmirStmt::Assign {
                        rhs: AmirRvalue::GenGet { .. },
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(!rewritten.iter().any(|stmt| matches!(
            stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Borrow(_)
                    | AmirRvalue::BorrowMut(_)
                    | AmirRvalue::GenInsert { .. },
                ..
            }
        )));
    }
}
