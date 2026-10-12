//! Scalar Replacement of Aggregates (SROA) for AMIR.
//!
//! Propagates values from StructLiteral/Tuple directly into FieldAccess reads
//! within basic blocks where operands remain stable and Copy. Effects and
//! block boundaries invalidate this forwarding; typed local scalarization is
//! handled separately by `scalarize_locals`.

use crate::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, BlockId, InstrId, TempId};
use rustc_hash::FxHashMap;

/// Applies SROA to `func`.
///
/// Returns `true` if any field access was resolved to a scalar operand.
pub fn sroa(func: &mut AmirFunc) -> bool {
    let n_blocks = func.blocks.len();
    if n_blocks == 0 || func.stmts.is_empty() {
        return false;
    }

    let mut changed = false;
    let mut definitions = vec![0usize; func.temps.len()];
    for &param in func
        .params
        .iter()
        .chain(func.receiver.iter().map(|r| &r.temp))
    {
        definitions[param.as_usize()] += 1;
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            definitions[param.id.as_usize()] += 1;
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                definitions[lhs.as_usize()] += 1;
            }
        }
    }

    // Track aggregates assigned to temporaries: TempId -> field values
    let mut struct_temps: FxHashMap<TempId, Vec<AmirOperand>> = FxHashMap::default();
    let mut rewrites: Vec<(InstrId, AmirStmt)> = Vec::new();

    for bi in 0..n_blocks {
        struct_temps.clear();
        let bid = BlockId::from_usize(bi);
        let stmt_ids: Vec<InstrId> = func.block_stmt_ids(bid).collect();
        for stmt_id in stmt_ids {
            let stmt = func.stmt(stmt_id).clone();
            if matches!(
                stmt,
                AmirStmt::Call { .. }
                    | AmirStmt::Store { .. }
                    | AmirStmt::Destroy(_)
                    | AmirStmt::Free(_)
            ) {
                struct_temps.clear();
            }
            if let AmirStmt::Assign { lhs, rhs } = stmt {
                struct_temps.remove(&lhs);
                match rhs {
                    AmirRvalue::StructLiteral { fields, .. } => {
                        let field_ops: Vec<AmirOperand> =
                            fields.into_iter().map(|(_, op)| op).collect();
                        if func.temps[lhs.as_usize()].is_copy
                            && field_ops
                                .iter()
                                .all(|operand| stable_operand(operand, &definitions, func))
                        {
                            struct_temps.insert(lhs, field_ops);
                        }
                    }
                    AmirRvalue::Tuple { items } => {
                        if func.temps[lhs.as_usize()].is_copy
                            && items
                                .iter()
                                .all(|operand| stable_operand(operand, &definitions, func))
                        {
                            struct_temps.insert(lhs, items);
                        }
                    }
                    AmirRvalue::FieldAccess { base, field } => {
                        let base_temp = match base {
                            AmirOperand::Copy(t) | AmirOperand::Move(t) => Some(t),
                            _ => None,
                        };
                        if let Some(bt) = base_temp
                            && let Some(field_ops) = struct_temps.get(&bt)
                            && let Some(resolved_op) = field_ops.get(field)
                        {
                            rewrites.push((
                                stmt_id,
                                AmirStmt::Assign {
                                    lhs,
                                    rhs: AmirRvalue::Use(*resolved_op),
                                },
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if !rewrites.is_empty() {
        for (stmt_id, new_stmt) in rewrites {
            if let Some(target) = func.stmts.get_mut(stmt_id) {
                *target = new_stmt;
            }
        }
        changed = true;
    }

    changed
}

fn stable_operand(operand: &AmirOperand, definitions: &[usize], func: &AmirFunc) -> bool {
    match operand {
        AmirOperand::Copy(temp) => {
            definitions[temp.as_usize()] <= 1 && func.temps[temp.as_usize()].is_copy
        }
        AmirOperand::Constant(_) | AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => true,
        AmirOperand::Move(_) => false,
    }
}
