use super::super::LowerCtx;
use crate::amir::{
    AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator, TempId,
};
use rustc_hash::FxHashMap;

impl LowerCtx<'_> {
    pub(crate) fn rewrite_all_operands(&mut self) {
        // Rewrite all statements
        for stmt_idx in 0..self.builder.stmts.len() {
            let stmt_id = crate::amir::stmt::InstrId::from_usize(stmt_idx);
            if let Some(stmt) = self.builder.stmts.get_mut(stmt_id) {
                Self::resolve_stmt(&self.redirected_temps, stmt);
            }
        }

        // Rewrite all block terminators
        for block_idx in 0..self.builder.blocks.len() {
            let mut term = std::mem::replace(
                &mut self.builder.blocks[block_idx].terminator,
                AmirTerminator::Unreachable,
            );
            Self::resolve_terminator(&self.redirected_temps, &mut term);
            self.builder.blocks[block_idx].terminator = term;
        }
    }

    pub(crate) fn resolve_operand(
        redirected_temps: &FxHashMap<TempId, AmirOperand>,
        op: AmirOperand,
    ) -> AmirOperand {
        match op {
            AmirOperand::Copy(t) => {
                if let Some(&repl) = redirected_temps.get(&t) {
                    Self::resolve_operand(redirected_temps, repl)
                } else {
                    op
                }
            }
            AmirOperand::Move(t) => {
                if let Some(&repl) = redirected_temps.get(&t) {
                    let resolved = Self::resolve_operand(redirected_temps, repl);
                    match resolved {
                        AmirOperand::Copy(rt) => AmirOperand::Move(rt),
                        other => other,
                    }
                } else {
                    op
                }
            }
            _ => op,
        }
    }

    pub(super) fn resolve_rvalue(
        redirected_temps: &FxHashMap<TempId, AmirOperand>,
        rval: &mut AmirRvalue,
    ) {
        crate::amir::visit::for_each_rvalue_operand_mut(rval, |operand| {
            *operand = Self::resolve_operand(redirected_temps, *operand);
        });
    }

    fn resolve_place(redirected_temps: &FxHashMap<TempId, AmirOperand>, place: &mut AmirPlace) {
        for proj in &mut place.projections {
            if let AmirProjection::Index(op) = proj {
                *op = Self::resolve_operand(redirected_temps, *op);
            }
        }
    }

    fn resolve_stmt(redirected_temps: &FxHashMap<TempId, AmirOperand>, stmt: &mut AmirStmt) {
        match stmt {
            AmirStmt::Assign { lhs: _, rhs } => {
                Self::resolve_rvalue(redirected_temps, rhs);
            }
            AmirStmt::Store { lhs, rhs } => {
                Self::resolve_place(redirected_temps, lhs);
                *rhs = Self::resolve_operand(redirected_temps, *rhs);
            }
            AmirStmt::Call {
                lhs: _,
                callee,
                args,
                ..
            } => {
                *callee = Self::resolve_operand(redirected_temps, *callee);
                for arg in args {
                    *arg = Self::resolve_operand(redirected_temps, *arg);
                }
            }
            AmirStmt::Free(op) => {
                *op = Self::resolve_operand(redirected_temps, *op);
            }
            AmirStmt::Destroy(place) => {
                Self::resolve_place(redirected_temps, place);
            }
            _ => {}
        }
    }

    fn resolve_terminator(
        redirected_temps: &FxHashMap<TempId, AmirOperand>,
        term: &mut AmirTerminator,
    ) {
        match term {
            AmirTerminator::Goto { target: _, args } => {
                for arg in args {
                    *arg = Self::resolve_operand(redirected_temps, *arg);
                }
            }
            AmirTerminator::Suspend {
                future,
                resume: _,
                args,
            } => {
                *future = Self::resolve_operand(redirected_temps, *future);
                for arg in args {
                    *arg = Self::resolve_operand(redirected_temps, *arg);
                }
            }
            AmirTerminator::Branch {
                condition,
                if_true: _,
                true_args,
                if_false: _,
                false_args,
            } => {
                *condition = Self::resolve_operand(redirected_temps, *condition);
                for arg in true_args {
                    *arg = Self::resolve_operand(redirected_temps, *arg);
                }
                for arg in false_args {
                    *arg = Self::resolve_operand(redirected_temps, *arg);
                }
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                *discriminant = Self::resolve_operand(redirected_temps, *discriminant);
                for (_, _, target_args) in targets {
                    for arg in target_args {
                        *arg = Self::resolve_operand(redirected_temps, *arg);
                    }
                }
                for arg in &mut otherwise.1 {
                    *arg = Self::resolve_operand(redirected_temps, *arg);
                }
            }
            _ => {}
        }
    }
}
