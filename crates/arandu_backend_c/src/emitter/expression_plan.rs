//! Adjacent, single-use expression reconstruction. No memory reads or effects move.
use arandu_middle::amir::visit::{for_each_stmt_operand, for_each_terminator_operand};
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt};
use arandu_middle::ops::{BinaryOp, UnaryOp};

pub(super) fn candidates(func: &AmirFunc, scalar: impl Fn(usize) -> bool) -> Vec<bool> {
    let mut uses = vec![0usize; func.temps.len()];
    let mut defs = vec![0usize; func.temps.len()];
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            defs[param.id.as_usize()] += 1;
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                defs[lhs.as_usize()] += 1;
            }
            for_each_stmt_operand(stmt, |operand| {
                if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand {
                    uses[temp.as_usize()] += 1;
                }
            });
        }
        for_each_terminator_operand(&block.terminator, |operand| {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand {
                uses[temp.as_usize()] += 1;
            }
        });
    }
    let mut result = vec![false; func.temps.len()];
    for block in &func.blocks {
        let statements: Vec<_> = func.block_stmts(block.id).collect();
        let mut depth = 0usize;
        for pair in statements.windows(2) {
            let AmirStmt::Assign { lhs, rhs } = pair[0] else {
                depth = 0;
                continue;
            };
            let index = lhs.as_usize();
            let pure = match rhs {
                AmirRvalue::Use(AmirOperand::Copy(_) | AmirOperand::Constant(_)) => true,
                AmirRvalue::Unary { op, .. } => matches!(op, UnaryOp::Not | UnaryOp::BitNot),
                AmirRvalue::Binary { op, .. } => matches!(
                    op,
                    BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::Mul
                        | BinaryOp::BitAnd
                        | BinaryOp::BitOr
                        | BinaryOp::BitXor
                        | BinaryOp::Equal
                        | BinaryOp::NotEqual
                        | BinaryOp::Lt
                        | BinaryOp::Gt
                        | BinaryOp::LtEqual
                        | BinaryOp::GtEqual
                ),
                _ => false,
            };
            let mut immediate = false;
            let mut scalar_operands = true;
            for_each_stmt_operand(pair[0], |operand| {
                scalar_operands &= match operand {
                    AmirOperand::Copy(temp) => scalar(temp.as_usize()),
                    AmirOperand::Constant(_) => true,
                    _ => false,
                };
            });
            for_each_stmt_operand(pair[1], |operand| {
                immediate |= matches!(operand, AmirOperand::Copy(temp) if *temp == *lhs);
            });
            let parameter = func.params.contains(lhs)
                || func.receiver.is_some_and(|receiver| receiver.temp == *lhs);
            if index != 0
                && !parameter
                && defs[index] == 1
                && uses[index] == 1
                && scalar(index)
                && scalar_operands
                && pure
                && immediate
                && depth < 8
            {
                result[index] = true;
                depth += 1;
            } else {
                depth = 0;
            }
        }
    }
    result
}
