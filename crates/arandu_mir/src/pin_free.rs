//! A3.4 — pin-free self-references via [`crate::amir::LocalId`] indices.
//!
//! Absolute pointers into a coroutine frame break if the state blob moves
//! (stack → heap). This pass rewrites borrows that **cross a `Suspend`** into
//! [`AmirRvalue::RelativeBorrow`] (value = dense local index) and rewrites
//! `*p` of those temps into [`AmirRvalue::Load`] of the local place — so the
//! load always goes through the current home of the local, never a stale addr.
//!
//! Refs that escape as call args (or other non-deref uses) stay absolute and
//! are still rejected by [`crate::suspend_check`] (O010).

use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator, TempId,
};
use crate::liveness::analyze_temp_liveness;
use crate::ops::UnaryOp;
use crate::types::{ArType, TypeInterner};
use rustc_hash::{FxHashMap, FxHashSet};

/// Rewrite absolute borrows that cross suspension into LocalId-relative form.
pub fn apply_pin_free_refs(func: &mut AmirFunc, interner: &TypeInterner) {
    if func.blocks.is_empty() {
        return;
    }
    let has_suspend = func
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, AmirTerminator::Suspend { .. }));
    if !has_suspend {
        return;
    }

    let temp_live = analyze_temp_liveness(func);

    // Temps that are Ref/RefMut and live into at least one resume block.
    let mut cross_suspend_refs: FxHashSet<TempId> = FxHashSet::default();
    for block in &func.blocks {
        let AmirTerminator::Suspend { resume, .. } = &block.terminator else {
            continue;
        };
        let live_in = temp_live.live_in(*resume);
        for t in 0..func.temps.len() {
            let tid = TempId::from_usize(t);
            if !live_in.contains(tid) {
                continue;
            }
            let Some(temp) = func.temps.get(t) else {
                continue;
            };
            let ty = interner.resolve(temp.ty);
            if matches!(ty, ArType::Ref(_) | ArType::RefMut(_)) {
                cross_suspend_refs.insert(tid);
            }
        }
    }
    if cross_suspend_refs.is_empty() {
        return;
    }

    // Defining Borrow/BorrowMut of a local or constant-offset field projection.
    let mut relative_of: FxHashMap<TempId, (AmirPlace, bool)> = FxHashMap::default();
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            let AmirStmt::Assign { lhs, rhs } = stmt else {
                continue;
            };
            if !cross_suspend_refs.contains(lhs) {
                continue;
            }
            match rhs {
                AmirRvalue::Borrow(place) if is_constant_field_projection(place) => {
                    relative_of.insert(*lhs, (place.clone(), false));
                }
                AmirRvalue::BorrowMut(place) if is_constant_field_projection(place) => {
                    relative_of.insert(*lhs, (place.clone(), true));
                }
                _ => {}
            }
        }
    }

    // Refs used as call args (or store RHS of ref itself) cannot be relative.
    let mut escapes: FxHashSet<TempId> = FxHashSet::default();
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Call { args, callee, .. } => {
                    note_escape_ops(callee, &relative_of, &mut escapes);
                    for a in args {
                        note_escape_ops(a, &relative_of, &mut escapes);
                    }
                }
                AmirStmt::Store { rhs, .. } => {
                    note_escape_ops(rhs, &relative_of, &mut escapes);
                }
                AmirStmt::Assign { rhs, .. } => match rhs {
                    // Deref is rewritten; Use of bare local ref copies the index.
                    AmirRvalue::Unary {
                        op: UnaryOp::Deref, ..
                    } => {}
                    AmirRvalue::Use(op) => {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
                            && relative_of
                                .get(t)
                                .is_some_and(|(place, _)| !place.projections.is_empty())
                        {
                            escapes.insert(*t);
                        }
                    }
                    AmirRvalue::Borrow(_)
                    | AmirRvalue::BorrowMut(_)
                    | AmirRvalue::RelativeBorrow { .. }
                    | AmirRvalue::Load(_) => {}
                    other => {
                        // Any other rvalue that mentions the ref temp escapes.
                        scan_rvalue_ops(other, &relative_of, &mut escapes);
                    }
                },
                AmirStmt::Free(op) => note_escape_ops(op, &relative_of, &mut escapes),
                _ => {}
            }
        }
    }
    for t in escapes {
        relative_of.remove(&t);
    }
    if relative_of.is_empty() {
        return;
    }

    // Rewrite statements.
    let block_ids: Vec<_> = func.blocks.iter().map(|b| b.id).collect();
    for bid in block_ids {
        let stmt_ids: Vec<_> = func.block_stmt_ids(bid).collect();
        for sid in stmt_ids {
            let Some(stmt) = func.stmts.get_mut(sid) else {
                continue;
            };
            let AmirStmt::Assign { lhs, rhs } = stmt else {
                continue;
            };
            if let Some((place, mutable)) = relative_of.get(lhs)
                && matches!(rhs, AmirRvalue::Borrow(_) | AmirRvalue::BorrowMut(_))
            {
                *rhs = AmirRvalue::RelativeBorrow {
                    local: place.local,
                    mutable: *mutable,
                };
                continue;
            }
            if let AmirRvalue::Unary {
                op: UnaryOp::Deref,
                operand,
            } = rhs
            {
                let t = match operand {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => *t,
                    _ => continue,
                };
                if let Some((place, _)) = relative_of.get(&t) {
                    *rhs = AmirRvalue::Load(place.clone());
                }
            }
        }
    }
}

fn is_constant_field_projection(place: &AmirPlace) -> bool {
    place
        .projections
        .iter()
        .all(|p| matches!(p, AmirProjection::Field(_) | AmirProjection::TupleField(_)))
}

fn note_escape_ops(
    op: &AmirOperand,
    relative_of: &FxHashMap<TempId, (AmirPlace, bool)>,
    escapes: &mut FxHashSet<TempId>,
) {
    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
        && relative_of.contains_key(t)
    {
        escapes.insert(*t);
    }
}

fn scan_rvalue_ops(
    rhs: &AmirRvalue,
    relative_of: &FxHashMap<TempId, (AmirPlace, bool)>,
    escapes: &mut FxHashSet<TempId>,
) {
    use crate::amir::for_each_rvalue_operand;
    for_each_rvalue_operand(rhs, |op| note_escape_ops(op, relative_of, escapes));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{AmirBasicBlock, AmirLocal, AmirStmtTable, AmirTemp, BlockId, LocalId};
    use crate::layout::DenseRange;
    use crate::types::{ArType, Primitive};
    use smallvec::smallvec;

    #[test]
    fn rewrites_constant_field_projection_borrow_across_suspend() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let coroutine = interner.intern(ArType::Coroutine(int));
        let field_sym = crate::SymbolId::new(0, 7);
        let projected = AmirPlace {
            local: LocalId::from_usize(0),
            projections: smallvec![AmirProjection::Field(field_sym)],
        };

        let mut stmts = AmirStmtTable::new();
        let s_borrow = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Borrow(projected.clone()),
        });
        let s_deref = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Unary {
                op: UnaryOp::Deref,
                operand: AmirOperand::Copy(TempId::from_usize(0)),
            },
        });
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::new(0, 1),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Suspend {
                    future: AmirOperand::Copy(TempId::from_usize(2)),
                    resume: BlockId::from_usize(1),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::new(1, 1),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: int,
            receiver: None,
            params: vec![],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: crate::Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: crate::Span::new(0, 1, 2),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: int,
                    is_copy: true,
                    is_nullable: false,
                    span: crate::Span::new(0, 2, 3),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: coroutine,
                    is_copy: false,
                    is_nullable: false,
                    span: crate::Span::new(0, 3, 4),
                },
            ],
            blocks,
            block_params: vec![],
            stmts,
            cfg,
        };

        apply_pin_free_refs(&mut func, &interner);

        assert!(matches!(
            &func.stmts.get(s_borrow),
            Some(AmirStmt::Assign {
                rhs: AmirRvalue::RelativeBorrow {
                    local,
                    mutable: false
                },
                ..
            }) if *local == LocalId::from_usize(0)
        ));
        assert!(matches!(
            &func.stmts.get(s_deref),
            Some(AmirStmt::Assign {
                rhs: AmirRvalue::Load(place),
                ..
            }) if *place == projected
        ));
    }
}
