use super::arrays::expand_array_drop_loops;
use super::dynamic::expand_dynamic_enum_drops;
use super::flags::{apply_drop_flags, compute_may_init};
use super::needs_drop::{
    emit_recursive_drops, find_owned_string_locals, projected_place_type, type_needs_drop,
};
use crate::amir::{
    AmirFunc, AmirPlace, AmirProjection, AmirStmt, AmirStmtTable, AmirTerminator, BlockId, LocalId,
};
use crate::move_checker::{
    LocalMoveState, MoveState, move_states_exit_and_before_statements, place_is_prefix,
    tracked_move_place,
};
use arandu_middle::layout::DenseRange;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::type_context::AmirTypeContext;
use smallvec::SmallVec;

fn push_unique_place(tracked: &mut Vec<AmirPlace>, place: AmirPlace) {
    if !tracked.contains(&place) {
        tracked.push(place);
    }
}

fn record_tracked_places(
    place: &AmirPlace,
    must_here: bool,
    move_state: &MoveState,
    func: &AmirFunc,
    type_info: &dyn AmirTypeContext,
    tracked: &mut Vec<AmirPlace>,
) {
    let state = move_state.drop_fact_state(place);
    if state == LocalMoveState::Moved {
        return;
    }
    let canon = tracked_move_place(place);
    if !must_here || state == LocalMoveState::MaybeMoved {
        push_unique_place(tracked, canon.clone());
    }
    for mm in move_state.maybe_moved_places() {
        if mm.local == canon.local
            && mm != canon
            && place_is_prefix(&canon, &mm)
            && move_state.drop_fact_state(&mm) == LocalMoveState::MaybeMoved
            && projected_place_type(&mm, func, type_info)
                .is_some_and(|t| type_needs_drop(t, type_info))
        {
            push_unique_place(tracked, mm);
        }
    }
}

fn has_variant_or_payload(place: &AmirPlace) -> bool {
    place.projections.iter().any(|proj| {
        matches!(
            proj,
            AmirProjection::Variant(_) | AmirProjection::Payload { .. }
        )
    })
}

/// Insert exactly-once root-local and nested cascade destruction before overwrites,
/// scope exits (`StorageDead`), and normal function returns, guarded by runtime
/// drop flags when initialization or moves are path-dependent.
pub fn elaborate_drops(
    func: &mut AmirFunc,
    type_info: &dyn AmirTypeContext,
    literal_pool: &mut AmirLiteralPool,
) {
    let owned_string_locals = find_owned_string_locals(func);
    let is_destructor_func = type_info.is_destructor_instance(func.symbol);

    let (initialized, initialized_before) =
        crate::definite_init::initialized_exit_and_before_statements(func);
    let (may_initialized, may_initialized_before) = compute_may_init(func);
    let (moved, moved_before) = move_states_exit_and_before_statements(func);
    // A merge can make a payload move look conditional even when the incoming
    // paths are disjoint enum variants (for example `Ok(value)` vs `Err(_)`).
    // When the merge is immediately followed by `StorageDead`, move cleanup
    // back onto its unconditional incoming goto edges, where each path's
    // ownership state is precise, avoiding unnecessary drop flags.
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
                || !initialized_before[block.id.as_usize()][stmt_index].contains(*local_id)
                || moved_before[block.id.as_usize()][stmt_index].root_state(*local_id)
                    != LocalMoveState::Available
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

    let mut tracked_places = Vec::<AmirPlace>::new();
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
            let must_before =
                initialized_before[block.id.as_usize()][stmt_index].contains(match stmt {
                    AmirStmt::Store { lhs, .. } => lhs.local,
                    AmirStmt::StorageDead(local_id) => *local_id,
                    _ => LocalId::from_usize(0),
                });
            let move_before = &moved_before[block.id.as_usize()][stmt_index];

            if let AmirStmt::Store { lhs, .. } = stmt
                && may_initialized_before[block.id.as_usize()][stmt_index].contains(lhs.local)
                && !(is_destructor_func && lhs.local.as_usize() == 0)
                && let Some(place_ty) = projected_place_type(lhs, func, type_info)
                && (type_needs_drop(place_ty, type_info)
                    || (lhs.projections.is_empty() && owned_string_locals.contains(&lhs.local)))
                && move_before.drop_fact_state(lhs) != LocalMoveState::Moved
            {
                record_tracked_places(
                    lhs,
                    must_before,
                    move_before,
                    func,
                    type_info,
                    &mut tracked_places,
                );
                if lhs.projections.is_empty() && owned_string_locals.contains(&lhs.local) {
                    rebuilt.push(AmirStmt::Destroy(lhs.clone()));
                } else {
                    emit_recursive_drops(
                        lhs,
                        place_ty,
                        type_info,
                        move_before,
                        false,
                        true,
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
                    && may_initialized_before[block.id.as_usize()][stmt_index].contains(*local_id)
                    && move_before.drop_fact_state(&root) != LocalMoveState::Moved
                {
                    record_tracked_places(
                        &root,
                        must_before,
                        move_before,
                        func,
                        type_info,
                        &mut tracked_places,
                    );
                    if is_owned_string {
                        rebuilt.push(AmirStmt::Destroy(root));
                    } else {
                        emit_recursive_drops(
                            &root,
                            local.ty,
                            type_info,
                            move_before,
                            false,
                            true,
                            &mut rebuilt,
                        );
                    }
                }
            }
            rebuilt.push(stmt.clone());
        }
        if let Some(locals) = edge_drops.get(&block.id) {
            let move_exit = &moved[block.id.as_usize()];
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
                    move_exit,
                    false,
                    true,
                    &mut rebuilt,
                );
            }
        }
        if matches!(block.terminator, AmirTerminator::Return) {
            let move_state = &moved[block.id.as_usize()];
            for local in func.locals.iter().rev() {
                let skip_self = is_destructor_func && local.id.as_usize() == 0;
                let is_owned_string = owned_string_locals.contains(&local.id);
                let root_place = AmirPlace {
                    local: LocalId::from_usize(local.id.as_usize()),
                    projections: SmallVec::new(),
                };
                if (type_needs_drop(local.ty, type_info) || is_owned_string)
                    && may_initialized[block.id.as_usize()].contains(local.id)
                    && move_state.drop_fact_state(&root_place) != LocalMoveState::Moved
                {
                    let must_exit = initialized[block.id.as_usize()].contains(local.id);
                    record_tracked_places(
                        &root_place,
                        must_exit,
                        move_state,
                        func,
                        type_info,
                        &mut tracked_places,
                    );
                    if is_owned_string {
                        rebuilt.push(AmirStmt::Destroy(root_place));
                    } else {
                        emit_recursive_drops(
                            &root_place,
                            local.ty,
                            type_info,
                            move_state,
                            skip_self,
                            true,
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

    tracked_places.sort_by_cached_key(|p| {
        (
            p.local.as_usize(),
            p.projections.len(),
            format!("{:?}", p.projections),
        )
    });
    let (phase_b, phase_a): (Vec<_>, Vec<_>) =
        tracked_places.into_iter().partition(has_variant_or_payload);

    expand_indirect_drops(func, type_info);
    apply_drop_flags(func, type_info, &phase_a);
    for _ in 0..16 {
        let changed_enum = expand_dynamic_enum_drops(func, type_info);
        let changed_arr = expand_array_drop_loops(func, type_info, literal_pool);
        if !changed_enum && !changed_arr {
            break;
        }
    }
    apply_drop_flags(func, type_info, &phase_b);
}

/// Canonicalize unsafe `dropInPlace` operations before backends see the AMIR.
/// The intrinsic is lowered to `Destroy(*pointer)`; this pass recursively
/// decomposes that place just like lexical cleanup, so emitters only need to
/// implement atomic leaf destruction and nominal destructor calls.
fn expand_indirect_drops(func: &mut AmirFunc, type_info: &dyn AmirTypeContext) {
    let has_indirect_destroy = func.stmts.payloads.raw.iter().any(|stmt| {
        matches!(
            stmt,
            AmirStmt::Destroy(place)
                if place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, AmirProjection::Deref))
        )
    });
    if !has_indirect_destroy {
        return;
    }

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
                    emit_recursive_drops(
                        place,
                        ty,
                        type_info,
                        move_state,
                        false,
                        true,
                        &mut rebuilt,
                    );
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
