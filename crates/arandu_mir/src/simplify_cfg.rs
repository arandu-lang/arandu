use crate::amir::{AmirBasicBlock, AmirFunc, AmirStmtTable, AmirTerminator, BlockId};
use crate::cfg::{clear_block, compute_cfg_edges, retarget_successor, transfer_edges};
use crate::layout::DenseRange;
use crate::{DiagCode, Diagnostic, Span};
use std::collections::VecDeque;

/// CFG simplification pass: jump threading, block merging, unreachable removal.
///
/// Uses a local worklist inside an outer fixpoint loop so that each
/// transformation feeds the next (e.g. block removal enables new merges).
/// Only `remove_unreachable_blocks` recomputes the full CFG (block indices
/// change); threading and merging update the CFG incrementally.
///
/// Returns `true` if any change was made.
pub fn simplify_cfg(func: &mut AmirFunc, bump: &bumpalo::Bump) -> Result<bool, Diagnostic> {
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
            block_changed |= thread_block(func, bid);
            block_changed |= merge_block_candidate(func, bid, bump)?;

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
fn thread_block(func: &mut AmirFunc, bid: BlockId) -> bool {
    let (target, args) = match &func.block(bid).terminator {
        AmirTerminator::Goto { target, args } => (*target, args),
        _ => return false,
    };
    if !args.is_empty() {
        return false;
    }

    let final_target = resolve_goto_chain(func, target);
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

fn resolve_goto_chain(func: &AmirFunc, mut block: BlockId) -> BlockId {
    let original = block;
    // A terminating chain visits at most one block per CFG node. Beyond
    // that bound a node was revisited: keep the original edge into the cycle.
    // This also handles self-loops without allocating a visited set.
    for _ in 0..func.blocks.len() {
        let b = func.block(block);
        if !b.statements.is_empty() || !func.block_params(b.params).is_empty() {
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
fn merge_block_candidate(
    func: &mut AmirFunc,
    bid: BlockId,
    bump: &bumpalo::Bump,
) -> Result<bool, Diagnostic> {
    // Gate 1: only Goto(empty). Suspend/Branch/Switch are control edges, not
    // fallthrough — same rule as LLVM merge of trivial blocks.
    match &func.block(bid).terminator {
        AmirTerminator::Goto { args, .. } if args.is_empty() => {}
        _ => return Ok(false),
    }

    let succs = func.successors(bid);
    if succs.len() != 1 {
        return Ok(false);
    }
    let succ = succs[0];
    if succ.as_usize() >= func.blocks.len() || succ == bid {
        return Ok(false);
    }
    if func.predecessors(succ).len() != 1 || func.predecessors(succ)[0] != bid {
        return Ok(false);
    }

    // Gate 2: successor phis/block-params cannot be dropped on the floor.
    if !func.block_params(func.block(succ).params).is_empty() {
        return Ok(false);
    }

    merge_two_blocks(func, bid, succ, bump)?;
    Ok(true)
}

fn merge_two_blocks(
    func: &mut AmirFunc,
    into: BlockId,
    from: BlockId,
    bump: &bumpalo::Bump,
) -> Result<(), Diagnostic> {
    // Snapshot per-block stmt order before taking ownership of the table.
    let mut block_stmt_ids = bumpalo::collections::Vec::with_capacity_in(func.blocks.len(), bump);
    for bi in 0..func.blocks.len() {
        let mut ids = bumpalo::collections::Vec::new_in(bump);
        ids.extend(func.block_stmt_ids(BlockId::from_usize(bi)));
        block_stmt_ids.push(ids);
    }

    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let ice_span = optimization_span(func);
    let mut slots = bumpalo::collections::Vec::with_capacity_in(old.payloads.raw.len(), bump);
    slots.extend(old.payloads.raw.into_iter().map(Some));
    let mut new_stmts = AmirStmtTable::new();
    let mut new_ranges = bumpalo::collections::Vec::with_capacity_in(func.blocks.len(), bump);

    for bi in 0..func.blocks.len() {
        let bid = BlockId::from_usize(bi);
        let start = new_stmts.len();
        let mut count = 0usize;

        if bid == into {
            for &stmt_id in &block_stmt_ids[bi] {
                let stmt = take_stmt(&mut slots, stmt_id.as_usize(), ice_span, "block merge")?;
                new_stmts.push(stmt);
                count += 1;
            }
            for &stmt_id in &block_stmt_ids[from.as_usize()] {
                let stmt = take_stmt(&mut slots, stmt_id.as_usize(), ice_span, "block merge")?;
                new_stmts.push(stmt);
                count += 1;
            }
        } else if bid == from {
            // stmts already merged into `into`
        } else {
            for &stmt_id in &block_stmt_ids[bi] {
                let stmt = take_stmt(&mut slots, stmt_id.as_usize(), ice_span, "block merge")?;
                new_stmts.push(stmt);
                count += 1;
            }
        }

        new_ranges.push(DenseRange::new(start, count));
    }

    let from_term = std::mem::replace(
        &mut func.block_mut(from).terminator,
        AmirTerminator::Unreachable,
    );
    func.block_mut(into).terminator = from_term;
    func.stmts = new_stmts;
    for (i, range) in new_ranges.into_iter().enumerate() {
        func.blocks[i].statements = range;
    }

    // Incremental CFG update: `into` inherits `from`'s successors;
    // `from` is cleared and will be removed by unreachable sweep.
    transfer_edges(&mut func.cfg, into, from);
    clear_block(&mut func.cfg, from);
    Ok(())
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
    use crate::passes::type_checker::types::{ArType, Primitive};
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
            span: arandu_lexer::Span::new(0, 0, 0),
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
}
