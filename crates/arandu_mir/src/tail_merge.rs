//! Share identical cleanup suffixes of parameter-free return blocks.
use crate::amir::{AmirBasicBlock, AmirFunc, AmirStmt, AmirStmtTable, AmirTerminator, BlockId};
use crate::layout::DenseRange;

fn cleanup(statement: &AmirStmt) -> bool {
    matches!(
        statement,
        AmirStmt::Free(_)
            | AmirStmt::Destroy(_)
            | AmirStmt::StorageDead(_)
            | AmirStmt::Call {
                lhs: None,
                return_borrow: None,
                ..
            }
    )
}
fn same(left: &AmirStmt, right: &AmirStmt) -> bool {
    match (left, right) {
        (AmirStmt::Free(a), AmirStmt::Free(b)) => a == b,
        (AmirStmt::Destroy(a), AmirStmt::Destroy(b)) => a == b,
        (AmirStmt::StorageDead(a), AmirStmt::StorageDead(b)) => a == b,
        (
            AmirStmt::Call {
                lhs: None,
                callee: a,
                args: aa,
                return_borrow: None,
            },
            AmirStmt::Call {
                lhs: None,
                callee: b,
                args: ba,
                return_borrow: None,
            },
        ) => a == b && aa == ba,
        _ => false,
    }
}

pub(crate) fn merge_return_tails(func: &mut AmirFunc) -> bool {
    let mut groups: Vec<(Vec<AmirStmt>, Vec<BlockId>)> = Vec::new();
    for block in &func.blocks {
        if !block.params.is_empty() || !matches!(block.terminator, AmirTerminator::Return) {
            continue;
        }
        let statements: Vec<_> = func.block_stmts(block.id).collect();
        let count = statements
            .iter()
            .rev()
            .take_while(|stmt| cleanup(stmt))
            .count();
        if count == 0 {
            continue;
        }
        let tail = &statements[statements.len() - count..];
        if let Some((_, blocks)) = groups.iter_mut().find(|(previous, _)| {
            previous.len() == count && previous.iter().zip(tail).all(|(a, b)| same(a, b))
        }) {
            blocks.push(block.id);
        } else {
            groups.push((
                tail.iter().map(|stmt| (*stmt).clone()).collect(),
                vec![block.id],
            ));
        }
    }
    groups.retain(|(_, blocks)| blocks.len() > 1);
    if groups.is_empty() {
        return false;
    }
    let mut removed = vec![0usize; func.blocks.len()];
    for (tail, blocks) in &groups {
        let target = BlockId::from_usize(func.blocks.len());
        for &block in blocks {
            removed[block.as_usize()] = tail.len();
            func.blocks[block.as_usize()].terminator = AmirTerminator::Goto {
                target,
                args: Vec::new(),
            };
        }
        func.blocks.push(AmirBasicBlock {
            id: target,
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        });
    }
    let original_count = removed.len();
    let mut table = AmirStmtTable::new();
    for block in &mut func.blocks {
        let start = table.len();
        if block.id.as_usize() < original_count {
            let count = block.statements.iter_ids::<crate::amir::InstrId>().count()
                - removed[block.id.as_usize()];
            for id in block
                .statements
                .iter_ids::<crate::amir::InstrId>()
                .take(count)
            {
                table.push(func.stmts.payloads[id].clone());
            }
        } else {
            for stmt in &groups[block.id.as_usize() - original_count].0 {
                table.push(stmt.clone());
            }
        }
        block.statements = DenseRange::new(start, table.len() - start);
    }
    func.stmts = table;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
    true
}
