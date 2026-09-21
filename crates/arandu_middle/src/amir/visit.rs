//! Shared AMIR operand visitors (RC-ANALYSIS-LOAD).
//!
//! # Design (single source of truth)
//! Analyses (liveness, move, DCE, …) and backends **must** walk operands through
//! these helpers. Adding a new `AmirRvalue` / `AmirTerminator` field without
//! updating the matching visitor is a compile-time gap only if you use an
//! exhaustive `match` here — keep matches exhaustive (no `_ => {}` for new
//! payload-bearing variants).
//!
//! Historical bugs this pattern prevents:
//! - DCE ignoring `Goto.args` / `Suspend.args` → live values deleted, hang under `--opt`
//! - Analyses that only walked branch conditions but not block-param jump args

use super::stmt::AmirTerminator;
use super::value::{AmirOperand, AmirPlace, AmirProjection, AmirRvalue};

/// Invoke `f` for every operand nested in `place` projections (e.g. index).
pub fn for_each_place_operand(place: &AmirPlace, mut f: impl FnMut(&AmirOperand)) {
    for proj in &place.projections {
        if let AmirProjection::Index(op) = proj {
            f(op);
        }
    }
}

/// Invoke `f` for every operand **used** by a terminator.
///
/// Includes:
/// - control conditions (`Branch.condition`, `SwitchInt.discriminant`, `Suspend.future`)
/// - **all jump args** that feed successor block parameters (`Goto.args`,
///   `Branch.true_args` / `false_args`, `SwitchInt` arm args, `Suspend.args`)
///
/// Omitting jump args is incorrect for SSA/block-param form: those values are
/// live uses even when no statement in the block references them.
pub fn for_each_terminator_operand(term: &AmirTerminator, mut f: impl FnMut(&AmirOperand)) {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
        AmirTerminator::Goto { args, .. } => {
            for a in args {
                f(a);
            }
        }
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            f(condition);
            for a in true_args {
                f(a);
            }
            for a in false_args {
                f(a);
            }
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            f(discriminant);
            for (_, _, args) in targets {
                for a in args {
                    f(a);
                }
            }
            for a in &otherwise.1 {
                f(a);
            }
        }
        AmirTerminator::Suspend { future, args, .. } => {
            f(future);
            for a in args {
                f(a);
            }
        }
    }
}

/// Invoke `f` for every operand used by `rvalue` (not places themselves).
pub fn for_each_rvalue_operand(rvalue: &AmirRvalue, mut f: impl FnMut(&AmirOperand)) {
    match rvalue {
        AmirRvalue::Use(op)
        | AmirRvalue::Unary { operand: op, .. }
        | AmirRvalue::Len(op)
        | AmirRvalue::SliceData(op)
        | AmirRvalue::StrBytes { source: op }
        | AmirRvalue::StrView { owner: op }
        | AmirRvalue::Alloc(op)
        | AmirRvalue::Discriminant { value: op }
        | AmirRvalue::EnumPayload { value: op, .. }
        | AmirRvalue::FieldAccess { base: op, .. }
        | AmirRvalue::ToStr { value: op, .. }
        | AmirRvalue::BlackBox { value: op, .. }
        | AmirRvalue::CoroutineReady { value: op, .. }
        | AmirRvalue::GenInsert { value: op, .. }
        | AmirRvalue::GenGet { gen_ref: op, .. }
        | AmirRvalue::GenRemove { gen_ref: op, .. } => f(op),
        AmirRvalue::GenSet { gen_ref, value, .. }
        | AmirRvalue::GenUpsert { gen_ref, value, .. } => {
            f(gen_ref);
            f(value);
        }

        AmirRvalue::Binary { left, right, .. }
        | AmirRvalue::IndexAccess {
            base: left,
            index: right,
        } => {
            f(left);
            f(right);
        }

        AmirRvalue::SliceView { owner, data, len } => {
            f(owner);
            f(data);
            f(len);
        }
        AmirRvalue::SliceSubslice { slice, start, len } => {
            f(slice);
            f(start);
            f(len);
        }

        AmirRvalue::EnumConstruct { payload, .. } => {
            if let Some(op) = payload {
                f(op);
            }
        }

        AmirRvalue::StructLiteral { fields, .. } => {
            for (_, op) in fields {
                f(op);
            }
        }

        AmirRvalue::Array { items }
        | AmirRvalue::Tuple { items }
        | AmirRvalue::StringInterp { parts: items } => {
            for op in items {
                f(op);
            }
        }

        AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
            for_each_place_operand(place, &mut f);
        }

        // A3.4: index only — no nested operands.
        AmirRvalue::RelativeBorrow { .. } => {}
    }
}

/// Invoke `f` for every place nested in `rvalue` (Load/Borrow/BorrowMut).
pub fn for_each_rvalue_place(rvalue: &AmirRvalue, mut f: impl FnMut(&AmirPlace)) {
    match rvalue {
        AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
            f(place);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::local::TempId;
    use crate::ops::BinaryOp;

    #[test]
    fn visits_binary_operands() {
        let rv = AmirRvalue::Binary {
            op: BinaryOp::Add,
            left: AmirOperand::Copy(TempId::from_usize(1)),
            right: AmirOperand::Copy(TempId::from_usize(2)),
        };
        let mut temps = Vec::new();
        for_each_rvalue_operand(&rv, |op| {
            if let AmirOperand::Copy(t) = op {
                temps.push(t.as_usize());
            }
        });
        assert_eq!(temps, vec![1, 2]);
    }

    #[test]
    fn visits_string_interp_parts() {
        let rv = AmirRvalue::StringInterp {
            parts: vec![
                AmirOperand::Copy(TempId::from_usize(0)),
                AmirOperand::Copy(TempId::from_usize(3)),
            ],
        };
        let mut n = 0;
        for_each_rvalue_operand(&rv, |_| n += 1);
        assert_eq!(n, 2);
    }

    #[test]
    fn terminator_visits_goto_jump_args() {
        use crate::amir::block::BlockId;
        use crate::amir::stmt::AmirTerminator;
        let term = AmirTerminator::Goto {
            target: BlockId::from_usize(1),
            args: vec![
                AmirOperand::Copy(TempId::from_usize(4)),
                AmirOperand::Copy(TempId::from_usize(7)),
            ],
        };
        let mut temps = Vec::new();
        for_each_terminator_operand(&term, |op| {
            if let AmirOperand::Copy(t) = op {
                temps.push(t.as_usize());
            }
        });
        assert_eq!(temps, vec![4, 7]);
    }

    #[test]
    fn terminator_visits_branch_condition_and_args() {
        use crate::amir::block::BlockId;
        use crate::amir::stmt::AmirTerminator;
        let term = AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId::from_usize(0)),
            if_true: BlockId::from_usize(1),
            true_args: vec![AmirOperand::Copy(TempId::from_usize(1))],
            if_false: BlockId::from_usize(2),
            false_args: vec![AmirOperand::Copy(TempId::from_usize(2))],
        };
        let mut temps = Vec::new();
        for_each_terminator_operand(&term, |op| {
            if let AmirOperand::Copy(t) = op {
                temps.push(t.as_usize());
            }
        });
        assert_eq!(temps, vec![0, 1, 2]);
    }
}
