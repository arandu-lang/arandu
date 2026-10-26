use super::dynamic::expand_dynamic_enum_drops;
use super::needs_drop::{
    emit_recursive_drops, find_owned_string_locals, projected_place_type, type_needs_drop,
};
use crate::amir::{
    AmirFunc, AmirPlace, AmirProjection, AmirStmt, AmirStmtTable, AmirTerminator, BlockId, LocalId,
};
use crate::move_checker::move_states_at_block_exit;
use arandu_middle::layout::DenseRange;
use arandu_middle::type_context::AmirTypeContext;
use smallvec::SmallVec;

/// Insert exactly-once root-local and nested cascade destruction before normal function returns.
pub fn elaborate_drops(func: &mut AmirFunc, type_info: &dyn AmirTypeContext) {
    let owned_string_locals = find_owned_string_locals(func);
    let is_destructor_func = type_info.is_destructor_instance(func.symbol);

    let initialized = crate::definite_init::initialized_at_block_exit(func);
    let initialized_before = crate::definite_init::initialized_before_statements(func);
    let moved = move_states_at_block_exit(func);
    let moved_before = crate::move_checker::move_states_before_statements(func);
    // A merge can make a payload move look conditional even when the incoming
    // paths are disjoint enum variants (for example `Ok(value)` vs `Err(_)`).
    // A root `Destroy` after that merge would recursively drop the already-moved
    // payload. When the merge is immediately followed by `StorageDead`, move
    // cleanup back onto its unconditional incoming goto edges, where each path's
    // ownership state is precise.
    let mut edge_drops = rustc_hash::FxHashMap::<BlockId, Vec<LocalId>>::default();
    let mut edge_cleaned = rustc_hash::FxHashSet::<(BlockId, LocalId)>::default();
    for block in &func.blocks {
        for (stmt_index, stmt) in func.block_stmts(block.id).enumerate() {
            let AmirStmt::StorageDead(local_id) = stmt else {
                continue;
            };
            let root = AmirPlace {
                local: *local_id,
                projections: SmallVec::new(),
            };
            let local = &func.locals[local_id.as_usize()];
            if !type_needs_drop(local.ty, type_info)
                || !moved_before[block.id.as_usize()][stmt_index].has_maybe_moved_descendant(&root)
            {
                continue;
            }
            let prefix_is_lifecycle_only =
                func.block_stmts(block.id).take(stmt_index).all(|stmt| {
                    matches!(
                        stmt,
                        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop
                    )
                });
            if !prefix_is_lifecycle_only {
                continue;
            }
            let predecessors = func.predecessors(block.id);
            if predecessors.is_empty()
                || predecessors.iter().any(|pred| {
                    !matches!(
                        func.block(*pred).terminator,
                        AmirTerminator::Goto { target, .. } if target == block.id
                    )
                })
            {
                continue;
            }
            for &pred in predecessors {
                edge_drops.entry(pred).or_default().push(*local_id);
                edge_cleaned.insert((block.id, *local_id));
            }
        }
    }
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut rebuilt = AmirStmtTable::new();
    let mut ranges = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let start = rebuilt.len();
        for (stmt_index, id) in block
            .statements
            .iter_ids::<crate::amir::InstrId>()
            .enumerate()
        {
            let stmt = &old.payloads[id];
            if let AmirStmt::Store { lhs, .. } = stmt
                && initialized_before[block.id.as_usize()][stmt_index].contains(lhs.local)
                && !(is_destructor_func && lhs.local.as_usize() == 0)
                && let Some(place_ty) = projected_place_type(lhs, func, type_info)
                && (type_needs_drop(place_ty, type_info)
                    || (lhs.projections.is_empty() && owned_string_locals.contains(&lhs.local)))
                && moved_before[block.id.as_usize()][stmt_index].place_itself_is_available(lhs)
            {
                if lhs.projections.is_empty() && owned_string_locals.contains(&lhs.local) {
                    rebuilt.push(AmirStmt::Destroy(lhs.clone()));
                } else {
                    emit_recursive_drops(
                        lhs,
                        place_ty,
                        type_info,
                        &moved_before[block.id.as_usize()][stmt_index],
                        false,
                        &mut rebuilt,
                    );
                }
            }
            if let AmirStmt::StorageDead(local_id) = stmt {
                let local = &func.locals[local_id.as_usize()];
                let root = AmirPlace {
                    local: *local_id,
                    projections: SmallVec::new(),
                };
                let is_owned_string = owned_string_locals.contains(local_id);
                if !edge_cleaned.contains(&(block.id, *local_id))
                    && !(is_destructor_func && local_id.as_usize() == 0)
                    && (type_needs_drop(local.ty, type_info) || is_owned_string)
                    && initialized_before[block.id.as_usize()][stmt_index].contains(*local_id)
                {
                    let move_state = &moved_before[block.id.as_usize()][stmt_index];
                    if is_owned_string {
                        if move_state.place_itself_is_available(&root) {
                            rebuilt.push(AmirStmt::Destroy(root));
                        }
                    } else {
                        emit_recursive_drops(
                            &root,
                            local.ty,
                            type_info,
                            move_state,
                            false,
                            &mut rebuilt,
                        );
                    }
                }
            }
            rebuilt.push(stmt.clone());
        }
        if let Some(locals) = edge_drops.get(&block.id) {
            for local_id in locals {
                let local = &func.locals[local_id.as_usize()];
                let root = AmirPlace {
                    local: *local_id,
                    projections: SmallVec::new(),
                };
                emit_recursive_drops(
                    &root,
                    local.ty,
                    type_info,
                    &moved[block.id.as_usize()],
                    false,
                    &mut rebuilt,
                );
            }
        }
        if matches!(block.terminator, AmirTerminator::Return) {
            let move_state = &moved[block.id.as_usize()];
            for local in func.locals.iter().rev() {
                let skip_self = is_destructor_func && local.id.as_usize() == 0;
                let is_owned_string = owned_string_locals.contains(&local.id);
                if (type_needs_drop(local.ty, type_info) || is_owned_string)
                    && initialized[block.id.as_usize()].contains(local.id)
                {
                    let root_place = AmirPlace {
                        local: LocalId::from_usize(local.id.as_usize()),
                        projections: SmallVec::new(),
                    };
                    if is_owned_string {
                        if move_state.place_itself_is_available(&root_place) {
                            rebuilt.push(AmirStmt::Destroy(root_place));
                        }
                    } else {
                        emit_recursive_drops(
                            &root_place,
                            local.ty,
                            type_info,
                            move_state,
                            skip_self,
                            &mut rebuilt,
                        );
                    }
                }
            }
        }
        ranges.push(DenseRange::new(start, rebuilt.len() - start));
    }

    func.stmts = rebuilt;
    for (block, range) in func.blocks.iter_mut().zip(ranges) {
        block.statements = range;
    }

    expand_indirect_drops(func, type_info);
    expand_dynamic_enum_drops(func, type_info);
}

/// Canonicalize unsafe `dropInPlace` operations before backends see the AMIR.
/// The intrinsic is lowered to `Destroy(*pointer)`; this pass recursively
/// decomposes that place just like lexical cleanup, so emitters only need to
/// implement atomic leaf destruction and nominal destructor calls.
fn expand_indirect_drops(func: &mut AmirFunc, type_info: &dyn AmirTypeContext) {
    let moved_before = crate::move_checker::move_states_before_statements(func);
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut rebuilt = AmirStmtTable::new();
    let mut ranges = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let start = rebuilt.len();
        for (stmt_index, id) in block
            .statements
            .iter_ids::<crate::amir::InstrId>()
            .enumerate()
        {
            let stmt = &old.payloads[id];
            if let AmirStmt::Destroy(place) = stmt
                && place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, AmirProjection::Deref))
                && let Some(ty) = projected_place_type(place, func, type_info)
            {
                let move_state = &moved_before[block.id.as_usize()][stmt_index];
                if type_needs_drop(ty, type_info) {
                    emit_recursive_drops(place, ty, type_info, move_state, false, &mut rebuilt);
                }
            } else {
                rebuilt.push(stmt.clone());
            }
        }
        ranges.push(DenseRange::new(start, rebuilt.len() - start));
    }

    func.stmts = rebuilt;
    for (block, range) in func.blocks.iter_mut().zip(ranges) {
        block.statements = range;
    }
}
