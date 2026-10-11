//! Conservative storage reuse for values confined to a single AMIR block.
//!
//! This is independent of register allocation. Parameters, return registers,
//! block parameters, repeated definitions and cross-block uses stay dedicated.
//! Backends must additionally reject addressable or non-scalar representations.

use arandu_middle::amir::visit::{for_each_stmt_operand, for_each_terminator_operand};
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirStmt, TempId};

/// Map each temporary to the representative temporary of its storage slot.
/// Exact AMIR types remain separate, even if an ABI maps them to the same type.
#[must_use]
pub fn block_local_temp_slots(func: &AmirFunc, eligible: impl Fn(TempId) -> bool) -> Vec<usize> {
    let count = func.temps.len();
    let mut definitions = vec![None; count];
    let mut repeated = vec![false; count];
    let mut uses = vec![Vec::new(); count];
    let mut dedicated = vec![false; count];
    if let Some(ret) = dedicated.first_mut() {
        *ret = true;
    }
    for &param in func
        .params
        .iter()
        .chain(func.receiver.iter().map(|r| &r.temp))
    {
        if let Some(slot) = dedicated.get_mut(param.as_usize()) {
            *slot = true;
        }
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            if let Some(slot) = dedicated.get_mut(param.id.as_usize()) {
                *slot = true;
            }
        }
        let mut position = 0;
        for stmt in func.block_stmts(block.id) {
            let def = match stmt {
                AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } => Some(*lhs),
                AmirStmt::Call { lhs: None, .. }
                | AmirStmt::Store { .. }
                | AmirStmt::Free(_)
                | AmirStmt::Destroy(_)
                | AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Nop => None,
            };
            if let Some(temp) = def
                && let Some(definition) = definitions.get_mut(temp.as_usize())
            {
                repeated[temp.as_usize()] |= definition.is_some();
                *definition = Some((block.id, position));
            }
            for_each_stmt_operand(stmt, |op| record_use(op, block.id, position, &mut uses));
            position += 1;
        }
        for_each_terminator_operand(&block.terminator, |op| {
            record_use(op, block.id, position, &mut uses);
        });
    }
    let mut intervals = Vec::new();
    for temp in &func.temps {
        let index = temp.id.as_usize();
        if dedicated[index] || repeated[index] || !eligible(temp.id) {
            continue;
        }
        let Some((block, start)) = definitions[index] else {
            continue;
        };
        // A use in the defining instruction reads the old value. Keep such
        // temps dedicated rather than assuming ordinary SSA assignment.
        if uses[index]
            .iter()
            .any(|&(owner, position)| owner != block || position <= start)
        {
            continue;
        }
        let end = uses[index]
            .iter()
            .map(|&(_, position)| position)
            .max()
            .unwrap_or(start);
        intervals.push((block, start, end, index));
    }
    intervals.sort_unstable_by_key(|&(block, start, _, index)| (block, start, index));
    let mut representatives: Vec<_> = (0..count).collect();
    let mut slots: Vec<(
        arandu_middle::types::TypeId,
        arandu_middle::amir::BlockId,
        usize,
        usize,
    )> = Vec::new();
    for (block, start, end, index) in intervals {
        let ty = func.temps[index].ty;
        if let Some((_, owner, last_use, representative)) = slots
            .iter_mut()
            .find(|slot| slot.0 == ty && (slot.1 != block || slot.2 < start))
        {
            representatives[index] = *representative;
            *owner = block;
            *last_use = end;
        } else {
            slots.push((ty, block, end, index));
        }
    }
    representatives
}

fn record_use(
    operand: &AmirOperand,
    block: arandu_middle::amir::BlockId,
    position: usize,
    uses: &mut [Vec<(arandu_middle::amir::BlockId, usize)>],
) {
    if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand
        && let Some(entries) = uses.get_mut(temp.as_usize())
    {
        entries.push((block, position));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_middle::amir::{AmirBasicBlock, AmirStmtTable, AmirTemp, AmirTerminator, BlockId};
    use arandu_middle::layout::DenseRange;
    use arandu_middle::types::{ArType, Primitive, TypeInterner};

    fn fixture() -> AmirFunc {
        let interner = TypeInterner::new();
        let ty = interner.intern(ArType::Primitive(Primitive::Int));
        let mut statements = AmirStmtTable::new();
        for (dest, source) in [(1, 5), (2, 1), (3, 2), (4, 3), (0, 4)] {
            statements.push(AmirStmt::Assign {
                lhs: TempId(dest),
                rhs: arandu_middle::amir::AmirRvalue::Use(AmirOperand::Copy(TempId(source))),
            });
        }
        let block = AmirBasicBlock {
            id: BlockId(0),
            statements: DenseRange::new(0, 5),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        };
        AmirFunc {
            symbol: arandu_middle::SymbolId::new(0, 0),
            return_type: ty,
            receiver: None,
            params: vec![TempId(5)],
            locals: vec![],
            temps: (0..6)
                .map(|id| AmirTemp {
                    id: TempId(id),
                    ty,
                    is_copy: true,
                    is_nullable: false,
                    span: arandu_middle::Span::new(0, 0, 0),
                })
                .collect(),
            cfg: arandu_middle::cfg::compute_cfg_edges(std::slice::from_ref(&block)),
            blocks: vec![block],
            block_params: vec![],
            stmts: statements,
        }
    }

    #[test]
    fn disjoint_intervals_reuse_but_inputs_and_return_stay_dedicated() {
        let func = fixture();
        let slots = block_local_temp_slots(&func, |_| true);
        assert_eq!(slots, [0, 1, 2, 1, 2, 5]);
        assert_eq!(slots, block_local_temp_slots(&func, |_| true));
        assert_eq!(block_local_temp_slots(&func, |_| false), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn cross_block_uses_and_redefinitions_prevent_reuse() {
        let mut func = fixture();
        let instruction = func.stmts.push(AmirStmt::Assign {
            lhs: TempId(0),
            rhs: arandu_middle::amir::AmirRvalue::Use(AmirOperand::Copy(TempId(1))),
        });
        func.blocks.push(AmirBasicBlock {
            id: BlockId(1),
            statements: DenseRange::new(instruction.as_usize(), 1),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        });
        let slots = block_local_temp_slots(&func, |_| true);
        assert_eq!(slots[1], 1);
        assert_ne!(slots[1], slots[3]);
        // Both definitions of t0 remain dedicated as well.
        assert_eq!(slots[0], 0);
    }

    #[test]
    fn exact_types_are_not_coalesced() {
        let mut func = fixture();
        let interner = TypeInterner::new();
        interner.intern(ArType::Primitive(Primitive::Int));
        func.temps[3].ty = interner.intern(ArType::Primitive(Primitive::Bool));
        let slots = block_local_temp_slots(&func, |_| true);
        assert_ne!(slots[1], slots[3]);
    }
}
