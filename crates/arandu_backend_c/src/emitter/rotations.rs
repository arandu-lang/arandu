//! Adjacent complementary unsigned shifts with valid constant counts.
use arandu_middle::amir::{AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, TempId};
use arandu_middle::literal_pool::{AmirLiteralEntry, AmirLiteralPool};
use arandu_middle::ops::BinaryOp;
use arandu_middle::types::{ArType, Primitive, TypeInterner};

#[derive(Clone, Copy)]
pub(crate) struct Rotation {
    pub value: AmirOperand,
    pub count: u32,
    pub width: u32,
}

pub(super) fn plan(
    func: &AmirFunc,
    pool: &AmirLiteralPool,
    interner: &TypeInterner,
) -> (Vec<Option<Rotation>>, Vec<bool>) {
    let mut uses = vec![0usize; func.temps.len()];
    let mut definitions = vec![0usize; func.temps.len()];
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            definitions[param.id.as_usize()] += 1;
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                definitions[lhs.as_usize()] += 1;
            }
            arandu_middle::amir::visit::for_each_stmt_operand(stmt, |op| {
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                    uses[t.as_usize()] += 1;
                }
            });
        }
        arandu_middle::amir::visit::for_each_terminator_operand(&block.terminator, |op| {
            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                uses[t.as_usize()] += 1;
            }
        });
    }
    let constant = |op: &AmirOperand| -> Option<u32> {
        let AmirOperand::Constant(AmirConstant::Pool(id)) = op else {
            return None;
        };
        let AmirLiteralEntry::Int(value) = pool.get(*id) else {
            return None;
        };
        value.parse().ok()
    };
    let mut constants = vec![None; func.temps.len()];
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Assign {
            lhs,
            rhs: AmirRvalue::Use(op),
        } = stmt
            && definitions[lhs.as_usize()] == 1
            && matches!(
                interner.resolve(func.temps[lhs.as_usize()].ty),
                ArType::Primitive(Primitive::U32 | Primitive::U64)
            )
        {
            constants[lhs.as_usize()] = constant(op);
        }
    }
    let count_value = |op: &AmirOperand| match op {
        AmirOperand::Copy(t) => constants[t.as_usize()],
        _ => constant(op),
    };
    let removable_constant = |stmt: &&AmirStmt| matches!(stmt, AmirStmt::Assign { lhs, .. } if *lhs != TempId(0) && !func.params.contains(lhs) && constants[lhs.as_usize()].is_some() && uses[lhs.as_usize()] == 1);
    let mut rotations = vec![None; func.temps.len()];
    let mut elided = vec![false; func.temps.len()];
    for block in &func.blocks {
        let original: Vec<_> = func.block_stmts(block.id).collect();
        let positions: Vec<_> = original
            .iter()
            .enumerate()
            .filter(|(_, stmt)| !removable_constant(stmt))
            .map(|(i, _)| i)
            .collect();
        let statements: Vec<_> = positions.iter().map(|&i| original[i]).collect();
        for (window, triple) in statements.windows(3).enumerate() {
            let [
                AmirStmt::Assign {
                    lhs: a,
                    rhs:
                        AmirRvalue::Binary {
                            op: op_a,
                            left: x,
                            right: count_a,
                        },
                },
                AmirStmt::Assign {
                    lhs: b,
                    rhs:
                        AmirRvalue::Binary {
                            op: op_b,
                            left: y,
                            right: count_b,
                        },
                },
                AmirStmt::Assign {
                    lhs: result,
                    rhs:
                        AmirRvalue::Binary {
                            op: BinaryOp::BitOr,
                            left,
                            right,
                        },
                },
            ] = triple
            else {
                continue;
            };
            let paired = (*left == AmirOperand::Copy(*a) && *right == AmirOperand::Copy(*b))
                || (*left == AmirOperand::Copy(*b) && *right == AmirOperand::Copy(*a));
            let width = match interner.resolve(func.temps[result.as_usize()].ty) {
                ArType::Primitive(Primitive::U32) => 32,
                ArType::Primitive(Primitive::U64) => 64,
                _ => continue,
            };
            let (Some(ca), Some(cb)) = (count_value(count_a), count_value(count_b)) else {
                continue;
            };
            if !paired
                || x != y
                || !matches!(x, AmirOperand::Copy(_) | AmirOperand::Constant(_))
                || ca == 0
                || cb == 0
                || ca >= width
                || cb >= width
                || ca + cb != width
                || *a == TempId(0)
                || *b == TempId(0)
                || uses[a.as_usize()] != 1
                || uses[b.as_usize()] != 1
                || definitions[a.as_usize()] != 1
                || definitions[b.as_usize()] != 1
                || func.temps[a.as_usize()].ty != func.temps[result.as_usize()].ty
                || func.temps[b.as_usize()].ty != func.temps[result.as_usize()].ty
            {
                continue;
            }
            let count = match (op_a, op_b) {
                (BinaryOp::ShiftRight, BinaryOp::ShiftLeft) => ca,
                (BinaryOp::ShiftLeft, BinaryOp::ShiftRight) => cb,
                _ => continue,
            };
            let omitted: Vec<_> = original[positions[window]..positions[window + 2]]
                .iter()
                .filter(|stmt| removable_constant(stmt))
                .filter_map(|stmt| {
                    if let AmirStmt::Assign { lhs, .. } = stmt {
                        Some(*lhs)
                    } else {
                        None
                    }
                })
                .collect();
            if omitted
                .iter()
                .any(|t| *count_a != AmirOperand::Copy(*t) && *count_b != AmirOperand::Copy(*t))
            {
                continue;
            }
            rotations[result.as_usize()] = Some(Rotation {
                value: *x,
                count,
                width,
            });
            elided[a.as_usize()] = true;
            elided[b.as_usize()] = true;
            for temp in omitted {
                elided[temp.as_usize()] = true;
            }
        }
    }
    (rotations, elided)
}
