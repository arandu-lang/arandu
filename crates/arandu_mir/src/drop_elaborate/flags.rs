use crate::BitSet;
use crate::Span;
use crate::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirRvalue,
    AmirStmt, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, LocalId, TempId,
    for_each_rvalue_operand,
};
use crate::definite_init::transfer_init_stmt;
use crate::move_checker::{compute_temp_origins, place_is_prefix, tracked_move_place};
use arandu_middle::layout::DenseRange;
use arandu_middle::type_context::AmirTypeContext;
use arandu_middle::types::{ArType, Primitive};
use smallvec::SmallVec;
use std::collections::VecDeque;

/// Forward union dataflow: MAY-initialized locals at block exit and before each
/// statement. Pairs with `definite_init`'s MUST-init to detect conditionally
/// initialized locals cleanly without dropping uninitialized slots.
pub(super) fn compute_may_init(
    func: &AmirFunc,
) -> (Vec<BitSet<LocalId>>, Vec<Vec<BitSet<LocalId>>>) {
    let num_locals = func.locals.len();
    let num_blocks = func.blocks.len();
    if num_locals == 0 || num_blocks == 0 {
        return (
            vec![BitSet::with_capacity(num_locals); num_blocks],
            func.blocks
                .iter()
                .map(|b| vec![BitSet::with_capacity(num_locals); b.statements.len_usize()])
                .collect(),
        );
    }

    let mut block_in = vec![BitSet::<LocalId>::with_capacity(num_locals); num_blocks];
    let mut block_out = vec![BitSet::<LocalId>::with_capacity(num_locals); num_blocks];
    let mut worklist = VecDeque::new();
    for block in &func.blocks {
        worklist.push_back(block.id);
    }

    let mut iterations = 0;
    let sanity_limit = num_blocks * num_locals + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;
    while let Some(bid) = worklist.pop_front() {
        iterations += 1;
        assert!(
            iterations <= sanity_limit,
            "may-init dataflow failed to converge within limit: {iterations} > {sanity_limit}"
        );
        let bi = bid.as_usize();
        let block = &func.blocks[bi];

        let mut new_in = BitSet::with_capacity(num_locals);
        for &pred in func.predecessors(bid) {
            new_in.union_with(&block_out[pred.as_usize()]);
        }

        let mut new_out = new_in.clone();
        for param in func.block_params(block.params) {
            new_out.insert(param.local);
        }
        for stmt in func.block_stmts(bid) {
            transfer_init_stmt(stmt, &mut new_out);
        }

        if new_out != block_out[bi] {
            block_in[bi] = new_in;
            block_out[bi] = new_out;
            match &block.terminator {
                AmirTerminator::Return | AmirTerminator::Unreachable | AmirTerminator::Trap(_) => {}
                AmirTerminator::Goto { target, .. } => worklist.push_back(*target),
                AmirTerminator::Suspend { resume, .. } => worklist.push_back(*resume),
                AmirTerminator::Branch {
                    if_true, if_false, ..
                } => {
                    worklist.push_back(*if_true);
                    worklist.push_back(*if_false);
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (_, b, _) in targets {
                        worklist.push_back(*b);
                    }
                    worklist.push_back(otherwise.0);
                }
            }
        } else {
            block_in[bi] = new_in;
        }
    }

    let mut before = Vec::with_capacity(num_blocks);
    for (block, entry) in func.blocks.iter().zip(block_in) {
        let mut current = entry;
        for param in func.block_params(block.params) {
            current.insert(param.local);
        }
        let mut block_before = Vec::with_capacity(block.statements.len_usize());
        for stmt in func.block_stmts(block.id) {
            block_before.push(current.clone());
            transfer_init_stmt(stmt, &mut current);
        }
        before.push(block_before);
    }

    (block_out, before)
}

fn root_place(local: LocalId) -> AmirPlace {
    AmirPlace {
        local,
        projections: SmallVec::new(),
    }
}

fn push_block_stmts(table: &mut AmirStmtTable, stmts: Vec<AmirStmt>) -> DenseRange {
    let start = table.len();
    for stmt in stmts {
        table.push(stmt);
    }
    DenseRange::new(start, table.len() - start)
}

fn best_flag_for(place: &AmirPlace, flags: &[(&AmirPlace, LocalId)]) -> Option<LocalId> {
    let canon = tracked_move_place(place);
    flags
        .iter()
        .filter(|(tracked, _)| place_is_prefix(tracked, &canon))
        .max_by_key(|(tracked, _)| tracked.projections.len())
        .map(|(_, flag)| *flag)
}

fn collect_moved_origins_for_stmt(
    stmt: &AmirStmt,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
) -> Vec<AmirPlace> {
    let mut moved = Vec::new();
    let mut record_op = |op: &AmirOperand| {
        if let AmirOperand::Move(temp) = op
            && func.temps.get(temp.as_usize()).is_some_and(|t| !t.is_copy)
            && let Some(origin) = temp_origins.get(temp.as_usize()).and_then(Option::as_ref)
        {
            moved.push(origin.clone());
        }
    };
    match stmt {
        AmirStmt::Assign { lhs, rhs } => {
            if !matches!(
                rhs,
                AmirRvalue::Len(_) | AmirRvalue::SliceData(_) | AmirRvalue::Discriminant { .. }
            ) {
                for_each_rvalue_operand(rhs, |op| record_op(op));
            }
            if let AmirRvalue::Load(place) = rhs
                && lhs.as_usize() == 0
                && func
                    .temps
                    .first()
                    .is_some_and(|ret| !ret.is_copy && ret.ty == func.return_type)
            {
                moved.push(place.clone());
            }
        }
        AmirStmt::Store { rhs, .. } | AmirStmt::Free(rhs) | AmirStmt::ArenaRewind(rhs) => {
            record_op(rhs);
        }
        AmirStmt::Call { callee, args, .. } => {
            record_op(callee);
            for arg in args {
                record_op(arg);
            }
        }
        AmirStmt::Destroy(_)
        | AmirStmt::StorageLive(_)
        | AmirStmt::StorageDead(_)
        | AmirStmt::Nop => {}
    }
    moved
}

fn flags_cleared_by_move_places(
    moved_places: &[AmirPlace],
    flags: &[(&AmirPlace, LocalId)],
) -> Vec<LocalId> {
    let mut cleared = Vec::new();
    for moved_place in moved_places {
        let canon = tracked_move_place(moved_place);
        for (tracked, flag) in flags {
            if place_is_prefix(&canon, tracked) && !cleared.contains(flag) {
                cleared.push(*flag);
            }
        }
    }
    cleared
}

fn flags_cleared_by_args(
    args: &[AmirOperand],
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    flags: &[(&AmirPlace, LocalId)],
) -> Vec<LocalId> {
    let mut moved_places = Vec::new();
    for op in args {
        if let AmirOperand::Move(temp) = op
            && func.temps.get(temp.as_usize()).is_some_and(|t| !t.is_copy)
            && let Some(origin) = temp_origins.get(temp.as_usize()).and_then(Option::as_ref)
        {
            moved_places.push(origin.clone());
        }
    }
    flags_cleared_by_move_places(&moved_places, flags)
}

/// Allocates runtime boolean drop flags for `tracked_places`, initializes them
/// at `bb0`, updates them on initialization (`true`) and moves (`false`), and
/// wraps matching `Destroy` statements with a runtime guard.
pub(super) fn apply_drop_flags(
    func: &mut AmirFunc,
    type_info: &dyn AmirTypeContext,
    tracked_places: &[AmirPlace],
) {
    if tracked_places.is_empty() || func.blocks.is_empty() {
        return;
    }

    let temp_origins = compute_temp_origins(func);
    let bool_ty = type_info
        .type_interner()
        .intern(ArType::Primitive(Primitive::Bool));

    let base_local = func.locals.len();
    let mut flags: Vec<(&AmirPlace, LocalId)> = Vec::with_capacity(tracked_places.len());
    for (offset, place) in tracked_places.iter().enumerate() {
        let flag_id = LocalId::from_usize(base_local + offset);
        func.locals.push(AmirLocal {
            id: flag_id,
            ty: bool_ty,
            is_memory: true,
            symbol: None,
            span: Span::new(0, 0, 0),
            use_span: None,
        });
        flags.push((place, flag_id));
    }

    let old_blocks = std::mem::take(&mut func.blocks);
    let old_stmts = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut blocks: Vec<Option<AmirBasicBlock>> = vec![None; old_blocks.len()];
    let mut statements = AmirStmtTable::new();

    for old_block in old_blocks {
        let mut cur_bid = old_block.id;
        let mut cur_params = old_block.params;
        let mut cur_stmts = Vec::new();

        if old_block.id.as_usize() == 0 {
            for &(_, flag) in &flags {
                cur_stmts.push(AmirStmt::StorageLive(flag));
                cur_stmts.push(AmirStmt::Store {
                    lhs: root_place(flag),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
                });
            }
        }
        for param in func.block_params(old_block.params) {
            for &(tracked, flag) in &flags {
                if tracked.local == param.local {
                    cur_stmts.push(AmirStmt::Store {
                        lhs: root_place(flag),
                        rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                    });
                }
            }
        }

        let block_stmts = old_stmts
            .payloads
            .raw
            .get(old_block.statements.as_range())
            .unwrap_or(&[]);

        let mut i = 0;
        while i < block_stmts.len() {
            if let AmirStmt::Destroy(place) = &block_stmts[i]
                && let Some(flag) = best_flag_for(place, &flags)
            {
                let mut group = vec![AmirStmt::Destroy(place.clone())];
                let mut j = i + 1;
                while j < block_stmts.len() {
                    if let AmirStmt::Destroy(next_place) = &block_stmts[j]
                        && best_flag_for(next_place, &flags) == Some(flag)
                    {
                        group.push(AmirStmt::Destroy(next_place.clone()));
                        j += 1;
                    } else {
                        break;
                    }
                }
                group.push(AmirStmt::Store {
                    lhs: root_place(flag),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
                });

                let cond_temp = TempId::from_usize(func.temps.len());
                func.temps.push(AmirTemp {
                    id: cond_temp,
                    ty: bool_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 0),
                });
                cur_stmts.push(AmirStmt::Assign {
                    lhs: cond_temp,
                    rhs: AmirRvalue::Load(root_place(flag)),
                });

                let drop_bid = BlockId::from_usize(blocks.len());
                blocks.push(None);
                let cont_bid = BlockId::from_usize(blocks.len());
                blocks.push(None);

                blocks[cur_bid.as_usize()] = Some(AmirBasicBlock {
                    id: cur_bid,
                    params: cur_params,
                    statements: push_block_stmts(&mut statements, std::mem::take(&mut cur_stmts)),
                    terminator: AmirTerminator::Branch {
                        condition: AmirOperand::Copy(cond_temp),
                        if_true: drop_bid,
                        true_args: Vec::new(),
                        if_false: cont_bid,
                        false_args: Vec::new(),
                    },
                });
                blocks[drop_bid.as_usize()] = Some(AmirBasicBlock {
                    id: drop_bid,
                    params: DenseRange::empty(),
                    statements: push_block_stmts(&mut statements, group),
                    terminator: AmirTerminator::Goto {
                        target: cont_bid,
                        args: Vec::new(),
                    },
                });

                cur_bid = cont_bid;
                cur_params = DenseRange::empty();
                i = j;
                continue;
            }

            let stmt = &block_stmts[i];
            let moved_places = collect_moved_origins_for_stmt(stmt, func, &temp_origins);
            let cleared = flags_cleared_by_move_places(&moved_places, &flags);
            for flag in cleared {
                cur_stmts.push(AmirStmt::Store {
                    lhs: root_place(flag),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
                });
            }
            cur_stmts.push(stmt.clone());
            if let AmirStmt::Store { lhs, .. } = stmt {
                let canon_lhs = tracked_move_place(lhs);
                for &(tracked, flag) in &flags {
                    if place_is_prefix(&canon_lhs, tracked) {
                        cur_stmts.push(AmirStmt::Store {
                            lhs: root_place(flag),
                            rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                        });
                    }
                }
            }
            i += 1;
        }

        let mut term = old_block.terminator;
        if matches!(term, AmirTerminator::Return) {
            for &(_, flag) in &flags {
                cur_stmts.push(AmirStmt::StorageDead(flag));
            }
        } else {
            let mut split_edge = |target: &mut BlockId, args: &mut Vec<AmirOperand>| {
                let cleared = flags_cleared_by_args(args, func, &temp_origins, &flags);
                if cleared.is_empty() {
                    return;
                }
                let tramp_bid = BlockId::from_usize(blocks.len());
                blocks.push(None);
                let mut tramp_stmts = Vec::with_capacity(cleared.len());
                for flag in cleared {
                    tramp_stmts.push(AmirStmt::Store {
                        lhs: root_place(flag),
                        rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
                    });
                }
                let old_target = *target;
                let old_args = std::mem::take(args);
                blocks[tramp_bid.as_usize()] = Some(AmirBasicBlock {
                    id: tramp_bid,
                    params: DenseRange::empty(),
                    statements: push_block_stmts(&mut statements, tramp_stmts),
                    terminator: AmirTerminator::Goto {
                        target: old_target,
                        args: old_args,
                    },
                });
                *target = tramp_bid;
            };
            match &mut term {
                AmirTerminator::Goto { target, args } => split_edge(target, args),
                AmirTerminator::Branch {
                    if_true,
                    true_args,
                    if_false,
                    false_args,
                    ..
                } => {
                    split_edge(if_true, true_args);
                    split_edge(if_false, false_args);
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (_, target, args) in targets.iter_mut() {
                        split_edge(target, args);
                    }
                    split_edge(&mut otherwise.0, &mut otherwise.1);
                }
                AmirTerminator::Suspend { resume, args, .. } => split_edge(resume, args),
                AmirTerminator::Return | AmirTerminator::Unreachable | AmirTerminator::Trap(_) => {}
            }
        }

        blocks[cur_bid.as_usize()] = Some(AmirBasicBlock {
            id: cur_bid,
            params: cur_params,
            statements: push_block_stmts(&mut statements, cur_stmts),
            terminator: term,
        });
    }

    func.blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, block)| {
            block.unwrap_or(AmirBasicBlock {
                id: BlockId::from_usize(index),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Unreachable,
            })
        })
        .collect();
    func.stmts = statements;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
}
