use crate::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirStmtTable,
    AmirTemp, AmirTerminator, BlockId, TempId, for_each_rvalue_operand,
};
use crate::cfg::{clear_block, compute_cfg_edges, retarget_successor, transfer_edges};
use crate::layout::DenseRange;
use crate::ops::{BinaryOp, UnaryOp};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use crate::{DiagCode, Diagnostic, Span};
use std::collections::VecDeque;

/// CFG simplification pass: jump threading, block merging, diamond
/// if-conversion (when `interner` is available), unreachable removal.
///
/// Uses a local worklist inside an outer fixpoint loop so that each
/// transformation feeds the next (e.g. block removal enables new merges).
/// Only `remove_unreachable_blocks` recomputes the full CFG (block indices
/// change); threading and merging update the CFG incrementally.
///
/// Returns `true` if any change was made.
#[cfg(test)]
pub fn simplify_cfg(func: &mut AmirFunc, bump: &bumpalo::Bump) -> Result<bool, Diagnostic> {
    simplify_cfg_with_interner(func, bump, None)
}

pub fn simplify_cfg_with_interner(
    func: &mut AmirFunc,
    bump: &bumpalo::Bump,
    interner: Option<&TypeInterner>,
) -> Result<bool, Diagnostic> {
    // Recompute CFG from scratch to catch any terminator changes made by
    // prior passes (SCCP, DCE) — those passes do not update the CFG.
    func.cfg = compute_cfg_edges(&func.blocks);

    let mut changed = false;
    let mut outer_iters = 0u32;

    loop {
        outer_iters += 1;
        if outer_iters > crate::analysis_limits::CFG_SIMPLIFY_MAX_OUTER_ITERS {
            break;
        }

        let mut local = false;

        // Phase 1 — jump threading + block merging with local worklist.
        // Merges are logical: `absorbed[b]` lists the blocks whose statements
        // follow `b`'s, and one compaction rebuilds the table afterwards.
        // Rebuilding it per merge made a long chain quadratic.
        let mut absorbed: Vec<Vec<BlockId>> = vec![Vec::new(); func.blocks.len()];
        let mut worklist: VecDeque<BlockId> =
            (0..func.blocks.len()).map(BlockId::from_usize).collect();
        let max_visits = func.blocks.len().saturating_mul(4).max(16);

        let mut visited = 0u64;
        while let Some(bid) = worklist.pop_front() {
            visited += 1;
            if visited > max_visits as u64 {
                break;
            }
            if bid.as_usize() >= func.blocks.len() {
                continue;
            }

            let mut block_changed = false;
            block_changed |= thread_block(func, bid, &absorbed);
            block_changed |= merge_block_candidate(func, bid, &mut absorbed);

            if block_changed {
                local = true;
                for &pred in func.predecessors(bid) {
                    worklist.push_back(pred);
                }
                for &succ in func.successors(bid) {
                    worklist.push_back(succ);
                }
            }
        }

        if absorbed.iter().any(|blocks| !blocks.is_empty()) {
            compact_merged_statements(func, &absorbed, bump)?;
        }

        if let Some(interner) = interner {
            local |= if_convert_diamonds(func, interner, bump)?;
        }

        // Phase 2 — unreachable removal (full CFG recompute, indices shift).
        local |= remove_unreachable_blocks(func, bump)?;

        changed |= local;
        if !local {
            break;
        }
    }

    Ok(changed)
}

// ---------------------------------------------------------------------------
// Jump threading
// ---------------------------------------------------------------------------

/// If `bid` ends with `Goto(target)` and `target` is an empty `Goto`-only
/// chain, bypass the chain and go directly to the final non-trivial block.
fn thread_block(func: &mut AmirFunc, bid: BlockId, absorbed: &[Vec<BlockId>]) -> bool {
    let (target, args) = match &func.block(bid).terminator {
        AmirTerminator::Goto { target, args } => (*target, args),
        _ => return false,
    };
    if !args.is_empty() {
        return false;
    }

    let final_target = resolve_goto_chain(func, target, absorbed);
    if final_target == target {
        return false;
    }

    func.block_mut(bid).terminator = AmirTerminator::Goto {
        target: final_target,
        args: Vec::new(),
    };
    let _ = retarget_successor(&mut func.cfg, bid, target, final_target);
    true
}

fn resolve_goto_chain(func: &AmirFunc, mut block: BlockId, absorbed: &[Vec<BlockId>]) -> BlockId {
    let original = block;
    // A terminating chain visits at most one block per CFG node. Beyond
    // that bound a node was revisited: keep the original edge into the cycle.
    // This also handles self-loops without allocating a visited set.
    for _ in 0..func.blocks.len() {
        let b = func.block(block);
        if !b.statements.is_empty()
            || !absorbed[block.as_usize()].is_empty()
            || !func.block_params(b.params).is_empty()
        {
            return block;
        }
        match &b.terminator {
            AmirTerminator::Goto { target, args } => {
                if !args.is_empty() {
                    return block;
                }
                block = *target;
            }
            _ => return block,
        }
    }
    original
}

// ---------------------------------------------------------------------------
// Block merging
// ---------------------------------------------------------------------------

/// If `bid` has exactly one successor `succ` and `succ` has exactly one
/// predecessor `bid`, merge them.
///
/// # Safety of merge (SSA / block-param form)
/// Only pure fallthrough is legal:
/// - `bid` must end in **`Goto` with empty args** (not `Suspend` / `Branch` /
///   `SwitchInt`). Merging across `Suspend` deletes the await frontier and
///   drops resume block params → ICE / wrong codegen under `--opt`.
/// - `succ` must have **no block params**. Params require jump-arg materialization
///   before merge; dropping them leaves undef uses of the param temps.
fn merge_block_candidate(func: &mut AmirFunc, bid: BlockId, absorbed: &mut [Vec<BlockId>]) -> bool {
    // Gate 1: only Goto(empty). Suspend/Branch/Switch are control edges, not
    // fallthrough — same rule as LLVM merge of trivial blocks.
    match &func.block(bid).terminator {
        AmirTerminator::Goto { args, .. } if args.is_empty() => {}
        _ => return false,
    }

    let succs = func.successors(bid);
    if succs.len() != 1 {
        return false;
    }
    let succ = succs[0];
    if succ.as_usize() >= func.blocks.len() || succ == bid {
        return false;
    }
    if func.predecessors(succ).len() != 1 || func.predecessors(succ)[0] != bid {
        return false;
    }

    // Gate 2: successor phis/block-params cannot be dropped on the floor.
    if !func.block_params(func.block(succ).params).is_empty() {
        return false;
    }

    merge_two_blocks(func, bid, succ, absorbed);
    true
}

/// Appends `from` to `into` logically: `into` takes `from`'s terminator and
/// CFG edges now, and `from`'s statements at the next compaction.
fn merge_two_blocks(
    func: &mut AmirFunc,
    into: BlockId,
    from: BlockId,
    absorbed: &mut [Vec<BlockId>],
) {
    let mut moved = std::mem::take(&mut absorbed[from.as_usize()]);
    absorbed[into.as_usize()].push(from);
    absorbed[into.as_usize()].append(&mut moved);

    let from_term = std::mem::replace(
        &mut func.block_mut(from).terminator,
        AmirTerminator::Unreachable,
    );
    func.block_mut(into).terminator = from_term;

    // Incremental CFG update: `into` inherits `from`'s successors;
    // `from` is cleared and will be removed by unreachable sweep.
    transfer_edges(&mut func.cfg, into, from);
    clear_block(&mut func.cfg, from);
}

/// Rebuilds the statement table once, placing each block's absorbed blocks'
/// statements after its own, in merge order.
fn compact_merged_statements(
    func: &mut AmirFunc,
    absorbed: &[Vec<BlockId>],
    bump: &bumpalo::Bump,
) -> Result<(), Diagnostic> {
    let mut block_stmt_ids = bumpalo::collections::Vec::with_capacity_in(func.blocks.len(), bump);
    for bi in 0..func.blocks.len() {
        let mut ids = bumpalo::collections::Vec::new_in(bump);
        ids.extend(func.block_stmt_ids(BlockId::from_usize(bi)));
        block_stmt_ids.push(ids);
    }
    let mut merged_away = bumpalo::collections::Vec::from_iter_in(
        std::iter::repeat_n(false, func.blocks.len()),
        bump,
    );
    for blocks in absorbed {
        for from in blocks {
            merged_away[from.as_usize()] = true;
        }
    }

    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let ice_span = optimization_span(func);
    let mut slots = bumpalo::collections::Vec::with_capacity_in(old.payloads.raw.len(), bump);
    slots.extend(old.payloads.raw.into_iter().map(Some));
    let mut new_stmts = AmirStmtTable::new();
    let mut new_ranges = bumpalo::collections::Vec::with_capacity_in(func.blocks.len(), bump);

    for bi in 0..func.blocks.len() {
        let start = new_stmts.len();
        if !merged_away[bi] {
            let parts =
                std::iter::once(BlockId::from_usize(bi)).chain(absorbed[bi].iter().copied());
            for part in parts {
                for &stmt_id in &block_stmt_ids[part.as_usize()] {
                    let stmt = take_stmt(&mut slots, stmt_id.as_usize(), ice_span, "block merge")?;
                    new_stmts.push(stmt);
                }
            }
        }
        new_ranges.push(DenseRange::new(start, new_stmts.len() - start));
    }

    func.stmts = new_stmts;
    for (i, range) in new_ranges.into_iter().enumerate() {
        func.blocks[i].statements = range;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Diamond if-conversion (Branch diamond -> AmirRvalue::Select)
// ---------------------------------------------------------------------------

const MAX_SELECT_ARM_PREP_STMTS: usize = 2;

fn is_scalar_select_ty(ty: TypeId, interner: &TypeInterner) -> bool {
    match interner.resolve(ty) {
        ArType::Primitive(Primitive::Bool | Primitive::Char) | ArType::Ptr(_) => true,
        ArType::Primitive(primitive) => primitive.is_numeric(),
        _ => false,
    }
}

fn temp_is_select_scalar(func: &AmirFunc, temp: TempId, interner: &TypeInterner) -> bool {
    func.temps.get(temp.as_usize()).is_some_and(|meta| {
        meta.is_copy && !meta.is_nullable && is_scalar_select_ty(meta.ty, interner)
    })
}

fn operand_is_speculatable(
    op: &AmirOperand,
    lhs_ty: TypeId,
    func: &AmirFunc,
    interner: &TypeInterner,
) -> bool {
    match op {
        AmirOperand::Move(_) | AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => false,
        // `Nil` stands for a value a path leaves undefined; as a select arm
        // it is valid only where it is a null pointer (as the validator
        // checks), never for an integer or a float.
        AmirOperand::Constant(AmirConstant::Nil) => {
            matches!(interner.resolve(lhs_ty), ArType::Ptr(_))
        }
        AmirOperand::Constant(_) => true,
        AmirOperand::Copy(temp) => {
            if !temp_is_select_scalar(func, *temp, interner) {
                return false;
            }
            // Narrowing float-to-int casts trap on NaN / out-of-range inputs.
            let src_float = matches!(
                interner.resolve(func.temps[temp.as_usize()].ty),
                ArType::Primitive(p) if p.is_float()
            );
            let dst_int = matches!(
                interner.resolve(lhs_ty),
                ArType::Primitive(p) if p.is_integer()
            );
            !(src_float && dst_int)
        }
    }
}

fn rvalue_is_speculatable(
    rhs: &AmirRvalue,
    lhs_ty: TypeId,
    func: &AmirFunc,
    interner: &TypeInterner,
) -> bool {
    if !is_scalar_select_ty(lhs_ty, interner) {
        return false;
    }
    let mut operands_ok = true;
    for_each_rvalue_operand(rhs, |op| {
        operands_ok &= operand_is_speculatable(op, lhs_ty, func, interner);
    });
    if !operands_ok {
        return false;
    }
    match rhs {
        AmirRvalue::Use(_) | AmirRvalue::Select { .. } => true,
        AmirRvalue::Unary { op, .. } => matches!(op, UnaryOp::Not | UnaryOp::BitNot),
        AmirRvalue::Binary { op, .. } => matches!(
            op,
            BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::LtEqual
                | BinaryOp::Gt
                | BinaryOp::GtEqual
                | BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
        ),
        _ => false,
    }
}

fn collect_speculatable_arm(
    func: &AmirFunc,
    bid: BlockId,
    max_assigns: usize,
    interner: &TypeInterner,
) -> Option<Vec<(TempId, AmirRvalue)>> {
    let mut assigns = Vec::new();
    for stmt in func.block_stmts(bid) {
        match stmt {
            AmirStmt::Nop => continue,
            AmirStmt::Assign { lhs, rhs } => {
                if assigns.len() >= max_assigns || !temp_is_select_scalar(func, *lhs, interner) {
                    return None;
                }
                let lhs_ty = func.temps[lhs.as_usize()].ty;
                if !rvalue_is_speculatable(rhs, lhs_ty, func, interner) {
                    return None;
                }
                assigns.push((*lhs, rhs.clone()));
            }
            _ => return None,
        }
    }
    Some(assigns)
}

fn arm_reads_temp(assigns: &[(TempId, AmirRvalue)], target: TempId) -> bool {
    let mut found = false;
    for (_, rhs) in assigns {
        for_each_rvalue_operand(rhs, |op| {
            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                found |= *t == target;
            }
        });
    }
    found
}

fn push_temp(func: &mut AmirFunc, ty: TypeId, span: Span) -> TempId {
    let id = TempId::from_usize(func.temps.len());
    func.temps.push(AmirTemp {
        id,
        ty,
        is_copy: true,
        is_nullable: false,
        span,
    });
    id
}

fn emit_arm_result(
    func: &mut AmirFunc,
    dest_ty: TypeId,
    span: Span,
    arm: Vec<(TempId, AmirRvalue)>,
    out: &mut Vec<AmirStmt>,
) -> AmirOperand {
    let len = arm.len();
    for (idx, (lhs, rhs)) in arm.into_iter().enumerate() {
        if idx + 1 < len {
            out.push(AmirStmt::Assign { lhs, rhs });
        } else {
            if let AmirRvalue::Use(op @ (AmirOperand::Copy(_) | AmirOperand::Constant(_))) = &rhs
                && match op {
                    AmirOperand::Copy(src) => func.temps[src.as_usize()].ty == dest_ty,
                    AmirOperand::Constant(_) => true,
                    _ => false,
                }
            {
                return *op;
            }
            let tmp = push_temp(func, dest_ty, span);
            out.push(AmirStmt::Assign { lhs: tmp, rhs });
            return AmirOperand::Copy(tmp);
        }
    }
    unreachable!("non-empty arm always produces an operand")
}

fn if_convert_diamonds(
    func: &mut AmirFunc,
    interner: &TypeInterner,
    bump: &bumpalo::Bump,
) -> Result<bool, Diagnostic> {
    let n_blocks = func.blocks.len();
    if n_blocks < 3 || func.temps.is_empty() {
        return Ok(false);
    }

    let mut temp_defs = vec![0usize; func.temps.len()];
    for &p in &func.params {
        temp_defs[p.as_usize()] += 1;
    }
    if let Some(recv) = &func.receiver {
        temp_defs[recv.temp.as_usize()] += 1;
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            temp_defs[param.id.as_usize()] += 1;
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                temp_defs[lhs.as_usize()] += 1;
            }
        }
    }

    let mut consumed = vec![false; n_blocks];
    let mut appended: Vec<Vec<AmirStmt>> = vec![Vec::new(); n_blocks];
    let mut any_converted = false;
    let span = optimization_span(func);

    for bi in 0..n_blocks {
        let bid = BlockId::from_usize(bi);
        if consumed[bi] {
            continue;
        }
        let AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } = func.blocks[bi].terminator.clone()
        else {
            continue;
        };
        if !true_args.is_empty()
            || !false_args.is_empty()
            || if_true == if_false
            || if_true == bid
            || if_false == bid
            || if_true.as_usize() == 0
            || if_false.as_usize() == 0
            || consumed[if_true.as_usize()]
            || consumed[if_false.as_usize()]
        {
            continue;
        }
        let cond_ok = match &condition {
            AmirOperand::Constant(AmirConstant::Bool(_)) => true,
            AmirOperand::Copy(t) => matches!(
                interner.resolve(func.temps[t.as_usize()].ty),
                ArType::Primitive(Primitive::Bool)
            ),
            _ => false,
        };
        if !cond_ok {
            continue;
        }
        if func.predecessors(if_true) != [bid]
            || func.predecessors(if_false) != [bid]
            || !func
                .block_params(func.blocks[if_true.as_usize()].params)
                .is_empty()
            || !func
                .block_params(func.blocks[if_false.as_usize()].params)
                .is_empty()
        {
            continue;
        }

        let term_t = func.blocks[if_true.as_usize()].terminator.clone();
        let term_f = func.blocks[if_false.as_usize()].terminator.clone();

        match (&term_t, &term_f) {
            // Case A: SSA block-param diamond (`goto join(arg_t)` / `goto join(arg_f)`)
            (
                AmirTerminator::Goto {
                    target: join_t,
                    args: args_t,
                },
                AmirTerminator::Goto {
                    target: join_f,
                    args: args_f,
                },
            ) if join_t == join_f
                && *join_t != bid
                && *join_t != if_true
                && *join_t != if_false
                && !consumed[join_t.as_usize()]
                && args_t.len() == 1
                && args_f.len() == 1 =>
            {
                let join_params = func.block_params(func.blocks[join_t.as_usize()].params);
                if join_params.len() != 1
                    || !temp_is_select_scalar(func, join_params[0].id, interner)
                {
                    continue;
                }
                let select_ty = join_params[0].ty;
                let arg_t = args_t[0];
                let arg_f = args_f[0];
                if !operand_is_speculatable(&arg_t, select_ty, func, interner)
                    || !operand_is_speculatable(&arg_f, select_ty, func, interner)
                {
                    continue;
                }
                let Some(arm_t) =
                    collect_speculatable_arm(func, if_true, MAX_SELECT_ARM_PREP_STMTS, interner)
                else {
                    continue;
                };
                let Some(arm_f) =
                    collect_speculatable_arm(func, if_false, MAX_SELECT_ARM_PREP_STMTS, interner)
                else {
                    continue;
                };
                if !arm_t
                    .iter()
                    .chain(arm_f.iter())
                    .all(|(lhs, _)| temp_defs[lhs.as_usize()] == 1)
                {
                    continue;
                }
                for (lhs, rhs) in arm_t.into_iter().chain(arm_f) {
                    appended[bi].push(AmirStmt::Assign { lhs, rhs });
                }
                let select_tmp = push_temp(func, select_ty, span);
                appended[bi].push(AmirStmt::Assign {
                    lhs: select_tmp,
                    rhs: AmirRvalue::Select {
                        condition,
                        if_true: arg_t,
                        if_false: arg_f,
                    },
                });
                func.blocks[bi].terminator = AmirTerminator::Goto {
                    target: *join_t,
                    args: vec![AmirOperand::Copy(select_tmp)],
                };
                func.blocks[if_true.as_usize()].terminator = AmirTerminator::Unreachable;
                func.blocks[if_false.as_usize()].terminator = AmirTerminator::Unreachable;
                consumed[if_true.as_usize()] = true;
                consumed[if_false.as_usize()] = true;
                any_converted = true;
            }
            // Case B: shared-dest `if` expression (`dest = rhs_t; goto join()` / `dest = rhs_f; goto join()`)
            (
                AmirTerminator::Goto {
                    target: join_t,
                    args: args_t,
                },
                AmirTerminator::Goto {
                    target: join_f,
                    args: args_f,
                },
            ) if join_t == join_f
                && *join_t != bid
                && *join_t != if_true
                && *join_t != if_false
                && !consumed[join_t.as_usize()]
                && args_t.is_empty()
                && args_f.is_empty()
                && func
                    .block_params(func.blocks[join_t.as_usize()].params)
                    .is_empty() =>
            {
                let Some(arm_t) = collect_speculatable_arm(
                    func,
                    if_true,
                    MAX_SELECT_ARM_PREP_STMTS + 1,
                    interner,
                ) else {
                    continue;
                };
                let Some(arm_f) = collect_speculatable_arm(
                    func,
                    if_false,
                    MAX_SELECT_ARM_PREP_STMTS + 1,
                    interner,
                ) else {
                    continue;
                };
                let (Some((dest_t, _)), Some((dest_f, _))) = (arm_t.last(), arm_f.last()) else {
                    continue;
                };
                let dest = *dest_t;
                if dest != *dest_f
                    || temp_defs[dest.as_usize()] != 2
                    || arm_reads_temp(&arm_t, dest)
                    || arm_reads_temp(&arm_f, dest)
                {
                    continue;
                }
                if !arm_t[..arm_t.len() - 1]
                    .iter()
                    .chain(&arm_f[..arm_f.len() - 1])
                    .all(|(lhs, _)| temp_defs[lhs.as_usize()] == 1)
                {
                    continue;
                }
                let dest_ty = func.temps[dest.as_usize()].ty;
                let mut hoisted = Vec::new();
                let op_t = emit_arm_result(func, dest_ty, span, arm_t, &mut hoisted);
                let op_f = emit_arm_result(func, dest_ty, span, arm_f, &mut hoisted);
                appended[bi].extend(hoisted);
                appended[bi].push(AmirStmt::Assign {
                    lhs: dest,
                    rhs: AmirRvalue::Select {
                        condition,
                        if_true: op_t,
                        if_false: op_f,
                    },
                });
                func.blocks[bi].terminator = AmirTerminator::Goto {
                    target: *join_t,
                    args: Vec::new(),
                };
                func.blocks[if_true.as_usize()].terminator = AmirTerminator::Unreachable;
                func.blocks[if_false.as_usize()].terminator = AmirTerminator::Unreachable;
                consumed[if_true.as_usize()] = true;
                consumed[if_false.as_usize()] = true;
                any_converted = true;
            }
            // Case C: pure scalar early-return diamond (`_0 = t; Return` / `_0 = f; Return`)
            (AmirTerminator::Return, AmirTerminator::Return)
                if !func.locals.first().is_some_and(|local| local.is_memory)
                    && temp_is_select_scalar(func, TempId::from_usize(0), interner) =>
            {
                let Some(arm_t) = collect_speculatable_arm(
                    func,
                    if_true,
                    MAX_SELECT_ARM_PREP_STMTS + 1,
                    interner,
                ) else {
                    continue;
                };
                let Some(arm_f) = collect_speculatable_arm(
                    func,
                    if_false,
                    MAX_SELECT_ARM_PREP_STMTS + 1,
                    interner,
                ) else {
                    continue;
                };
                let ret_temp = TempId::from_usize(0);
                let (Some((dest_t, _)), Some((dest_f, _))) = (arm_t.last(), arm_f.last()) else {
                    continue;
                };
                if *dest_t != ret_temp
                    || *dest_f != ret_temp
                    || arm_reads_temp(&arm_t, ret_temp)
                    || arm_reads_temp(&arm_f, ret_temp)
                    || !arm_t[..arm_t.len() - 1]
                        .iter()
                        .chain(&arm_f[..arm_f.len() - 1])
                        .all(|(lhs, _)| *lhs != ret_temp && temp_defs[lhs.as_usize()] == 1)
                {
                    continue;
                }
                let ret_ty = func.temps[0].ty;
                let mut hoisted = Vec::new();
                let op_t = emit_arm_result(func, ret_ty, span, arm_t, &mut hoisted);
                let op_f = emit_arm_result(func, ret_ty, span, arm_f, &mut hoisted);
                appended[bi].extend(hoisted);
                appended[bi].push(AmirStmt::Assign {
                    lhs: ret_temp,
                    rhs: AmirRvalue::Select {
                        condition,
                        if_true: op_t,
                        if_false: op_f,
                    },
                });
                func.blocks[bi].terminator = AmirTerminator::Return;
                func.blocks[if_true.as_usize()].terminator = AmirTerminator::Unreachable;
                func.blocks[if_false.as_usize()].terminator = AmirTerminator::Unreachable;
                consumed[if_true.as_usize()] = true;
                consumed[if_false.as_usize()] = true;
                any_converted = true;
            }
            _ => {}
        }
    }

    if !any_converted {
        return Ok(false);
    }

    let mut block_stmt_ids = bumpalo::collections::Vec::with_capacity_in(n_blocks, bump);
    for bi in 0..n_blocks {
        let mut ids = bumpalo::collections::Vec::new_in(bump);
        ids.extend(func.block_stmt_ids(BlockId::from_usize(bi)));
        block_stmt_ids.push(ids);
    }
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut slots = bumpalo::collections::Vec::with_capacity_in(old.payloads.raw.len(), bump);
    slots.extend(old.payloads.raw.into_iter().map(Some));
    let mut new_stmts = AmirStmtTable::new();
    let mut new_ranges = bumpalo::collections::Vec::with_capacity_in(n_blocks, bump);

    for bi in 0..n_blocks {
        let start = new_stmts.len();
        if !consumed[bi] {
            for &stmt_id in &block_stmt_ids[bi] {
                let stmt = take_stmt(&mut slots, stmt_id.as_usize(), span, "diamond if-convert")?;
                new_stmts.push(stmt);
            }
            for stmt in std::mem::take(&mut appended[bi]) {
                new_stmts.push(stmt);
            }
        }
        new_ranges.push(DenseRange::new(start, new_stmts.len() - start));
    }

    func.stmts = new_stmts;
    for (block, range) in func.blocks.iter_mut().zip(new_ranges) {
        block.statements = range;
    }
    func.cfg = compute_cfg_edges(&func.blocks);
    Ok(true)
}

// ---------------------------------------------------------------------------
// Unreachable block removal
// ---------------------------------------------------------------------------

fn remove_unreachable_blocks(
    func: &mut AmirFunc,
    bump: &bumpalo::Bump,
) -> Result<bool, Diagnostic> {
    let n = func.blocks.len();
    if n == 0 {
        return Ok(false);
    }

    let mut reachable =
        bumpalo::collections::Vec::from_iter_in(std::iter::repeat_n(false, n), bump);
    let mut queue = VecDeque::new();
    reachable[0] = true;
    queue.push_back(BlockId::from_usize(0));

    while let Some(bid) = queue.pop_front() {
        let idx = bid.as_usize();
        if idx >= n {
            continue;
        }
        for succ in &func.cfg.successors[idx] {
            let sidx = succ.as_usize();
            if sidx < n && !reachable[sidx] {
                reachable[sidx] = true;
                queue.push_back(*succ);
            }
        }
    }

    let reachable_count = reachable.iter().filter(|&&r| r).count();
    if reachable_count == n {
        return Ok(false);
    }

    let mut old_to_new =
        bumpalo::collections::Vec::from_iter_in(std::iter::repeat_n(None, n), bump);
    let mut new_idx = 0usize;
    for old in 0..n {
        if reachable[old] {
            old_to_new[old] = Some(BlockId::from_usize(new_idx));
            new_idx += 1;
        }
    }

    // Snapshot stmt ids before taking ownership of the table / remapping blocks.
    let mut block_stmt_ids = bumpalo::collections::Vec::with_capacity_in(n, bump);
    for old in 0..n {
        let mut ids = bumpalo::collections::Vec::new_in(bump);
        ids.extend(func.block_stmt_ids(BlockId::from_usize(old)));
        block_stmt_ids.push(ids);
    }

    let mut new_blocks: Vec<AmirBasicBlock> = Vec::with_capacity(reachable_count);
    let mut new_block_params: Vec<crate::amir::BlockParam> = Vec::new();
    for old in 0..n {
        if !reachable[old] {
            continue;
        }
        let Some(new_id) = old_to_new[old] else {
            // Invariant: every reachable block got a new id in the loop above.
            continue;
        };
        let old_term = std::mem::replace(
            &mut func.blocks[old].terminator,
            AmirTerminator::Unreachable,
        );
        let param_start = new_block_params.len();
        new_block_params.extend_from_slice(func.block_params(func.blocks[old].params));
        let param_len = new_block_params.len() - param_start;
        new_blocks.push(AmirBasicBlock {
            id: new_id,
            statements: func.blocks[old].statements,
            params: DenseRange::new(param_start, param_len),
            terminator: remap_terminator(old_term, &old_to_new),
        });
    }

    let old_stmts = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let ice_span = optimization_span(func);
    let mut slots = bumpalo::collections::Vec::with_capacity_in(old_stmts.payloads.raw.len(), bump);
    slots.extend(old_stmts.payloads.raw.into_iter().map(Some));
    let mut new_stmts = AmirStmtTable::new();
    let mut new_ranges = bumpalo::collections::Vec::with_capacity_in(reachable_count, bump);

    for (old, &reached) in reachable.iter().enumerate() {
        if !reached {
            continue;
        }
        let start = new_stmts.len();
        let mut count = 0usize;
        for &stmt_id in &block_stmt_ids[old] {
            let stmt = take_stmt(
                &mut slots,
                stmt_id.as_usize(),
                ice_span,
                "unreachable sweep",
            )?;
            new_stmts.push(stmt);
            count += 1;
        }
        new_ranges.push(DenseRange::new(start, count));
    }

    func.blocks = new_blocks;
    func.block_params = new_block_params;
    func.stmts = new_stmts;

    for (block, range) in func.blocks.iter_mut().zip(new_ranges) {
        block.statements = range;
    }

    func.cfg = compute_cfg_edges(&func.blocks);
    Ok(true)
}

fn take_stmt(
    slots: &mut [Option<crate::amir::AmirStmt>],
    index: usize,
    span: Span,
    phase: &str,
) -> Result<crate::amir::AmirStmt, Diagnostic> {
    slots.get_mut(index).and_then(Option::take).ok_or_else(|| {
        Diagnostic::ice(
            DiagCode::ICEO001,
            format!("CFG {phase} encountered duplicate or out-of-range statement id {index}"),
            span,
        )
    })
}

fn optimization_span(func: &AmirFunc) -> Span {
    func.temps
        .first()
        .map(|temp| temp.span)
        .or_else(|| func.locals.first().map(|local| local.span))
        .unwrap_or_else(|| Span::new(func.symbol.file_id, 0, 0))
}

// ---------------------------------------------------------------------------
// Terminator helpers
// ---------------------------------------------------------------------------

/// Remap block ids in a terminator by value (moves arg vectors — no clone).
fn remap_terminator(term: AmirTerminator, map: &[Option<BlockId>]) -> AmirTerminator {
    match term {
        AmirTerminator::Goto { target, args } => {
            let new_target = map[target.as_usize()].unwrap_or(target);
            AmirTerminator::Goto {
                target: new_target,
                args,
            }
        }
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => {
            let new_t = map[if_true.as_usize()].unwrap_or(if_true);
            let new_f = map[if_false.as_usize()].unwrap_or(if_false);
            AmirTerminator::Branch {
                condition,
                if_true: new_t,
                true_args,
                if_false: new_f,
                false_args,
            }
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            let new_otherwise = (
                map[otherwise.0.as_usize()].unwrap_or(otherwise.0),
                otherwise.1,
            );
            let new_targets: Vec<_> = targets
                .into_iter()
                .map(|(val, b, args)| (val, map[b.as_usize()].unwrap_or(b), args))
                .collect();
            AmirTerminator::SwitchInt {
                discriminant,
                targets: new_targets,
                otherwise: new_otherwise,
            }
        }
        AmirTerminator::Suspend {
            future,
            resume,
            args,
        } => AmirTerminator::Suspend {
            future,
            resume: map[resume.as_usize()].unwrap_or(resume),
            args,
        },
        // Keep this match exhaustive so every new control-flow variant must
        // explicitly account for block renumbering.
        AmirTerminator::Return => AmirTerminator::Return,
        AmirTerminator::Unreachable => AmirTerminator::Unreachable,
    }
}

#[cfg(test)]
mod tests {

    fn intern_ty(ty: crate::types::ArType) -> crate::types::TypeId {
        // Fresh interner per call is OK in unit tests (pre-interns primitives).
        crate::types::TypeInterner::new().intern(ty)
    }
    use super::*;
    use crate::amir::program::extend_block_range;
    use crate::amir::{
        AmirBasicBlock, AmirConstant, AmirOperand, AmirPlace, AmirStmt, AmirTemp, AmirTerminator,
        LocalId, TempId,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::types::{ArType, Primitive};
    use smallvec::smallvec;

    fn bbid(id: usize) -> BlockId {
        BlockId::from_usize(id)
    }

    fn int_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        }
    }

    fn block(id: usize, stmts: Vec<AmirStmt>, stmts_table: &mut AmirStmtTable) -> AmirBasicBlock {
        let mut range = DenseRange::empty();
        for stmt in stmts {
            let instr = stmts_table.push(stmt);
            extend_block_range(&mut range, instr);
        }
        AmirBasicBlock {
            id: BlockId::from_usize(id),
            statements: range,
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }
    }

    fn make_func(blocks: Vec<AmirBasicBlock>, stmts: AmirStmtTable) -> AmirFunc {
        let cfg = compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![int_temp(0)],
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        }
    }

    // ── Jump threading ──

    #[test]
    fn empty_goto_cycles_terminate_and_preserve_divergence() {
        // Include a prefix leading into a cycle, not only a cycle at entry.
        for targets in [&[0][..], &[1, 0], &[1, 2, 1]] {
            let blocks = targets
                .iter()
                .enumerate()
                .map(|(id, &target)| AmirBasicBlock {
                    id: bbid(id),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(target),
                        args: Vec::new(),
                    },
                })
                .collect();
            let mut func = make_func(blocks, AmirStmtTable::new());
            let bump = bumpalo::Bump::new();
            simplify_cfg(&mut func, &bump).unwrap();
            assert!(!func.blocks.is_empty());
            for block in &func.blocks {
                let AmirTerminator::Goto { target, args } = &block.terminator else {
                    panic!("simplification must preserve an infinite loop");
                };
                assert!(target.as_usize() < func.blocks.len());
                assert!(args.is_empty());
            }
            assert!(
                !simplify_cfg(&mut func, &bump).unwrap(),
                "must reach a fixpoint"
            );
        }
    }

    #[test]
    fn unreachable_sweep_remaps_suspend_resume_and_preserves_arguments() {
        let mut st = AmirStmtTable::new();
        let mut entry = block(0, vec![], &mut st);
        entry.terminator = AmirTerminator::Suspend {
            future: AmirOperand::Copy(TempId::from_usize(0)),
            resume: bbid(2),
            args: vec![AmirOperand::Copy(TempId::from_usize(1))],
        };
        let dead = block(1, vec![], &mut st);
        let mut resume = block(2, vec![], &mut st);
        resume.params = DenseRange::new(0, 1);
        let mut func = make_func(vec![entry, dead, resume], st);
        func.temps = vec![int_temp(0), int_temp(1), int_temp(2)];
        func.block_params.push(crate::amir::BlockParam {
            id: TempId::from_usize(2),
            local: LocalId::from_usize(0),
            ty: int_temp(2).ty,
            from: None,
            moved: false,
        });
        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg(&mut func, &bump).unwrap());
        assert_eq!(func.blocks.len(), 2);
        let AmirTerminator::Suspend {
            future,
            resume,
            args,
        } = &func.blocks[0].terminator
        else {
            panic!("suspend must survive unreachable removal");
        };
        assert_eq!(*future, AmirOperand::Copy(TempId::from_usize(0)));
        assert_eq!(*resume, bbid(1));
        assert_eq!(args, &[AmirOperand::Copy(TempId::from_usize(1))]);
        assert_eq!(
            func.block_params(func.blocks[1].params)[0].id,
            TempId::from_usize(2)
        );
        assert_eq!(func.successors(bbid(0)), &[bbid(1)]);
        assert!(!simplify_cfg(&mut func, &bump).unwrap());
    }

    #[test]
    fn jump_thread_skips_empty_goto_chain() {
        let mut st = AmirStmtTable::new();
        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(1),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: bbid(1),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(2),
                        args: Vec::new(),
                    },
                },
                block(2, vec![], &mut st),
            ],
            st,
        );
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg(&mut func, &bump).unwrap());
        // After threading (bb0→bb1→bb2 become bb0→bb2) then merging
        // (bb0 + bb2) and unreachable removal: only 1 block remains.
        assert_eq!(func.blocks.len(), 1);
        assert!(matches!(
            func.block(bbid(0)).terminator,
            AmirTerminator::Return
        ));
    }

    #[test]
    fn jump_thread_no_change_when_no_chain() {
        let mut st = AmirStmtTable::new();
        let mut func = make_func(vec![block(0, vec![], &mut st)], st);
        func.cfg = compute_cfg_edges(&func.blocks);
        let bump = bumpalo::Bump::new();
        assert!(!simplify_cfg(&mut func, &bump).unwrap());
    }

    // ── Merge blocks ──

    #[test]
    fn merge_single_pred_single_succ() {
        let mut st = AmirStmtTable::new();
        let _i0 = st.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            },
            rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
        });
        let _i1 = st.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![],
            },
            rhs: AmirOperand::Constant(AmirConstant::Bool(false)),
        });

        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: DenseRange::new(0, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(1),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: bbid(1),
                    statements: DenseRange::new(1, 1),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
            ],
            st,
        );
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg(&mut func, &bump).unwrap());

        let b0 = func.block(bbid(0));
        assert_eq!(b0.statements.len, 2);
        assert!(matches!(b0.terminator, AmirTerminator::Return));
    }

    // ── Remove unreachable ──

    #[test]
    fn remove_unreachable_removes_orphaned_blocks() {
        let mut st = AmirStmtTable::new();
        let mut func = make_func(
            vec![
                block(0, vec![], &mut st),
                block(1, vec![], &mut st),
                block(2, vec![], &mut st),
            ],
            st,
        );
        func.blocks[0].terminator = AmirTerminator::Return;
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg(&mut func, &bump).unwrap());
        assert_eq!(func.blocks.len(), 1);
    }

    #[test]
    fn remove_unreachable_rewrites_terminator_targets() {
        let mut st = AmirStmtTable::new();
        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(1),
                        args: Vec::new(),
                    },
                },
                block(1, vec![], &mut st),
                block(2, vec![], &mut st),
            ],
            st,
        );
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg(&mut func, &bump).unwrap());
        // bb0 merges with bb1 (single-pred+single-succ), then unreachable
        // sweep removes bb2.  Only bb0 survives with bb1's Return.
        assert_eq!(func.blocks.len(), 1);
        assert!(matches!(
            func.block(bbid(0)).terminator,
            AmirTerminator::Return
        ));
    }

    #[test]
    fn no_change_when_all_reachable() {
        let mut st = AmirStmtTable::new();
        let mut func = make_func(vec![block(0, vec![], &mut st)], st);
        func.cfg = compute_cfg_edges(&func.blocks);
        let bump = bumpalo::Bump::new();
        assert!(!simplify_cfg(&mut func, &bump).unwrap());
    }

    /// Regression: never merge a `Suspend` block into its resume (single-edge
    /// lookalike). That deleted the await frontier and resume params under `--opt`.
    #[test]
    fn does_not_merge_across_suspend() {
        let st = AmirStmtTable::new();
        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Suspend {
                        future: AmirOperand::Copy(TempId::from_usize(0)),
                        resume: bbid(1),
                        args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                    },
                },
                AmirBasicBlock {
                    id: bbid(1),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
            ],
            st,
        );
        func.temps = vec![int_temp(0), int_temp(1), int_temp(2)];
        func.block_params = vec![crate::amir::BlockParam {
            id: TempId::from_usize(2),
            local: LocalId::from_usize(0),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            from: None,
            moved: false,
        }];
        func.blocks[1].params = DenseRange::new(0, 1);
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        // May still remove nothing / no change, but must keep Suspend + 2 blocks.
        let _ = simplify_cfg(&mut func, &bump).unwrap();
        assert_eq!(func.blocks.len(), 2, "resume must not be merged away");
        assert!(
            matches!(
                func.block(bbid(0)).terminator,
                AmirTerminator::Suspend { .. }
            ),
            "Suspend frontier must survive simplify_cfg"
        );
        assert!(!func.block_params(func.block(bbid(1)).params).is_empty());
    }

    /// Goto with jump args into a param block is not fallthrough — refuse merge.
    #[test]
    fn does_not_merge_goto_with_args_into_param_block() {
        let st = AmirStmtTable::new();
        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(1),
                        args: vec![AmirOperand::Copy(TempId::from_usize(0))],
                    },
                },
                AmirBasicBlock {
                    id: bbid(1),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
            ],
            st,
        );
        func.temps = vec![int_temp(0), int_temp(1)];
        func.block_params = vec![crate::amir::BlockParam {
            id: TempId::from_usize(1),
            local: LocalId::from_usize(0),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            from: None,
            moved: false,
        }];
        func.blocks[1].params = DenseRange::new(0, 1);
        func.cfg = compute_cfg_edges(&func.blocks);
        let bump = bumpalo::Bump::new();
        let _ = simplify_cfg(&mut func, &bump).unwrap();
        assert_eq!(func.blocks.len(), 2);
        assert!(matches!(
            func.block(bbid(0)).terminator,
            AmirTerminator::Goto { .. }
        ));
    }

    // ── simplify_cfg integration ──

    #[test]
    fn simplify_cfg_removes_goto_chain() {
        let mut st = AmirStmtTable::new();
        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(1),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: bbid(1),
                    statements: DenseRange::empty(),
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(2),
                        args: Vec::new(),
                    },
                },
                block(2, vec![], &mut st),
            ],
            st,
        );
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg(&mut func, &bump).unwrap());
        assert_eq!(func.blocks.len(), 1);
    }

    #[test]
    fn overlapping_statement_ranges_return_ice() {
        let mut st = AmirStmtTable::new();
        st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: crate::amir::AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        let shared = DenseRange::new(0, 1);
        let mut func = make_func(
            vec![
                AmirBasicBlock {
                    id: bbid(0),
                    statements: shared,
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Goto {
                        target: bbid(1),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: bbid(1),
                    statements: shared,
                    params: DenseRange::empty(),
                    terminator: AmirTerminator::Return,
                },
            ],
            st,
        );
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        let error = simplify_cfg(&mut func, &bump).unwrap_err();
        assert_eq!(error.code, DiagCode::ICEO001);
    }

    fn bool_temp(id: usize, interner: &TypeInterner) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: interner.intern(ArType::Primitive(Primitive::Bool)),
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        }
    }

    fn shared_int_temp(id: usize, interner: &TypeInterner) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: interner.intern(ArType::Primitive(Primitive::Int)),
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        }
    }

    #[test]
    fn simplify_cfg_converts_block_param_diamond_to_select() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let mut st = AmirStmtTable::new();
        let p_true = TempId::from_usize(1);
        let p_false = TempId::from_usize(2);
        let cond = TempId::from_usize(3);
        let join_param = TempId::from_usize(4);
        let a = TempId::from_usize(5);

        let b0 = AmirBasicBlock {
            id: bbid(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Branch {
                condition: AmirOperand::Copy(cond),
                if_true: bbid(1),
                true_args: Vec::new(),
                if_false: bbid(2),
                false_args: Vec::new(),
            },
        };
        let i_t = st.push(AmirStmt::Assign {
            lhs: p_true,
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Add,
                left: AmirOperand::Copy(a),
                right: AmirOperand::Copy(a),
            },
        });
        let mut b1 = block(1, vec![], &mut st);
        b1.statements = DenseRange::new(i_t.as_usize(), 1);
        b1.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: vec![AmirOperand::Copy(p_true)],
        };
        let mut b2 = block(2, vec![], &mut st);
        b2.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: vec![AmirOperand::Copy(p_false)],
        };
        let i_ret = st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(join_param)),
        });
        let mut b3 = block(3, vec![], &mut st);
        b3.statements = DenseRange::new(i_ret.as_usize(), 1);
        b3.params = DenseRange::new(0, 1);

        let mut func = make_func(vec![b0, b1, b2, b3], st);
        func.return_type = int_ty;
        func.temps = vec![
            shared_int_temp(0, &interner),
            shared_int_temp(1, &interner),
            shared_int_temp(2, &interner),
            bool_temp(3, &interner),
            shared_int_temp(4, &interner),
            shared_int_temp(5, &interner),
        ];
        func.params = vec![p_false, cond, a];
        func.block_params = vec![crate::amir::BlockParam {
            id: join_param,
            local: LocalId::from_usize(0),
            ty: int_ty,
            from: None,
            moved: false,
        }];
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg_with_interner(&mut func, &bump, Some(&interner)).unwrap());
        assert_eq!(func.blocks.len(), 2);
        let has_select = func.stmts.payloads.iter().any(|stmt| {
            matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::Select { .. },
                    ..
                }
            )
        });
        assert!(has_select, "expected diamond to fold into Select");
    }

    #[test]
    fn an_undefined_integer_arm_does_not_become_a_select() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let mut st = AmirStmtTable::new();
        let p_true = TempId::from_usize(1);
        let p_false = TempId::from_usize(2);
        let cond = TempId::from_usize(3);
        let join_param = TempId::from_usize(4);
        let a = TempId::from_usize(5);

        let b0 = AmirBasicBlock {
            id: bbid(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Branch {
                condition: AmirOperand::Copy(cond),
                if_true: bbid(1),
                true_args: Vec::new(),
                if_false: bbid(2),
                false_args: Vec::new(),
            },
        };
        let i_t = st.push(AmirStmt::Assign {
            lhs: p_true,
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Add,
                left: AmirOperand::Copy(a),
                right: AmirOperand::Copy(a),
            },
        });
        let mut b1 = block(1, vec![], &mut st);
        b1.statements = DenseRange::new(i_t.as_usize(), 1);
        b1.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: vec![AmirOperand::Copy(p_true)],
        };
        let mut b2 = block(2, vec![], &mut st);
        // A path that leaves the joined integer undefined passes `Nil`.
        b2.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: vec![AmirOperand::Constant(AmirConstant::Nil)],
        };
        let i_ret = st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(join_param)),
        });
        let mut b3 = block(3, vec![], &mut st);
        b3.statements = DenseRange::new(i_ret.as_usize(), 1);
        b3.params = DenseRange::new(0, 1);

        let mut func = make_func(vec![b0, b1, b2, b3], st);
        func.return_type = int_ty;
        func.temps = vec![
            shared_int_temp(0, &interner),
            shared_int_temp(1, &interner),
            shared_int_temp(2, &interner),
            bool_temp(3, &interner),
            shared_int_temp(4, &interner),
            shared_int_temp(5, &interner),
        ];
        func.params = vec![p_false, cond, a];
        func.block_params = vec![crate::amir::BlockParam {
            id: join_param,
            local: LocalId::from_usize(0),
            ty: int_ty,
            from: None,
            moved: false,
        }];
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        simplify_cfg_with_interner(&mut func, &bump, Some(&interner)).unwrap();
        let has_select = func.stmts.payloads.iter().any(|stmt| {
            matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::Select { .. },
                    ..
                }
            )
        });
        assert!(!has_select, "`Nil` is not an integer select arm");
    }

    #[test]
    fn simplify_cfg_converts_shared_dest_diamond_and_return_diamond() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let cond = TempId::from_usize(1);
        let a = TempId::from_usize(2);
        let b = TempId::from_usize(3);
        let dest = TempId::from_usize(4);

        let mut st = AmirStmtTable::new();
        let b0 = AmirBasicBlock {
            id: bbid(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Branch {
                condition: AmirOperand::Copy(cond),
                if_true: bbid(1),
                true_args: Vec::new(),
                if_false: bbid(2),
                false_args: Vec::new(),
            },
        };
        let it = st.push(AmirStmt::Assign {
            lhs: dest,
            rhs: AmirRvalue::Use(AmirOperand::Copy(a)),
        });
        let mut b1 = block(1, vec![], &mut st);
        b1.statements = DenseRange::new(it.as_usize(), 1);
        b1.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: Vec::new(),
        };
        let ie = st.push(AmirStmt::Assign {
            lhs: dest,
            rhs: AmirRvalue::Use(AmirOperand::Copy(b)),
        });
        let mut b2 = block(2, vec![], &mut st);
        b2.statements = DenseRange::new(ie.as_usize(), 1);
        b2.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: Vec::new(),
        };
        let ir = st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(dest)),
        });
        let mut b3 = block(3, vec![], &mut st);
        b3.statements = DenseRange::new(ir.as_usize(), 1);

        let mut func = make_func(vec![b0, b1, b2, b3], st);
        func.return_type = int_ty;
        func.temps = vec![
            shared_int_temp(0, &interner),
            bool_temp(1, &interner),
            shared_int_temp(2, &interner),
            shared_int_temp(3, &interner),
            shared_int_temp(4, &interner),
        ];
        func.params = vec![cond, a, b];
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(simplify_cfg_with_interner(&mut func, &bump, Some(&interner)).unwrap());
        assert_eq!(func.blocks.len(), 1);
        assert!(func.stmts.payloads.iter().any(|stmt| matches!(
            stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Select { .. },
                ..
            }
        )));
    }

    #[test]
    fn simplify_cfg_keeps_diamond_with_trapping_div() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let cond = TempId::from_usize(1);
        let a = TempId::from_usize(2);
        let b = TempId::from_usize(3);
        let dest = TempId::from_usize(4);

        let mut st = AmirStmtTable::new();
        let b0 = AmirBasicBlock {
            id: bbid(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Branch {
                condition: AmirOperand::Copy(cond),
                if_true: bbid(1),
                true_args: Vec::new(),
                if_false: bbid(2),
                false_args: Vec::new(),
            },
        };
        let it = st.push(AmirStmt::Assign {
            lhs: dest,
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Div,
                left: AmirOperand::Copy(a),
                right: AmirOperand::Copy(b),
            },
        });
        let mut b1 = block(1, vec![], &mut st);
        b1.statements = DenseRange::new(it.as_usize(), 1);
        b1.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: Vec::new(),
        };
        let ie = st.push(AmirStmt::Assign {
            lhs: dest,
            rhs: AmirRvalue::Use(AmirOperand::Copy(a)),
        });
        let mut b2 = block(2, vec![], &mut st);
        b2.statements = DenseRange::new(ie.as_usize(), 1);
        b2.terminator = AmirTerminator::Goto {
            target: bbid(3),
            args: Vec::new(),
        };
        let ir = st.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(dest)),
        });
        let mut b3 = block(3, vec![], &mut st);
        b3.statements = DenseRange::new(ir.as_usize(), 1);

        let mut func = make_func(vec![b0, b1, b2, b3], st);
        func.return_type = int_ty;
        func.temps = vec![
            shared_int_temp(0, &interner),
            bool_temp(1, &interner),
            shared_int_temp(2, &interner),
            shared_int_temp(3, &interner),
            shared_int_temp(4, &interner),
        ];
        func.params = vec![cond, a, b];
        func.cfg = compute_cfg_edges(&func.blocks);

        let bump = bumpalo::Bump::new();
        assert!(!simplify_cfg_with_interner(&mut func, &bump, Some(&interner)).unwrap());
        assert_eq!(func.blocks.len(), 4);
    }
}
