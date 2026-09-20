use super::{LowerCtx, MoveState};
use crate::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTemp,
    AmirTerminator, BlockId, BlockParam, LocalId, TempId,
};
use crate::diagnostics::Diagnostic;
use crate::passes::type_checker::types::ArType;
use arandu_lexer::Span;
use rustc_hash::FxHashMap;

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

    /// If `local` is `T?` and `value` is a non-Nil constant, assign through a
    /// temp so codegen can box the scalar (prevents bare `0` becoming `nil`).
    fn materialize_nullable_const(&mut self, local: LocalId, value: AmirOperand) -> AmirOperand {
        let ty = self.resolve_ty(self.locals[local.as_usize()].ty);
        if !matches!(ty, ArType::Nullable(_)) {
            return value;
        }
        match &value {
            AmirOperand::Constant(AmirConstant::Nil) => value,
            AmirOperand::Constant(_) => {
                let t = self.new_temp(ty);
                self.emit_assign_temp(t, AmirRvalue::Use(value));
                AmirOperand::Copy(t)
            }
            _ => value,
        }
    }

    pub(crate) fn write_variable(&mut self, block: BlockId, local: LocalId, value: AmirOperand) {
        let value = self.materialize_nullable_const(local, value);
        if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = value {
            self.debug_bindings.push((temp, local));
        }
        self.current_def.insert((block, local), value);
    }

    pub(crate) fn canonical_debug_temp(&self, temp: TempId) -> Option<TempId> {
        match Self::resolve_operand(&self.redirected_temps, AmirOperand::Copy(temp)) {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => Some(temp),
            AmirOperand::Constant(_) | AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => {
                None
            }
        }
    }

    /// Current basic block, or ICE diagnostic if lowering lost block context.
    pub(crate) fn require_block(&self) -> Result<BlockId, Diagnostic> {
        self.builder.current_block.ok_or_else(|| {
            Diagnostic::ice(
                crate::DiagCode::ICEGEN001,
                "AMIR lower: no current basic block",
                self.diag_span(self.current_span),
            )
        })
    }

    pub(crate) fn write_variable_source(
        &mut self,
        local: LocalId,
        value: AmirOperand,
    ) -> Result<(), Diagnostic> {
        let block = self.require_block()?;
        let value = self.materialize_nullable_const(local, value);
        self.write_variable(block, local, value);
        // Emit a dummy Store statement
        self.push_stmt(AmirStmt::Store {
            lhs: AmirPlace {
                local,
                projections: smallvec::smallvec![],
            },
            rhs: value,
        });
        Ok(())
    }

    pub(crate) fn read_variable(&mut self, block: BlockId, local: LocalId) -> AmirOperand {
        if let Some(val) = self.current_def.get(&(block, local)) {
            *val
        } else {
            self.read_variable_recursive(block, local)
        }
    }

    pub(crate) fn read_variable_source(
        &mut self,
        local: LocalId,
    ) -> Result<AmirOperand, Diagnostic> {
        let block = self.require_block()?;
        let val = self.read_variable(block, local);

        self.note_local_use(local, self.current_span);

        // Emit a dummy Load statement
        let ty_id = self.locals[local.as_usize()].ty;
        let ty = self.resolve_ty(ty_id);
        let is_copy = self.tc.type_info.is_copy(ty_id);
        let is_nullable = matches!(ty, ArType::Nullable(_));
        let is_mem = super::is_memory_type(&ty)
            // F2.0: an address-taken local (`&`/`&mut`, W3.3 out-params) must be
            // re-loaded from its stack home after any call that could have
            // written through the borrow — never redirected to the SSA value.
            || self.locals[local.as_usize()].is_memory;
        let temp = self.next_temp_id();
        let span = if Self::span_is_usable(self.current_span) {
            self.current_span
        } else {
            Span::new(0, 0, 0)
        };
        self.temps.push(AmirTemp {
            id: temp,
            ty: ty_id,
            is_copy,
            is_nullable,
            span,
        });
        self.temp_states.push(MoveState::Available);
        self.temp_origins.push(Some(local));
        // For `T?`, never redirect a non-Nil constant into the use site: bare
        // `0` would collapse with `nil` under `ne 0, nil`. Materialize via
        // Assign so codegen can box the scalar into a handle.
        let keep_materialized = is_nullable
            && matches!(
                &val,
                AmirOperand::Constant(c) if !matches!(c, AmirConstant::Nil)
            );

        if keep_materialized {
            self.push_stmt(AmirStmt::Assign {
                lhs: temp,
                rhs: AmirRvalue::Use(val),
            });
        } else {
            self.push_stmt(AmirStmt::Assign {
                lhs: temp,
                rhs: AmirRvalue::Load(AmirPlace {
                    local,
                    projections: smallvec::smallvec![],
                }),
            });
            // Register redirection so that rewrite_all_operands replaces temp with the actual SSA value (val)
            // Only do this for register types; memory types must always load from their actual memory place.
            if !is_mem {
                self.redirected_temps.insert(temp, val);
            }
        }

        Ok(AmirOperand::Copy(temp))
    }

    pub(crate) fn read_variable_recursive(
        &mut self,
        block: BlockId,
        local: LocalId,
    ) -> AmirOperand {
        let val = if !self.sealed_blocks.contains(&block) {
            // Block is not sealed: generate a placeholder block parameter
            let ty_id = self.locals[local.as_usize()].ty;
            let ty = self.resolve_ty(ty_id);
            let is_copy = self.tc.type_info.is_copy(ty_id);
            let temp_id = self.new_temp(ty);
            self.temp_origins[temp_id.as_usize()] = Some(local);
            let from_name = self.locals[local.as_usize()]
                .symbol
                .map(|sym| self.tc.symbols.get(sym).name.clone());
            let param = BlockParam {
                id: temp_id,
                local,
                ty: ty_id,
                from: from_name,
                moved: !is_copy,
            };
            self.builder.push_block_param(block, param);
            let op = AmirOperand::Copy(temp_id);
            self.incomplete_phis
                .entry(block)
                .or_default()
                .push((local, temp_id));
            op
        } else {
            let preds = self
                .builder
                .predecessors
                .get(&block)
                .cloned()
                .unwrap_or_default();
            if preds.len() == 1 {
                self.read_variable(preds[0], local)
            } else {
                let ty_id = self.locals[local.as_usize()].ty;
                let ty = self.resolve_ty(ty_id);
                let is_copy = self.tc.type_info.is_copy(ty_id);
                let temp_id = self.new_temp(ty);
                self.temp_origins[temp_id.as_usize()] = Some(local);
                let from_name = self.locals[local.as_usize()]
                    .symbol
                    .map(|sym| self.tc.symbols.get(sym).name.clone());
                let param = BlockParam {
                    id: temp_id,
                    local,
                    ty: ty_id,
                    from: from_name,
                    moved: !is_copy,
                };
                self.builder.push_block_param(block, param);
                let op = AmirOperand::Copy(temp_id);
                self.write_variable(block, local, op);
                self.add_block_parameter_operands(block, local, temp_id);
                op
            }
        };
        self.write_variable(block, local, val);
        val
    }

    fn add_block_parameter_operands(&mut self, block: BlockId, local: LocalId, _temp_id: TempId) {
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
        if self.temps[temp_id.as_usize()].is_nullable
            && matches!(
                &val,
                AmirOperand::Constant(c) if !matches!(c, AmirConstant::Nil)
            )
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
                        if self.temps[p.id.as_usize()].is_nullable
                            && matches!(
                                &val,
                                AmirOperand::Constant(c) if !matches!(c, AmirConstant::Nil)
                            )
                        {
                            // leave block param in place; codegen boxes the constant
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

    fn resolve_rvalue(redirected_temps: &FxHashMap<TempId, AmirOperand>, rval: &mut AmirRvalue) {
        match rval {
            AmirRvalue::Use(op) => {
                *op = Self::resolve_operand(redirected_temps, *op);
            }
            AmirRvalue::Binary { left, right, .. } => {
                *left = Self::resolve_operand(redirected_temps, *left);
                *right = Self::resolve_operand(redirected_temps, *right);
            }
            AmirRvalue::Unary { operand, .. } => {
                *operand = Self::resolve_operand(redirected_temps, *operand);
            }
            AmirRvalue::FieldAccess { base, .. } => {
                *base = Self::resolve_operand(redirected_temps, *base);
            }
            AmirRvalue::StructLiteral { fields, .. } => {
                for (_, op) in fields {
                    *op = Self::resolve_operand(redirected_temps, *op);
                }
            }
            AmirRvalue::IndexAccess { base, index } => {
                *base = Self::resolve_operand(redirected_temps, *base);
                *index = Self::resolve_operand(redirected_temps, *index);
            }
            AmirRvalue::Array { items } => {
                for op in items {
                    *op = Self::resolve_operand(redirected_temps, *op);
                }
            }
            AmirRvalue::Tuple { items } => {
                for op in items {
                    *op = Self::resolve_operand(redirected_temps, *op);
                }
            }
            AmirRvalue::Discriminant { value } => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::EnumPayload { value, .. } => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::EnumConstruct { payload, .. } => {
                if let Some(op) = payload {
                    *op = Self::resolve_operand(redirected_temps, *op);
                }
            }

            AmirRvalue::Len(value) | AmirRvalue::SliceData(value) => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::SliceView { owner, data, len } => {
                *owner = Self::resolve_operand(redirected_temps, *owner);
                *data = Self::resolve_operand(redirected_temps, *data);
                *len = Self::resolve_operand(redirected_temps, *len);
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                *slice = Self::resolve_operand(redirected_temps, *slice);
                *start = Self::resolve_operand(redirected_temps, *start);
                *len = Self::resolve_operand(redirected_temps, *len);
            }
            AmirRvalue::StrView { owner } => {
                *owner = Self::resolve_operand(redirected_temps, *owner);
            }
            AmirRvalue::Alloc(value) => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::CoroutineReady { value, .. } => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::Load(place) => {
                Self::resolve_place(redirected_temps, place);
            }
            AmirRvalue::Borrow(place) => {
                Self::resolve_place(redirected_temps, place);
            }
            AmirRvalue::BorrowMut(place) => {
                Self::resolve_place(redirected_temps, place);
            }
            AmirRvalue::RelativeBorrow { .. } => {}
            AmirRvalue::GenInsert { value, .. } => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::GenGet { gen_ref, .. } | AmirRvalue::GenRemove { gen_ref, .. } => {
                *gen_ref = Self::resolve_operand(redirected_temps, *gen_ref);
            }
            AmirRvalue::GenSet { gen_ref, value, .. }
            | AmirRvalue::GenUpsert { gen_ref, value, .. } => {
                *gen_ref = Self::resolve_operand(redirected_temps, *gen_ref);
                *value = Self::resolve_operand(redirected_temps, *value);
            }
            AmirRvalue::StringInterp { parts } => {
                for op in parts {
                    *op = Self::resolve_operand(redirected_temps, *op);
                }
            }
            AmirRvalue::ToStr { value, .. } | AmirRvalue::BlackBox { value, .. } => {
                *value = Self::resolve_operand(redirected_temps, *value);
            }
        }
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
