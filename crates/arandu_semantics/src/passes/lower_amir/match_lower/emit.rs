use super::{LowerCtx, MatchSwitchContext, MatchSwitchPlan, OtherwisePlan};
use crate::SymbolTable;
use crate::amir::{AmirOperand, AmirRvalue, AmirTerminator, BlockId, TempId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::{HirMatchArm, HirMatchArmBody, HirPattern};
use crate::passes::type_checker::types::ArType;
use arandu_middle::Span;

impl LowerCtx<'_> {
    fn current_block_or_error(&self, span: Span) -> Result<BlockId, Diagnostic> {
        self.builder.current_block.ok_or_else(|| {
            Diagnostic::error(
                DiagCode::L001LoweringUnresolvedSymbol,
                "lowering error: missing current basic block for match switch",
                span,
            )
        })
    }

    pub(super) fn emit_match_switch(
        &mut self,
        plan: MatchSwitchPlan,
        ctx: MatchSwitchContext<'_>,
        dest: TempId,
    ) -> Result<(), Diagnostic> {
        let targets: Vec<(i128, BlockId)> = plan.arms.iter().map(|a| (a.value, a.block)).collect();

        let entry_bb = self.current_block_or_error(ctx.span)?;
        let otherwise_bb = self.new_block();

        self.builder.current_block = Some(entry_bb);
        self.emit_switch_int(plan.discriminant, targets, otherwise_bb);
        self.seal_block(otherwise_bb);
        for sw in &plan.arms {
            self.seal_block(sw.block);
        }

        for sw in &plan.arms {
            self.builder.current_block = Some(sw.block);
            self.lower_match_arm_body(
                &ctx.arms[sw.arm_index],
                dest,
                ctx.bb_end,
                ctx.symbols,
                None,
            )?;
        }

        self.builder.current_block = Some(otherwise_bb);
        match plan.otherwise {
            OtherwisePlan::Arm(idx) => {
                self.lower_match_arm_body(&ctx.arms[idx], dest, ctx.bb_end, ctx.symbols, None)?;
            }
            OtherwisePlan::Chain(indices) => {
                self.lower_match_chain_by_indices(
                    ctx.scrutinee,
                    ctx.arms,
                    &indices,
                    dest,
                    ctx.bb_end,
                    ctx.symbols,
                )?;
            }
            OtherwisePlan::Unreachable => {
                self.set_terminator(AmirTerminator::Unreachable);
            }
        }

        Ok(())
    }

    pub(super) fn emit_match_switch_stmt(
        &mut self,
        plan: MatchSwitchPlan,
        ctx: MatchSwitchContext<'_>,
    ) -> Result<(), Diagnostic> {
        let targets: Vec<(i128, BlockId)> = plan.arms.iter().map(|a| (a.value, a.block)).collect();
        let entry_bb = self.current_block_or_error(ctx.span)?;
        let otherwise_bb = self.new_block();

        self.builder.current_block = Some(entry_bb);
        self.emit_switch_int(plan.discriminant, targets, otherwise_bb);
        self.seal_block(otherwise_bb);
        for sw in &plan.arms {
            self.seal_block(sw.block);
        }
        for sw in &plan.arms {
            self.builder.current_block = Some(sw.block);
            self.lower_match_arm_stmt(&ctx.arms[sw.arm_index], ctx.bb_end, ctx.symbols, None)?;
        }
        self.builder.current_block = Some(otherwise_bb);
        match plan.otherwise {
            OtherwisePlan::Arm(idx) => {
                self.lower_match_arm_stmt(&ctx.arms[idx], ctx.bb_end, ctx.symbols, None)?;
            }
            OtherwisePlan::Chain(indices) => {
                self.lower_match_chain_stmt_by_indices(
                    ctx.scrutinee,
                    ctx.arms,
                    &indices,
                    ctx.bb_end,
                    ctx.symbols,
                )?;
            }
            OtherwisePlan::Unreachable => {
                self.set_terminator(AmirTerminator::Unreachable);
            }
        }
        Ok(())
    }

    pub(super) fn lower_match_chain(
        &mut self,
        scrutinee: AmirOperand,
        arms: &[HirMatchArm],
        dest: TempId,
        bb_end: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let indices: Vec<usize> = (0..arms.len()).collect();
        self.lower_match_chain_by_indices(scrutinee, arms, &indices, dest, bb_end, symbols)
    }

    pub(super) fn lower_match_chain_stmt(
        &mut self,
        scrutinee: AmirOperand,
        arms: &[HirMatchArm],
        bb_end: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let indices: Vec<usize> = (0..arms.len()).collect();
        self.lower_match_chain_stmt_by_indices(scrutinee, arms, &indices, bb_end, symbols)
    }

    fn lower_match_chain_stmt_by_indices(
        &mut self,
        scrutinee: AmirOperand,
        arms: &[HirMatchArm],
        indices: &[usize],
        bb_end: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        for (i, &idx) in indices.iter().enumerate() {
            let arm = &arms[idx];
            let bb_match = self.new_block_at(arm.span);
            let bb_next = self.new_block();
            let pattern = self.hir.pool.pattern(arm.pattern);
            if let Some(guard) = arm.guard {
                let bb_guard = self.new_block();
                self.lower_pattern_test_branch(scrutinee, pattern, bb_guard, bb_next, symbols)?;
                self.seal_block(bb_guard);
                self.builder.current_block = Some(bb_guard);
                let guard_scope = self.begin_local_scope();
                self.guard_borrows.clear();
                if self.pattern_has_bindings(pattern) {
                    let root = self.scrutinee_source_place(scrutinee).ok_or_else(|| {
                        Diagnostic::ice(
                            DiagCode::ICEGEN001,
                            "guarded match scrutinee was not materialized as a place",
                            self.diag_span(pattern.span()),
                        )
                    })?;
                    self.lower_guard_pattern_borrows(
                        root,
                        pattern,
                        self.operand_type(&scrutinee),
                        symbols,
                    )?;
                }
                let guard_res = self.lower_expr(guard, None, symbols)?;
                self.guard_borrows.clear();
                self.emit_local_scope_exit_from(guard_scope);
                self.set_bool_branch(guard_res, bb_match, bb_next);
                self.end_local_scope();
                self.seal_block(bb_match);
            } else {
                self.lower_pattern_test_branch(scrutinee, pattern, bb_match, bb_next, symbols)?;
                self.seal_block(bb_match);
            }
            self.builder.current_block = Some(bb_match);
            let arm_scope = self.begin_local_scope();
            if self.pattern_has_bindings(pattern) {
                // Commit every binding only after the complete pattern and
                // guard have succeeded. Borrow-typed bindings are projected
                // from the source place; no payload is copied out of `&T`.
                self.commit_match_bindings(scrutinee, pattern, symbols)?;
            }
            self.lower_match_arm_stmt(arm, bb_end, symbols, Some(arm_scope))?;
            self.end_local_scope();
            self.builder.current_block = Some(bb_next);
            self.seal_block(bb_next);
            if i + 1 == indices.len() {
                self.set_terminator(AmirTerminator::Unreachable);
            }
        }
        Ok(())
    }

    fn lower_match_arm_stmt(
        &mut self,
        arm: &HirMatchArm,
        bb_end: BlockId,
        symbols: &SymbolTable,
        scope_depth: Option<usize>,
    ) -> Result<(), Diagnostic> {
        match &arm.body {
            HirMatchArmBody::Expr(expr_id) => {
                self.lower_expr(*expr_id, None, symbols)?;
            }
            HirMatchArmBody::Block(block) => {
                self.lower_block(*block, symbols)?;
            }
        }
        if self.builder.current_block.is_some() {
            if let Some(depth) = scope_depth {
                self.emit_local_scope_exit_from(depth);
            }
            self.emit_goto(bb_end);
        }
        Ok(())
    }

    fn lower_match_chain_by_indices(
        &mut self,
        scrutinee: AmirOperand,
        arms: &[HirMatchArm],
        indices: &[usize],
        dest: TempId,
        bb_end: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        for (i, &idx) in indices.iter().enumerate() {
            let arm = &arms[idx];
            let bb_match = self.new_block_at(arm.span);
            let bb_next = self.new_block();

            let pattern = self.hir.pool.pattern(arm.pattern);

            if let Some(guard) = arm.guard {
                let bb_guard = self.new_block();
                self.lower_pattern_test_branch(scrutinee, pattern, bb_guard, bb_next, symbols)?;
                self.seal_block(bb_guard);
                self.builder.current_block = Some(bb_guard);
                let guard_scope = self.begin_local_scope();
                self.guard_borrows.clear();
                if self.pattern_has_bindings(pattern) {
                    let root = self.scrutinee_source_place(scrutinee).ok_or_else(|| {
                        Diagnostic::ice(
                            DiagCode::ICEGEN001,
                            "guarded match scrutinee was not materialized as a place",
                            self.diag_span(pattern.span()),
                        )
                    })?;
                    self.lower_guard_pattern_borrows(
                        root,
                        pattern,
                        self.operand_type(&scrutinee),
                        symbols,
                    )?;
                }
                let guard_res = self.lower_expr(guard, None, symbols)?;
                self.guard_borrows.clear();
                self.emit_local_scope_exit_from(guard_scope);
                self.set_bool_branch(guard_res, bb_match, bb_next);
                self.end_local_scope();
                self.seal_block(bb_match);
            } else {
                self.lower_pattern_test_branch(scrutinee, pattern, bb_match, bb_next, symbols)?;
                self.seal_block(bb_match);
            }

            self.builder.current_block = Some(bb_match);
            let arm_scope = self.begin_local_scope();
            if self.pattern_has_bindings(pattern) {
                self.commit_match_bindings(scrutinee, pattern, symbols)?;
            }
            self.lower_match_arm_body(arm, dest, bb_end, symbols, Some(arm_scope))?;
            self.end_local_scope();
            self.builder.current_block = Some(bb_next);
            self.seal_block(bb_next);

            if i + 1 == indices.len() {
                self.set_terminator(AmirTerminator::Unreachable);
            }
        }
        Ok(())
    }

    /// Lower refutable enum patterns as a decision chain. The payload is not
    /// read until its outer variant is selected, and a failed nested test
    /// falls through without consuming any source place.
    fn lower_pattern_test_branch(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        success: BlockId,
        failure: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if let HirPattern::Enum {
            type_symbol,
            variant,
            variant_symbol,
            payload,
            ..
        } = pattern
        {
            let payload_patterns = self.hir.pool.pattern_list(*payload);
            let variant_id = variant_symbol.or_else(|| {
                self.tc
                    .type_info
                    .enum_variants
                    .iter()
                    .find_map(|(candidate, (owner, _))| {
                        (*owner == *type_symbol
                            && symbols.get(*candidate).name.rsplit('.').next()
                                == Some(variant.as_str()))
                        .then_some(*candidate)
                    })
            });
            if let Some(variant_id) = variant_id
                && let (Some(&tag), Some((_, shape))) = (
                    self.tc.type_info.enum_variant_tags.get(&variant_id),
                    self.tc.type_info.enum_variants.get(&variant_id),
                )
            {
                let payload_types = match shape {
                    crate::passes::type_checker::EnumPayloadShape::Unit => Vec::new(),
                    crate::passes::type_checker::EnumPayloadShape::Tuple(types) => {
                        let (_, enum_ty) = self.peel_ref_scrutinee_type(scrutinee);
                        let enum_args = match enum_ty {
                            ArType::Named(owner, args) if owner == *type_symbol => {
                                self.tc.type_info.type_interner.type_args(args)
                            }
                            _ => Vec::new(),
                        };
                        self.instantiate_enum_payload_types(
                            *type_symbol,
                            variant_id,
                            &enum_args,
                            types,
                        )
                    }
                };
                if payload_types.len() == payload_patterns.len() {
                    let matches_tag = self.lower_option_variant_test(scrutinee, tag as i128);
                    if payload_patterns.is_empty() {
                        self.set_bool_branch(matches_tag, success, failure);
                        return Ok(());
                    }
                    if payload_patterns.len() == 1
                        && matches!(
                            self.hir.pool.pattern(payload_patterns[0]),
                            HirPattern::Bind { .. } | HirPattern::Wildcard { .. }
                        )
                    {
                        self.set_bool_branch(matches_tag, success, failure);
                        return Ok(());
                    }
                    let first_payload = self.new_block();
                    self.set_bool_branch(matches_tag, first_payload, failure);
                    self.seal_block(first_payload);
                    self.builder.current_block = Some(first_payload);
                    for (index, (&payload_pattern, &payload_ty)) in
                        payload_patterns.iter().zip(&payload_types).enumerate()
                    {
                        let payload_temp = self.new_temp_id(payload_ty);
                        self.emit_assign_temp(
                            payload_temp,
                            AmirRvalue::EnumPayload {
                                value: scrutinee,
                                variant: variant_id,
                                variant_tag: tag,
                                index,
                                field_ty: payload_ty,
                                tuple_ty: if payload_types.len() > 1 {
                                    Some(self.tc.type_info.type_interner.intern(ArType::tuple(
                                        &payload_types,
                                        &self.tc.type_info.type_interner,
                                    )))
                                } else {
                                    None
                                },
                            },
                        );
                        let next = if index + 1 == payload_patterns.len() {
                            success
                        } else {
                            self.new_block()
                        };
                        self.lower_pattern_test_branch(
                            AmirOperand::Copy(payload_temp),
                            self.hir.pool.pattern(payload_pattern),
                            next,
                            failure,
                            symbols,
                        )?;
                        if index + 1 < payload_patterns.len() {
                            self.seal_block(next);
                            self.builder.current_block = Some(next);
                        }
                    }
                    return Ok(());
                }
            }
        }

        if let HirPattern::TypeTuple { name, payload, .. } = pattern {
            let (scrutinee, ty) = self.peel_ref_scrutinee_type(scrutinee);
            let (tag, payload_ty) = match (&ty, name.as_str()) {
                (ArType::Option(inner), "Some") => (Some(1), Some(*inner)),
                (ArType::Option(_), "None") => (Some(0), None),
                (ArType::Result(ok, _), "Ok") => (Some(0), Some(*ok)),
                (ArType::Result(_, err), "Err") => (Some(1), Some(*err)),
                (ArType::Poll(inner), "Ready") => (Some(0), Some(*inner)),
                (ArType::Poll(_), "Pending") => (Some(1), None),
                _ => (None, None),
            };
            if let Some(tag) = tag {
                let matches_tag = self.lower_option_variant_test(scrutinee, tag);
                let payload_patterns = self.hir.pool.pattern_list(*payload);
                if payload_patterns.is_empty() {
                    self.set_bool_branch(matches_tag, success, failure);
                    return Ok(());
                }
                if payload_patterns.len() == 1
                    && matches!(
                        self.hir.pool.pattern(payload_patterns[0]),
                        HirPattern::Bind { .. } | HirPattern::Wildcard { .. }
                    )
                {
                    self.set_bool_branch(matches_tag, success, failure);
                    return Ok(());
                }
                if payload_patterns.len() == 1
                    && let Some(payload_ty) = payload_ty
                {
                    let payload_block = self.new_block();
                    self.set_bool_branch(matches_tag, payload_block, failure);
                    self.seal_block(payload_block);
                    self.builder.current_block = Some(payload_block);
                    let payload_temp = self.new_temp_id(payload_ty);
                    self.emit_assign_temp(
                        payload_temp,
                        AmirRvalue::FieldAccess {
                            base: scrutinee,
                            field: 1,
                        },
                    );
                    return self.lower_pattern_test_branch(
                        AmirOperand::Copy(payload_temp),
                        self.hir.pool.pattern(payload_patterns[0]),
                        success,
                        failure,
                        symbols,
                    );
                }
            }
        }

        let matches = self.lower_pattern_test(scrutinee, pattern, symbols)?;
        self.set_bool_branch(matches, success, failure);
        Ok(())
    }

    fn lower_match_arm_body(
        &mut self,
        arm: &HirMatchArm,
        dest: TempId,
        bb_end: BlockId,
        symbols: &SymbolTable,
        scope_depth: Option<usize>,
    ) -> Result<(), Diagnostic> {
        match &arm.body {
            HirMatchArmBody::Expr(expr_id) => {
                self.lower_expr(*expr_id, Some(dest), symbols)?;
            }
            HirMatchArmBody::Block(block) => {
                self.lower_block_as_expr(*block, Some(dest), symbols)?;
            }
        }
        if self.builder.current_block.is_some() {
            if let Some(depth) = scope_depth {
                self.emit_local_scope_exit_from(depth);
            }
            self.emit_goto(bb_end);
        }
        Ok(())
    }
}
