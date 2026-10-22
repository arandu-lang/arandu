//! CFG edge computation for AMIR basic blocks (C3).

use crate::amir::{AmirBasicBlock, AmirTerminator, BlockId};

#[derive(Debug, Clone, Default)]
pub struct ControlFlowGraph {
    pub successors: Vec<Vec<BlockId>>,
    pub predecessors: Vec<Vec<BlockId>>,
}

/// Build predecessor and successor edges for every block.
///
/// Runs in O(N + E) by walking each terminator's successors once.
pub fn compute_cfg_edges(blocks: &[AmirBasicBlock]) -> ControlFlowGraph {
    let num_blocks = blocks.len();
    let mut successors: Vec<Vec<BlockId>> = vec![Vec::new(); num_blocks];
    let mut predecessors: Vec<Vec<BlockId>> = vec![Vec::new(); num_blocks];

    for (i, block) in blocks.iter().enumerate() {
        let bid = BlockId::from_usize(i);
        let succs = terminator_successors(&block.terminator);
        for succ in succs {
            if succ.as_usize() < num_blocks {
                successors[bid.as_usize()].push(succ);
                predecessors[succ.as_usize()].push(bid);
            }
        }
    }

    ControlFlowGraph {
        successors,
        predecessors,
    }
}

// ---------------------------------------------------------------------------
// Incremental helpers (used by simplify_cfg pass)
// ---------------------------------------------------------------------------

/// When block `from`'s terminator changes so that its edge to `old` becomes
/// an edge to `new`, update both successor and predecessor lists in place.
///
/// Returns `false` if `old` is not currently a successor of `from` (no mutation).
#[must_use]
pub fn retarget_successor(
    cfg: &mut ControlFlowGraph,
    from: BlockId,
    old: BlockId,
    new: BlockId,
) -> bool {
    let succs = &mut cfg.successors[from.as_usize()];
    let Some(pos) = succs.iter().position(|&s| s == old) else {
        return false;
    };
    succs[pos] = new;

    cfg.predecessors[old.as_usize()].retain(|&p| p != from);
    cfg.predecessors[new.as_usize()].push(from);
    true
}

/// Remove all CFG entries for `block` and remove `block` from every
/// successor's predecessor list.  Does NOT remove the block itself from
/// the blocks vec — caller must handle that.
pub fn clear_block(cfg: &mut ControlFlowGraph, block: BlockId) {
    for &succ in &cfg.successors[block.as_usize()] {
        cfg.predecessors[succ.as_usize()].retain(|&p| p != block);
    }
    cfg.successors[block.as_usize()].clear();
    cfg.predecessors[block.as_usize()].clear();
}

/// Transfer all edges from `from` to `into`: every successor of `from`
/// now has `into` as a predecessor instead of `from`.  `from`'s successor
/// list is cleared after the transfer.
pub fn transfer_edges(cfg: &mut ControlFlowGraph, into: BlockId, from: BlockId) {
    // mem::take moves the Vec out without cloning, leaving an empty Vec in place.
    // The trailing .clear() is therefore unnecessary and has been removed.
    let from_succs = std::mem::take(&mut cfg.successors[from.as_usize()]);
    for succ in &from_succs {
        cfg.predecessors[succ.as_usize()].retain(|&p| p != from);
        cfg.predecessors[succ.as_usize()].push(into);
    }
    cfg.successors[into.as_usize()] = from_succs;
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn terminator_successors(term: &AmirTerminator) -> smallvec::SmallVec<[BlockId; 2]> {
    crate::amir::reachability::terminator_targets(term)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{AmirConstant, AmirOperand, AmirTerminator};

    fn block(id: usize, terminator: AmirTerminator) -> AmirBasicBlock {
        AmirBasicBlock {
            id: BlockId::from_usize(id),
            statements: crate::layout::DenseRange::empty(),
            params: crate::layout::DenseRange::empty(),
            terminator,
        }
    }

    #[test]
    fn cfg_empty_no_blocks() {
        let cfg = compute_cfg_edges(&[]);
        assert!(cfg.successors.is_empty());
        assert!(cfg.predecessors.is_empty());
    }

    #[test]
    fn cfg_single_return_block() {
        let blocks = vec![block(0, AmirTerminator::Return)];
        let cfg = compute_cfg_edges(&blocks);
        assert!(cfg.successors[0].is_empty());
        assert!(cfg.predecessors[0].is_empty());
    }

    #[test]
    fn terminator_edges_share_one_enumeration_and_ignore_invalid_targets() {
        use crate::amir::reachability::terminator_targets;
        let cond = AmirOperand::Constant(AmirConstant::Bool(true));
        let terms = [
            AmirTerminator::Return,
            AmirTerminator::Unreachable,
            AmirTerminator::Goto {
                target: BlockId::from_usize(4),
                args: Vec::new(),
            },
            AmirTerminator::Branch {
                condition: cond,
                if_true: BlockId::from_usize(1),
                true_args: Vec::new(),
                if_false: BlockId::from_usize(2),
                false_args: Vec::new(),
            },
            AmirTerminator::SwitchInt {
                discriminant: cond,
                targets: vec![(1, BlockId::from_usize(1), Vec::new())],
                otherwise: (BlockId::from_usize(2), Vec::new()),
            },
            AmirTerminator::Suspend {
                future: cond,
                resume: BlockId::from_usize(1),
                args: Vec::new(),
            },
        ];
        for term in &terms {
            assert_eq!(
                terminator_successors(term).as_slice(),
                terminator_targets(term).as_slice()
            );
        }
        let blocks = vec![block(
            0,
            AmirTerminator::Goto {
                target: BlockId::from_usize(9),
                args: Vec::new(),
            },
        )];
        let cfg = compute_cfg_edges(&blocks);
        assert!(cfg.successors[0].is_empty());
        assert_eq!(terminator_targets(&blocks[0].terminator).len(), 1);
    }

    #[test]
    fn cfg_two_blocks_with_goto() {
        let blocks = vec![
            block(
                0,
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
            ),
            block(1, AmirTerminator::Return),
        ];
        let cfg = compute_cfg_edges(&blocks);
        assert_eq!(cfg.successors[0], vec![BlockId::from_usize(1)]);
        assert_eq!(cfg.predecessors[1], vec![BlockId::from_usize(0)]);
    }

    #[test]
    fn cfg_branch_has_two_successors() {
        let cond = AmirOperand::Constant(AmirConstant::Bool(true));
        let blocks = vec![
            block(
                0,
                AmirTerminator::Branch {
                    condition: cond,
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            ),
            block(1, AmirTerminator::Return),
            block(2, AmirTerminator::Return),
        ];
        let cfg = compute_cfg_edges(&blocks);
        assert_eq!(cfg.successors[0].len(), 2);
        assert!(cfg.successors[0].contains(&BlockId::from_usize(1)));
        assert!(cfg.successors[0].contains(&BlockId::from_usize(2)));
        for target in &[BlockId::from_usize(1), BlockId::from_usize(2)] {
            assert_eq!(
                cfg.predecessors[target.as_usize()],
                vec![BlockId::from_usize(0)]
            );
        }
    }

    #[test]
    fn cfg_switch_int_multiple_targets() {
        let disc = AmirOperand::Constant(AmirConstant::Bool(false));
        let targets = vec![
            (1i128, BlockId::from_usize(1), Vec::new()),
            (2i128, BlockId::from_usize(2), Vec::new()),
        ];
        let blocks = vec![
            block(
                0,
                AmirTerminator::SwitchInt {
                    discriminant: disc,
                    targets,
                    otherwise: (BlockId::from_usize(3), Vec::new()),
                },
            ),
            block(1, AmirTerminator::Return),
            block(2, AmirTerminator::Return),
            block(3, AmirTerminator::Return),
        ];
        let cfg = compute_cfg_edges(&blocks);
        assert_eq!(cfg.successors[0].len(), 3);
        assert!(cfg.successors[0].contains(&BlockId::from_usize(1)));
        assert!(cfg.successors[0].contains(&BlockId::from_usize(2)));
        assert!(cfg.successors[0].contains(&BlockId::from_usize(3)));
    }

    #[test]
    fn cfg_unreachable_has_no_successors() {
        let blocks = vec![
            block(0, AmirTerminator::Unreachable),
            block(1, AmirTerminator::Return),
        ];
        let cfg = compute_cfg_edges(&blocks);
        assert!(cfg.successors[0].is_empty());
        assert!(cfg.predecessors[0].is_empty());
        assert!(cfg.predecessors[1].is_empty());
    }

    #[test]
    fn cfg_out_of_bounds_target_is_skipped() {
        let blocks = vec![block(
            0,
            AmirTerminator::Goto {
                target: BlockId::from_usize(5),
                args: Vec::new(),
            },
        )];
        let cfg = compute_cfg_edges(&blocks);
        assert!(cfg.successors[0].is_empty());
    }

    #[test]
    fn cfg_diamond_merge() {
        let cond = AmirOperand::Constant(AmirConstant::Bool(true));
        let blocks = vec![
            block(
                0,
                AmirTerminator::Branch {
                    condition: cond,
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            ),
            block(
                1,
                AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
            ),
            block(
                2,
                AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
            ),
            block(3, AmirTerminator::Return),
        ];
        let cfg = compute_cfg_edges(&blocks);
        assert_eq!(cfg.predecessors[3].len(), 2);
        assert!(cfg.predecessors[3].contains(&BlockId::from_usize(1)));
        assert!(cfg.predecessors[3].contains(&BlockId::from_usize(2)));
    }

    #[test]
    fn retarget_successor_updates_both_lists() {
        let mut cfg = compute_cfg_edges(&[
            block(
                0,
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
            ),
            block(1, AmirTerminator::Return),
            block(2, AmirTerminator::Return),
        ]);
        assert!(retarget_successor(
            &mut cfg,
            BlockId::from_usize(0),
            BlockId::from_usize(1),
            BlockId::from_usize(2),
        ));
        assert_eq!(cfg.successors[0], vec![BlockId::from_usize(2)]);
        assert!(cfg.predecessors[1].is_empty());
        assert_eq!(cfg.predecessors[2], vec![BlockId::from_usize(0)]);
    }

    #[test]
    fn clear_block_removes_all_edges() {
        let mut cfg = compute_cfg_edges(&[
            block(
                0,
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
            ),
            block(1, AmirTerminator::Return),
        ]);
        clear_block(&mut cfg, BlockId::from_usize(0));
        assert!(cfg.successors[0].is_empty());
        assert!(cfg.predecessors[0].is_empty());
        assert!(cfg.predecessors[1].is_empty());
    }

    #[test]
    fn transfer_edges_moves_all_edges() {
        let mut cfg = compute_cfg_edges(&[
            block(
                0,
                AmirTerminator::Goto {
                    target: BlockId::from_usize(2),
                    args: Vec::new(),
                },
            ),
            block(1, AmirTerminator::Return),
            block(2, AmirTerminator::Return),
        ]);
        // Block 0 goes to 2. Transfer edges from 0 to 1.
        transfer_edges(&mut cfg, BlockId::from_usize(1), BlockId::from_usize(0));
        assert_eq!(cfg.successors[1], vec![BlockId::from_usize(2)]);
        assert!(cfg.successors[0].is_empty());
        assert_eq!(cfg.predecessors[2], vec![BlockId::from_usize(1)]);
    }
}
