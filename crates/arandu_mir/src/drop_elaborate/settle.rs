//! Conditional moves.
//!
//! A value moved on one path and not on another (`if c { consume(s) }`, a
//! match arm that binds a payload beside one that ignores it) reaches the
//! join neither owned nor moved. Cleanup after the join cannot destroy it:
//! it may be gone. So each incoming edge on which the value is still owned
//! destroys it there, and the join sees it moved on every path. This is the
//! static form of the drop flags of rustc's drop elaboration; it is sound
//! because a value that may have been moved cannot be used after the join
//! (O007).
//!
//! A payload reached through an enum variant is only there when that variant
//! is the active one, so its destruction is guarded by the discriminant.

use super::needs_drop::{emit_recursive_drops, projected_place_type, type_needs_drop};
use crate::Span;
use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt,
    AmirStmtTable, AmirTemp, AmirTerminator, BlockId, TempId,
};
use crate::move_checker::MoveState;
use arandu_middle::layout::DenseRange;
use arandu_middle::type_context::AmirTypeContext;
use arandu_middle::types::ArType;
use smallvec::SmallVec;

struct Block {
    params: DenseRange,
    statements: Vec<AmirStmt>,
    terminator: AmirTerminator,
}

/// Destroys, on each edge into a join, the values other edges moved.
pub(super) fn settle_conditional_moves(func: &mut AmirFunc, type_info: &dyn AmirTypeContext) {
    let has_join_block = func
        .blocks
        .iter()
        .any(|b| func.predecessors(b.id).len() >= 2);
    if !has_join_block || !func.locals.iter().any(|l| type_needs_drop(l.ty, type_info)) {
        return;
    }

    let (entry, exit) = crate::move_checker::move_states_entry_and_exit(func);
    let initialized = crate::definite_init::initialized_at_block_exit(func);
    let is_destructor = type_info.is_destructor_instance(func.symbol);

    // (predecessor, join) -> the places to destroy on that edge.
    let mut edges: Vec<(BlockId, BlockId, Vec<AmirPlace>)> = Vec::new();
    for block in &func.blocks {
        let predecessors = func.predecessors(block.id);
        if predecessors.len() < 2 {
            continue;
        }
        let unsettled = entry[block.id.as_usize()].maybe_moved_places();
        if unsettled.is_empty() {
            continue;
        }
        for &predecessor in predecessors {
            let owned: Vec<AmirPlace> = unsettled
                .iter()
                .filter(|place| {
                    !(is_destructor && place.local.as_usize() == 0)
                        && initialized[predecessor.as_usize()].contains(place.local)
                        && exit[predecessor.as_usize()].place_itself_is_available(place)
                        && projected_place_type(place, func, type_info)
                            .is_some_and(|ty| type_needs_drop(ty, type_info))
                })
                .cloned()
                .collect();
            if !owned.is_empty()
                && !edges
                    .iter()
                    .any(|(p, b, _)| (*p, *b) == (predecessor, block.id))
            {
                edges.push((predecessor, block.id, owned));
            }
        }
    }
    if edges.is_empty() {
        return;
    }

    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut blocks: Vec<Block> = func
        .blocks
        .iter()
        .map(|block| Block {
            params: block.params,
            statements: old
                .payloads
                .raw
                .get(block.statements.as_range())
                .unwrap_or(&[])
                .to_vec(),
            terminator: block.terminator.clone(),
        })
        .collect();

    for (predecessor, join, places) in edges {
        // The edge block takes the jump's arguments and forwards them.
        let edge = BlockId::from_usize(blocks.len());
        let Some(args) = retarget(&mut blocks[predecessor.as_usize()].terminator, join, edge)
        else {
            continue;
        };
        blocks.push(Block {
            params: DenseRange::empty(),
            statements: Vec::new(),
            terminator: AmirTerminator::Unreachable,
        });
        let mut current = edge;
        for place in places {
            current = destroy_if_present(
                func,
                type_info,
                &mut blocks,
                current,
                &place,
                &exit[predecessor.as_usize()],
            );
        }
        blocks[current.as_usize()].terminator = AmirTerminator::Goto { target: join, args };
    }

    let mut statements = AmirStmtTable::new();
    func.blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, block)| {
            let start = statements.len();
            for statement in block.statements {
                statements.push(statement);
            }
            AmirBasicBlock {
                id: BlockId::from_usize(index),
                params: block.params,
                statements: DenseRange::new(start, statements.len() - start),
                terminator: block.terminator,
            }
        })
        .collect();
    func.stmts = statements;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
}

/// Points the single edge of `terminator` into `join` at `edge` and returns
/// the arguments it carried. `None` when the terminator has no such edge,
/// more than one, or cannot be split (a suspension resumes where it says).
fn retarget(
    terminator: &mut AmirTerminator,
    join: BlockId,
    edge: BlockId,
) -> Option<Vec<AmirOperand>> {
    match terminator {
        AmirTerminator::Goto { target, args } if *target == join => {
            *target = edge;
            Some(std::mem::take(args))
        }
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => match (*if_true == join, *if_false == join) {
            (true, false) => {
                *if_true = edge;
                Some(std::mem::take(true_args))
            }
            (false, true) => {
                *if_false = edge;
                Some(std::mem::take(false_args))
            }
            _ => None,
        },
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let hits = targets
                .iter()
                .filter(|(_, target, _)| *target == join)
                .count()
                + usize::from(otherwise.0 == join);
            if hits != 1 {
                return None;
            }
            if otherwise.0 == join {
                otherwise.0 = edge;
                return Some(std::mem::take(&mut otherwise.1));
            }
            let (_, target, args) = targets.iter_mut().find(|(_, target, _)| *target == join)?;
            *target = edge;
            Some(std::mem::take(args))
        }
        AmirTerminator::Goto { .. }
        | AmirTerminator::Return
        | AmirTerminator::Suspend { .. }
        | AmirTerminator::Unreachable
        | AmirTerminator::Trap(_) => None,
    }
}

/// Emits the destruction of `place` starting in `current` and returns the
/// block where control continues. Each enum variant on the way to `place`
/// is checked first: the payload of another variant is not there.
fn destroy_if_present(
    func: &mut AmirFunc,
    type_info: &dyn AmirTypeContext,
    blocks: &mut Vec<Block>,
    current: BlockId,
    place: &AmirPlace,
    state: &MoveState,
) -> BlockId {
    let Some(ty) = projected_place_type(place, func, type_info) else {
        return current;
    };
    let mut drops = AmirStmtTable::new();
    emit_recursive_drops(place, ty, type_info, state, false, &mut drops);
    let drops = drops.payloads.raw;
    if drops.is_empty() {
        return current;
    }
    let variants: Vec<(usize, usize)> = place
        .projections
        .iter()
        .enumerate()
        .filter_map(|(index, projection)| match projection {
            AmirProjection::Variant(tag) => Some((index, *tag)),
            _ => None,
        })
        .collect();
    if variants.is_empty() {
        blocks[current.as_usize()].statements.extend(drops);
        return current;
    }

    let continuation = BlockId::from_usize(blocks.len());
    blocks.push(Block {
        params: DenseRange::empty(),
        statements: Vec::new(),
        terminator: AmirTerminator::Unreachable,
    });
    let int_ty = type_info
        .type_interner()
        .intern(ArType::Primitive(crate::types::Primitive::Int));
    let mut testing = current;
    for (index, tag) in variants {
        let owner = AmirPlace {
            local: place.local,
            projections: SmallVec::from_slice(&place.projections[..index]),
        };
        let Some(owner_ty) = projected_place_type(&owner, func, type_info) else {
            blocks[testing.as_usize()].terminator = AmirTerminator::Goto {
                target: continuation,
                args: Vec::new(),
            };
            return continuation;
        };
        let borrowed = new_temp(
            func,
            type_info.type_interner().intern(ArType::Ref(owner_ty)),
        );
        let discriminant = new_temp(func, int_ty);
        let active = BlockId::from_usize(blocks.len());
        blocks.push(Block {
            params: DenseRange::empty(),
            statements: Vec::new(),
            terminator: AmirTerminator::Unreachable,
        });
        let block = &mut blocks[testing.as_usize()];
        block.statements.push(AmirStmt::Assign {
            lhs: borrowed,
            rhs: AmirRvalue::Borrow(owner),
        });
        block.statements.push(AmirStmt::Assign {
            lhs: discriminant,
            rhs: AmirRvalue::Discriminant {
                value: AmirOperand::Copy(borrowed),
            },
        });
        block.terminator = AmirTerminator::SwitchInt {
            discriminant: AmirOperand::Copy(discriminant),
            targets: vec![(tag as i128, active, Vec::new())],
            otherwise: (continuation, Vec::new()),
        };
        testing = active;
    }
    blocks[testing.as_usize()].statements.extend(drops);
    blocks[testing.as_usize()].terminator = AmirTerminator::Goto {
        target: continuation,
        args: Vec::new(),
    };
    continuation
}

fn new_temp(func: &mut AmirFunc, ty: arandu_middle::types::TypeId) -> TempId {
    let id = TempId::from_usize(func.temps.len());
    func.temps.push(AmirTemp {
        id,
        ty,
        is_copy: true,
        is_nullable: false,
        span: Span::new(0, 0, 0),
    });
    id
}
