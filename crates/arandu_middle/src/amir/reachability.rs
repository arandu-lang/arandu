use super::{AmirFunc, AmirTerminator, BlockId};
use crate::BitSet;
use smallvec::{SmallVec, smallvec};

#[must_use]
pub fn reachable_blocks_dense(func: &AmirFunc) -> BitSet<BlockId> {
    let mut reachable = BitSet::with_capacity(func.blocks.len());
    if func.blocks.is_empty() {
        return reachable;
    }

    let mut stack = vec![BlockId::from_usize(0)];
    while let Some(block_id) = stack.pop() {
        let index = block_id.as_usize();
        if index >= func.blocks.len() || !reachable.insert(block_id) {
            continue;
        }

        for successor in terminator_targets(&func.blocks[index].terminator) {
            stack.push(successor);
        }
    }

    reachable
}

pub fn terminator_targets(term: &AmirTerminator) -> SmallVec<[BlockId; 2]> {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable | AmirTerminator::Trap(_) => {
            SmallVec::new()
        }
        AmirTerminator::Goto { target, .. } => smallvec![*target],
        AmirTerminator::Branch {
            if_true, if_false, ..
        } => smallvec![*if_true, *if_false],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut blocks: SmallVec<[BlockId; 2]> =
                targets.iter().map(|(_, block, _)| *block).collect();
            blocks.push(otherwise.0);
            blocks
        }
        // A3.1: suspend is a single CFG edge to the resume block.
        AmirTerminator::Suspend { resume, .. } => smallvec![*resume],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DenseRange;
    use crate::SymbolId;
    use crate::amir::AmirBasicBlock;
    use crate::amir::stmt::AmirStmtTable;
    use crate::cfg::compute_cfg_edges;
    use crate::types::ArType;

    fn func_with_blocks(blocks: Vec<AmirBasicBlock>) -> AmirFunc {
        let cfg = compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: crate::types::TypeInterner::new().intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks,
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg,
        }
    }

    #[test]
    fn empty_func_none_reachable() {
        let func = func_with_blocks(vec![]);
        let r = reachable_blocks_dense(&func);
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn single_block_always_reachable() {
        let b = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let func = func_with_blocks(vec![b]);
        let r = reachable_blocks_dense(&func);
        assert_eq!(r.len(), 1);
        assert!(r.contains(BlockId::from_usize(0)));
    }

    #[test]
    fn unreachable_block_skipped() {
        let b0 = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: Vec::new(),
            },
        };
        let b1 = AmirBasicBlock {
            id: BlockId::from_usize(1),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let b2 = AmirBasicBlock {
            id: BlockId::from_usize(2),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let func = func_with_blocks(vec![b0, b1, b2]);
        let r = reachable_blocks_dense(&func);
        assert_eq!(r.len(), 2);
        assert!(r.contains(BlockId::from_usize(0)));
        assert!(r.contains(BlockId::from_usize(1)));
        assert!(!r.contains(BlockId::from_usize(2)));
    }

    #[test]
    fn branch_makes_both_reachable() {
        let b0 = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Branch {
                condition: crate::amir::value::AmirOperand::Constant(
                    crate::amir::value::AmirConstant::Bool(true),
                ),
                if_true: BlockId::from_usize(1),
                true_args: Vec::new(),
                if_false: BlockId::from_usize(2),
                false_args: Vec::new(),
            },
        };
        let b1 = AmirBasicBlock {
            id: BlockId::from_usize(1),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let b2 = AmirBasicBlock {
            id: BlockId::from_usize(2),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let func = func_with_blocks(vec![b0, b1, b2]);
        let r = reachable_blocks_dense(&func);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn switch_int_reachable() {
        use crate::amir::value::AmirOperand;
        let b0 = AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::SwitchInt {
                discriminant: AmirOperand::Constant(crate::amir::value::AmirConstant::Bool(true)),
                targets: vec![],
                otherwise: (BlockId::from_usize(1), Vec::new()),
            },
        };
        let b1 = AmirBasicBlock {
            id: BlockId::from_usize(1),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        let func = func_with_blocks(vec![b0, b1]);
        let r = reachable_blocks_dense(&func);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn terminator_targets_return_empty() {
        assert!(terminator_targets(&AmirTerminator::Return).is_empty());
    }

    #[test]
    fn terminator_targets_unreachable_empty() {
        assert!(terminator_targets(&AmirTerminator::Unreachable).is_empty());
    }

    #[test]
    fn terminator_targets_goto() {
        let t = terminator_targets(&AmirTerminator::Goto {
            target: BlockId::from_usize(3),
            args: Vec::new(),
        });
        assert_eq!(t.len(), 1);
        assert_eq!(t[0], BlockId::from_usize(3));
    }

    #[test]
    fn terminator_targets_branch() {
        let t = terminator_targets(&AmirTerminator::Branch {
            condition: crate::amir::value::AmirOperand::Constant(
                crate::amir::value::AmirConstant::Bool(false),
            ),
            if_true: BlockId::from_usize(1),
            true_args: Vec::new(),
            if_false: BlockId::from_usize(2),
            false_args: Vec::new(),
        });
        assert_eq!(t.len(), 2);
    }
}
