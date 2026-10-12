//! Self-tail recursion elimination for scalar, destructor-free functions.
//!
//! The ABI entry remains separate from the loop header. Incoming arguments
//! become block arguments, retaining simultaneous assignment on back edges.
use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator,
    BlockId, BlockParam, LocalId, TempId,
};
use crate::layout::DenseRange;
use crate::types::{ArType, Primitive, TypeInterner};

fn scalar(ty: crate::types::TypeId, interner: &TypeInterner) -> bool {
    matches!(
        interner.resolve(ty),
        ArType::Void | ArType::Primitive(Primitive::Bool | Primitive::Char)
    ) || matches!(interner.resolve(ty), ArType::Primitive(primitive) if primitive.is_numeric())
}

pub(crate) fn eliminate_self_tail_calls(func: &mut AmirFunc, interner: &TypeInterner) -> bool {
    if func.blocks.is_empty()
        || func.receiver.is_some()
        || !func.blocks[0].params.is_empty()
        || !func
            .temps
            .iter()
            .all(|temp| temp.is_copy && !temp.is_nullable && scalar(temp.ty, interner))
        || !func.locals.iter().all(|local| scalar(local.ty, interner))
    {
        return false;
    }
    for block in &func.blocks {
        if matches!(block.terminator, AmirTerminator::Suspend { .. }) {
            return false;
        }
        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Destroy(_) | AmirStmt::Free(_) => return false,
                AmirStmt::Assign { lhs, rhs } => {
                    if func.params.contains(lhs)
                        || matches!(
                            rhs,
                            AmirRvalue::Borrow(_)
                                | AmirRvalue::BorrowMut(_)
                                | AmirRvalue::RelativeBorrow { .. }
                        )
                    {
                        return false;
                    }
                }
                AmirStmt::Call { lhs: Some(lhs), .. } if func.params.contains(lhs) => return false,
                _ => {}
            }
        }
    }
    let mut sites = Vec::new();
    for block in &func.blocks {
        if !matches!(block.terminator, AmirTerminator::Return) {
            continue;
        }
        let ids: Vec<_> = func
            .block_stmt_ids(block.id)
            .filter(|&id| !matches!(func.stmt(id), AmirStmt::Nop | AmirStmt::StorageDead(_)))
            .collect();
        let Some(&last) = ids.last() else {
            continue;
        };
        let (call, result, copy) = match func.stmt(last) {
            AmirStmt::Call { lhs, .. } => (last, *lhs, None),
            AmirStmt::Assign {
                lhs: TempId(0),
                rhs: AmirRvalue::Use(AmirOperand::Copy(result)),
            } if ids.len() >= 2 => (ids[ids.len() - 2], Some(*result), Some(last)),
            _ => continue,
        };
        if let AmirStmt::Call {
            lhs,
            callee: AmirOperand::FunctionRef(symbol),
            args,
            ..
        } = func.stmt(call)
            && *symbol == func.symbol
            && *lhs == result
            && (copy.is_some()
                || result == Some(TempId(0))
                || (result.is_none() && matches!(interner.resolve(func.return_type), ArType::Void)))
            && args.len() == func.params.len()
        {
            sites.push((block.id, call, copy, args.to_vec()));
        }
    }
    if sites.is_empty() {
        return false;
    }
    let header = BlockId::from_usize(func.blocks.len());
    let param_start = func.block_params.len();
    let mut replacements = Vec::new();
    for &param in &func.params {
        let mut temp = func.temps[param.as_usize()].clone();
        temp.id = TempId::from_usize(func.temps.len());
        let local = LocalId::from_usize(func.locals.len());
        func.locals.push(AmirLocal {
            id: local,
            ty: temp.ty,
            is_memory: false,
            symbol: None,
            span: temp.span,
            use_span: None,
        });
        func.block_params.push(BlockParam {
            id: temp.id,
            local,
            ty: temp.ty,
            from: None,
            moved: false,
        });
        replacements.push((param, temp.id));
        func.temps.push(temp);
    }
    for (_, call, copy, _) in &sites {
        if let Some(stmt) = func.stmts.get_mut(*call) {
            *stmt = AmirStmt::Nop;
            func.stmts.kinds[*call] = crate::amir::AmirStmtKind::Nop;
        }
        if let Some(copy) = copy
            && let Some(stmt) = func.stmts.get_mut(*copy)
        {
            *stmt = AmirStmt::Nop;
            func.stmts.kinds[*copy] = crate::amir::AmirStmtKind::Nop;
        }
    }
    for (block, _, _, args) in sites {
        func.blocks[block.as_usize()].terminator = AmirTerminator::Goto {
            target: header,
            args,
        };
    }
    for block in &mut func.blocks {
        remap_entry(&mut block.terminator, header);
    }
    for stmt in func.stmts.payloads.iter_mut() {
        crate::amir::visit::for_each_stmt_operand_mut(stmt, |operand| {
            remap_operand(operand, &replacements)
        });
    }
    for block in &mut func.blocks {
        crate::amir::visit::for_each_terminator_operand_mut(&mut block.terminator, |operand| {
            remap_operand(operand, &replacements)
        });
    }
    let mut body = func.blocks[0].clone();
    body.id = header;
    body.params = DenseRange::new(param_start, replacements.len());
    func.blocks[0] = AmirBasicBlock {
        id: BlockId(0),
        params: DenseRange::empty(),
        statements: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: header,
            args: func
                .params
                .iter()
                .map(|&temp| AmirOperand::Copy(temp))
                .collect(),
        },
    };
    func.blocks.push(body);
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
    true
}

fn remap_operand(operand: &mut AmirOperand, replacements: &[(TempId, TempId)]) {
    if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand
        && let Some((_, target)) = replacements.iter().find(|(source, _)| source == temp)
    {
        *temp = *target;
    }
}

fn remap_entry(terminator: &mut AmirTerminator, header: BlockId) {
    let replace = |target: &mut BlockId| {
        if *target == BlockId(0) {
            *target = header;
        }
    };
    match terminator {
        AmirTerminator::Goto { target, .. } => replace(target),
        AmirTerminator::Branch {
            if_true, if_false, ..
        } => {
            replace(if_true);
            replace(if_false);
        }
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            for (_, target, _) in targets {
                replace(target);
            }
            replace(&mut otherwise.0);
        }
        AmirTerminator::Suspend { resume, .. } => replace(resume),
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}
