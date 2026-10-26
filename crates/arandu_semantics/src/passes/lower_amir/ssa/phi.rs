use super::super::LowerCtx;
use crate::amir::{AmirConstant, AmirOperand, AmirTerminator, BlockId, LocalId, TempId};

impl LowerCtx<'_> {
    // --- SSA / OSSA Block Arguments & Braun et al. Helpers ---

    pub(crate) fn seal_block(&mut self, block: BlockId) {
        if self.sealed_blocks.contains(&block) {
            return;
        }
        self.sealed_blocks.insert(block);

        // Resolve incomplete phis
        if let Some(incomplete) = self.incomplete_phis.remove(&block) {
            for (local, temp_id) in incomplete {
                self.add_block_parameter_operands(block, local, temp_id);
                self.simplify_phi(block, local, temp_id);
            }
        }
    }

    pub(super) fn add_block_parameter_operands(
        &mut self,
        block: BlockId,
        local: LocalId,
        _temp_id: TempId,
    ) {
        let preds = self
            .builder
            .predecessors
            .get(&block)
            .cloned()
            .unwrap_or_default();
        for pred in preds {
            let val = self.read_variable(pred, local);
            self.append_terminator_arg(pred, block, val);
        }
    }

    fn simplify_phi(&mut self, block: BlockId, local: LocalId, temp_id: TempId) -> AmirOperand {
        let preds = self
            .builder
            .predecessors
            .get(&block)
            .cloned()
            .unwrap_or_default();
        if preds.is_empty() {
            return AmirOperand::Copy(temp_id);
        }

        let mut unique_val: Option<AmirOperand> = None;
        for pred in preds {
            let val = self.read_variable(pred, local);
            if val == AmirOperand::Copy(temp_id) || val == AmirOperand::Move(temp_id) {
                continue;
            }
            if let Some(ref prev) = unique_val {
                if *prev != val {
                    return AmirOperand::Copy(temp_id);
                }
            } else {
                unique_val = Some(val);
            }
        }

        let val = unique_val.unwrap_or(AmirOperand::Constant(AmirConstant::Nil));
        // Keep `T?` temps that hold a non-Nil constant materialized — bare
        // integer constants must not replace a nullable handle (0 ≠ nil).
        if self.integer_constant_needs_type(self.temps[temp_id.as_usize()].ty, val)
            || (self.temps[temp_id.as_usize()].is_nullable
                && matches!(
                    &val,
                    AmirOperand::Constant(c) if !matches!(c, AmirConstant::Nil)
                ))
        {
            return AmirOperand::Copy(temp_id);
        }
        self.redirected_temps.insert(temp_id, val);
        val
    }

    /// A3.5: resume blocks of `Suspend` keep explicit state params (do not
    /// fold away as trivial phis) so `Suspend.args` remains the task state.
    fn is_suspend_resume_target(&self, block: BlockId) -> bool {
        let Some(preds) = self.builder.predecessors.get(&block) else {
            return false;
        };
        preds.iter().any(|pred| {
            matches!(
                &self.builder.blocks[pred.as_usize()].terminator,
                AmirTerminator::Suspend { resume, .. } if *resume == block
            )
        })
    }

    pub(crate) fn eliminate_trivial_phis(&mut self) {
        loop {
            let mut changed = false;
            for block_idx in 0..self.builder.blocks.len() {
                let block_id = BlockId::from_usize(block_idx);
                // Keep coroutine state slots materialised on resume BBs.
                if self.is_suspend_resume_target(block_id) {
                    continue;
                }
                let params = self.builder.block_params(block_id).to_vec();
                for (param_idx, p) in params.into_iter().enumerate() {
                    if self.redirected_temps.contains_key(&p.id) {
                        continue;
                    }

                    let preds = self
                        .builder
                        .predecessors
                        .get(&block_id)
                        .cloned()
                        .unwrap_or_default();
                    if preds.is_empty() {
                        continue;
                    }

                    let mut unique_val: Option<AmirOperand> = None;
                    let mut is_trivial = true;

                    for pred in preds {
                        if let Some(arg) = self.get_terminator_arg(pred, block_id, param_idx) {
                            let resolved = Self::resolve_operand(&self.redirected_temps, arg);
                            if resolved == AmirOperand::Copy(p.id)
                                || resolved == AmirOperand::Move(p.id)
                            {
                                continue;
                            }
                            if let Some(ref prev) = unique_val {
                                if *prev != resolved {
                                    is_trivial = false;
                                    break;
                                }
                            } else {
                                unique_val = Some(resolved);
                            }
                        } else {
                            is_trivial = false;
                            break;
                        }
                    }

                    if is_trivial {
                        let val = unique_val.unwrap_or(AmirOperand::Constant(AmirConstant::Nil));
                        if self.integer_constant_needs_type(self.temps[p.id.as_usize()].ty, val)
                            || (self.temps[p.id.as_usize()].is_nullable
                                && matches!(
                                    &val,
                                    AmirOperand::Constant(c) if !matches!(c, AmirConstant::Nil)
                                ))
                        {
                            // Keep the destination type: literals lack an integer
                            // type, and nullable scalars require boxing in codegen.
                            continue;
                        }
                        self.redirected_temps.insert(p.id, val);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    pub(crate) fn prune_eliminated_parameters(&mut self) {
        for block_idx in 0..self.builder.blocks.len() {
            let block_id = BlockId::from_usize(block_idx);
            // A3.5: never drop resume block params of a Suspend frontier.
            if self.is_suspend_resume_target(block_id) {
                continue;
            }
            let old_params = self.builder.take_block_params(block_id);
            let mut keep_indices = Vec::new();
            let mut new_params = Vec::new();
            for (i, p) in old_params.into_iter().enumerate() {
                if !self.redirected_temps.contains_key(&p.id) {
                    keep_indices.push(i);
                    new_params.push(p);
                }
            }
            self.builder.set_block_params(block_id, new_params);

            let preds = self
                .builder
                .predecessors
                .get(&block_id)
                .cloned()
                .unwrap_or_default();
            for pred in preds {
                self.prune_terminator_args(pred, block_id, &keep_indices);
            }
        }
    }

    fn get_terminator_arg(
        &self,
        pred: BlockId,
        target_block: BlockId,
        param_idx: usize,
    ) -> Option<AmirOperand> {
        let term = &self.builder.blocks[pred.as_usize()].terminator;
        match term {
            AmirTerminator::Goto { target, args }
            | AmirTerminator::Suspend {
                resume: target,
                args,
                ..
            } => {
                if *target == target_block {
                    args.get(param_idx).copied()
                } else {
                    None
                }
            }
            AmirTerminator::Branch {
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                if *if_true == target_block {
                    true_args.get(param_idx).copied()
                } else if *if_false == target_block {
                    false_args.get(param_idx).copied()
                } else {
                    None
                }
            }
            AmirTerminator::SwitchInt {
                targets, otherwise, ..
            } => {
                for (_, dest, target_args) in targets {
                    if *dest == target_block {
                        return target_args.get(param_idx).copied();
                    }
                }
                if otherwise.0 == target_block {
                    otherwise.1.get(param_idx).copied()
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn append_terminator_arg(&mut self, pred: BlockId, target_block: BlockId, val: AmirOperand) {
        let term = &mut self.builder.blocks[pred.as_usize()].terminator;
        match term {
            AmirTerminator::Goto { target, args } if *target == target_block => {
                args.push(val);
            }
            AmirTerminator::Suspend { resume, args, .. } if *resume == target_block => {
                args.push(val);
            }
            AmirTerminator::Branch {
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                if *if_true == target_block {
                    true_args.push(val);
                }
                if *if_false == target_block {
                    false_args.push(val);
                }
            }
            AmirTerminator::SwitchInt {
                targets, otherwise, ..
            } => {
                for (_, dest, target_args) in targets {
                    if *dest == target_block {
                        target_args.push(val);
                    }
                }
                if otherwise.0 == target_block {
                    otherwise.1.push(val);
                }
            }
            _ => {}
        }
    }

    fn prune_terminator_args(
        &mut self,
        pred: BlockId,
        target_block: BlockId,
        keep_indices: &[usize],
    ) {
        let term = &mut self.builder.blocks[pred.as_usize()].terminator;
        match term {
            AmirTerminator::Goto { target, args } if *target == target_block => {
                *args = keep_indices.iter().map(|&i| args[i]).collect();
            }
            AmirTerminator::Suspend { resume, args, .. } if *resume == target_block => {
                *args = keep_indices.iter().map(|&i| args[i]).collect();
            }
            AmirTerminator::Branch {
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                if *if_true == target_block {
                    *true_args = keep_indices.iter().map(|&i| true_args[i]).collect();
                }
                if *if_false == target_block {
                    *false_args = keep_indices.iter().map(|&i| false_args[i]).collect();
                }
            }
            AmirTerminator::SwitchInt {
                targets, otherwise, ..
            } => {
                for (_, dest, target_args) in targets {
                    if *dest == target_block {
                        *target_args = keep_indices.iter().map(|&i| target_args[i]).collect();
                    }
                }
                if otherwise.0 == target_block {
                    otherwise.1 = keep_indices.iter().map(|&i| otherwise.1[i]).collect();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn build_target_args(&mut self, target: BlockId) -> Vec<AmirOperand> {
        // Unsealed targets must not pre-fill jump args. Incomplete phis are
        // resolved only in `seal_block` via `append_terminator_arg` (Braun et al.
        // / cranelift-frontend: branch insts start with no jump args; the SSA
        // builder fills them at seal). Pre-filling here and then appending again
        // at seal duplicates operands on the while back-edge, e.g.
        //   goto header(newCap, newCap)  // wrong
        // instead of
        //   goto header(newCap, v)       // correct multi-phi header
        // Later loop-carried locals (only used after the loop) then append at
        // the wrong index and corrupt mut-ref bases after the loop.
        if !self.sealed_blocks.contains(&target) {
            return Vec::new();
        }
        let params = self.builder.block_params(target).to_vec();
        let mut args = Vec::new();
        let Some(curr) = self.builder.current_block else {
            return args;
        };
        for p in params {
            let arg = self.read_variable(curr, p.local);
            args.push(arg);
        }
        args
    }
}
