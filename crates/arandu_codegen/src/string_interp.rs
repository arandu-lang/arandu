//! Admission proof for allocation-free integer parts in a materialized concat.
//!
//! AMIR ownership remains unchanged. Backends may omit a ToStr buffer and its
//! Free only when the sole consumer is one later concat in the same block,
//! followed by exactly one Free. Other uses (including jump arguments) reject
//! admission. The numeric value is captured at the original ToStr statement.

use arandu_middle::amir::visit::{for_each_stmt_operand, for_each_terminator_operand};
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt};
use arandu_middle::types::{ArType, TypeInterner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegerStringKind {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy)]
struct Candidate {
    kind: IntegerStringKind,
    block: usize,
    definition: usize,
    concat: Option<usize>,
    free: Option<usize>,
    invalid: bool,
}

/// Return one admission entry per temp. Does not rewrite AMIR, allocate runtime
/// storage, or extend string lifetimes across a control-flow boundary.
#[must_use]
pub fn integer_concat_temps(
    function: &AmirFunc,
    interner: &TypeInterner,
) -> Vec<Option<IntegerStringKind>> {
    let mut candidates: Vec<Option<Candidate>> = vec![None; function.temps.len()];
    let mut definitions = vec![0usize; function.temps.len()];
    for (block_index, block) in function.blocks.iter().enumerate() {
        for (index, statement) in function.block_stmts(block.id).enumerate() {
            let lhs = match statement {
                AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } => *lhs,
                _ => continue,
            };
            definitions[lhs.as_usize()] += 1;
            let AmirStmt::Assign {
                rhs: AmirRvalue::ToStr { src_ty, .. },
                ..
            } = statement
            else {
                continue;
            };
            let kind = match interner.resolve(*src_ty) {
                ArType::IntLiteral => IntegerStringKind::Signed,
                ArType::Primitive(p) if p.is_integer() && p.is_signed() => {
                    IntegerStringKind::Signed
                }
                ArType::Primitive(p) if p.is_integer() => IntegerStringKind::Unsigned,
                _ => continue,
            };
            candidates[lhs.as_usize()] = Some(Candidate {
                kind,
                block: block_index,
                definition: index,
                concat: None,
                free: None,
                invalid: false,
            });
        }
    }
    for parameter in &function.params {
        definitions[parameter.as_usize()] += 1;
    }
    for parameter in &function.block_params {
        definitions[parameter.id.as_usize()] += 1;
    }
    for (block_index, block) in function.blocks.iter().enumerate() {
        for (index, statement) in function.block_stmts(block.id).enumerate() {
            for_each_stmt_operand(statement, |operand| {
                let (AmirOperand::Copy(temp) | AmirOperand::Move(temp)) = operand else {
                    return;
                };
                let Some(candidate) = &mut candidates[temp.as_usize()] else {
                    return;
                };
                if candidate.block != block_index || index <= candidate.definition {
                    candidate.invalid = true;
                    return;
                }
                match statement {
                    AmirStmt::Assign {
                        rhs: AmirRvalue::StringInterp { .. },
                        ..
                    } => {
                        if candidate.concat.replace(index).is_some() {
                            candidate.invalid = true;
                        }
                    }
                    AmirStmt::Free(_) => {
                        if candidate.free.replace(index).is_some() {
                            candidate.invalid = true;
                        }
                    }
                    _ => candidate.invalid = true,
                }
            });
        }
        for_each_terminator_operand(&block.terminator, |operand| {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand
                && let Some(candidate) = &mut candidates[temp.as_usize()]
            {
                candidate.invalid = true;
            }
        });
    }
    candidates
        .into_iter()
        .enumerate()
        .map(|(temp, candidate)| {
            let candidate = candidate?;
            // Return carries temp 0 implicitly, outside operand visitors.
            if temp == 0 {
                return None;
            }
            let concat = candidate.concat?;
            let free = candidate.free?;
            (!candidate.invalid && definitions[temp] == 1 && free > concat)
                .then_some(candidate.kind)
        })
        .collect()
}
