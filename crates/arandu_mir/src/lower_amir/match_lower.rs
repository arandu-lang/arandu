use super::LowerCtx;
use crate::amir::{
    AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirTerminator, BlockId, TempId,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::IndexRange;
use crate::hir::{HirExprId, HirMatchArm, HirMatchArmBody, HirPattern, HirPatternId};
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;

struct SwitchArm {
    value: i128,
    block: BlockId,
    arm_index: usize,
}

enum ArmClass {
    UnitVariant(usize),
    IntLiteral(i128),
    Wildcard,
    Complex,
}

enum OtherwisePlan {
    Arm(usize),
    Chain(Vec<usize>),
    Unreachable,
}

struct MatchSwitchContext<'a> {
    arms: &'a [HirMatchArm],
    bb_end: BlockId,
    scrutinee: AmirOperand,
    symbols: &'a SymbolTable,
    span: Span,
}

struct MatchSwitchPlan {
    discriminant: AmirOperand,
    arms: Vec<SwitchArm>,
    otherwise: OtherwisePlan,
}

impl LowerCtx<'_> {
    pub(crate) fn lower_match(
        &mut self,
        value_id: HirExprId,
        arms_range: &IndexRange,
        target: Option<TempId>,
        expr_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let value_expr = self.hir.pool.expr(value_id);
        let value_ty = self.resolve_ty(value_expr.ty);
        let value_ty_id = value_expr.ty;
        let value_span = value_expr.span;
        let arms = self.hir.pool.match_arms_list(*arms_range).to_vec();
        let needs_source_place = arms
            .iter()
            .any(|arm| self.pattern_has_bindings(self.hir.pool.pattern(arm.pattern)));

        self.begin_local_scope();
        let scrutinee = if self.tc.type_info.is_copy(value_ty_id) && !needs_source_place {
            self.lower_expr(value_id, None, symbols)?
        } else if let Ok(place) = self.lower_expr_to_place(value_id, symbols) {
            self.load_place(&place, value_ty_id)?
        } else {
            let value = self.lower_expr(value_id, None, symbols)?;
            let scrutinee_local = self.new_compiler_local_id(value_ty_id, value_span);
            self.locals[scrutinee_local.as_usize()].is_memory = true;
            self.emit_store_place(
                crate::amir::AmirPlace {
                    local: scrutinee_local,
                    projections: smallvec::SmallVec::new(),
                },
                value,
            )?;
            self.read_variable_source(scrutinee_local)?
        };
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        let bb_end = self.new_block();

        if let Some(plan) =
            self.build_match_switch_plan_from_ty(&value_ty, &arms, scrutinee, symbols)?
        {
            let disc = plan.discriminant;
            self.emit_match_switch(
                plan,
                MatchSwitchContext {
                    arms: &arms,
                    bb_end,
                    scrutinee: disc,
                    symbols,
                    span: value_span,
                },
                dest,
            )?;
        } else {
            self.lower_match_chain(scrutinee, &arms, dest, bb_end, symbols)?;
        }

        // If every arm returned, bb_end has no preds — do not resume there (CFG-5).
        self.finish_join(bb_end);
        self.end_local_scope();
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_match_stmt(
        &mut self,
        value_id: HirExprId,
        arms_range: &IndexRange,
        bb_end: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let value_expr = self.hir.pool.expr(value_id);
        let value_ty = self.resolve_ty(value_expr.ty);
        let value_ty_id = value_expr.ty;
        let value_span = value_expr.span;
        let arms = self.hir.pool.match_arms_list(*arms_range).to_vec();
        let needs_source_place = arms
            .iter()
            .any(|arm| self.pattern_has_bindings(self.hir.pool.pattern(arm.pattern)));

        self.begin_local_scope();
        let scrutinee = if self.tc.type_info.is_copy(value_ty_id) && !needs_source_place {
            self.lower_expr(value_id, None, symbols)?
        } else if let Ok(place) = self.lower_expr_to_place(value_id, symbols) {
            self.load_place(&place, value_ty_id)?
        } else {
            let value = self.lower_expr(value_id, None, symbols)?;
            let scrutinee_local = self.new_compiler_local_id(value_ty_id, value_span);
            self.locals[scrutinee_local.as_usize()].is_memory = true;
            self.emit_store_place(
                crate::amir::AmirPlace {
                    local: scrutinee_local,
                    projections: smallvec::SmallVec::new(),
                },
                value,
            )?;
            self.read_variable_source(scrutinee_local)?
        };

        if let Some(plan) =
            self.build_match_switch_plan_from_ty(&value_ty, &arms, scrutinee, symbols)?
        {
            let disc = plan.discriminant;
            self.emit_match_switch_stmt(
                plan,
                MatchSwitchContext {
                    arms: &arms,
                    bb_end,
                    scrutinee: disc,
                    symbols,
                    span: value_span,
                },
            )?;
        } else {
            self.lower_match_chain_stmt(scrutinee, &arms, bb_end, symbols)?;
        }
        self.finish_join(bb_end);
        self.end_local_scope();
        Ok(())
    }

    fn current_block_or_error(&self, span: Span) -> Result<BlockId, Diagnostic> {
        self.builder.current_block.ok_or_else(|| {
            Diagnostic::error(
                DiagCode::L001LoweringUnresolvedSymbol,
                "lowering error: missing current basic block for match switch",
                span,
            )
        })
    }

    fn build_match_switch_plan_from_ty(
        &mut self,
        value_ty: &ArType,
        arms: &[HirMatchArm],
        scrutinee: AmirOperand,
        symbols: &SymbolTable,
    ) -> Result<Option<MatchSwitchPlan>, Diagnostic> {
        if arms.iter().any(|arm| arm.guard.is_some()) {
            return Ok(None);
        }
        // The switch emitter predates transactional binding extraction. Keep
        // any binding on the general chain so the complete pattern selects
        // first and bindings are projected from a stable source place.
        if arms
            .iter()
            .any(|arm| self.pattern_has_bindings(self.hir.pool.pattern(arm.pattern)))
        {
            return Ok(None);
        }
        if let ArType::Named(enum_id, _) = value_ty {
            return self.build_enum_switch_plan(*enum_id, arms, scrutinee, symbols);
        }
        if matches!(
            value_ty,
            ArType::Primitive(Primitive::Int) | ArType::IntLiteral
        ) {
            return self.build_int_switch_plan(arms, scrutinee, symbols);
        }
        Ok(None)
    }

    fn build_enum_switch_plan(
        &mut self,
        enum_id: SymbolId,
        arms: &[HirMatchArm],
        scrutinee: AmirOperand,
        symbols: &SymbolTable,
    ) -> Result<Option<MatchSwitchPlan>, Diagnostic> {
        let mut unit_arms = Vec::new();
        let mut wildcards = Vec::new();
        let mut has_complex = false;

        for (index, arm) in arms.iter().enumerate() {
            match self.classify_enum_arm(enum_id, arm.pattern, symbols)? {
                ArmClass::UnitVariant(tag) => unit_arms.push((tag, index)),
                ArmClass::Wildcard => wildcards.push(index),
                ArmClass::IntLiteral(_) | ArmClass::Complex => has_complex = true,
            }
        }

        if has_complex || unit_arms.is_empty() {
            return Ok(None);
        }

        let tmp_tag = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tmp_tag, AmirRvalue::Discriminant { value: scrutinee });
        let disc = AmirOperand::Copy(tmp_tag);

        let switch_arms: Vec<SwitchArm> = unit_arms
            .into_iter()
            .map(|(tag, arm_index)| SwitchArm {
                value: tag as i128,
                block: self.new_block_at(arms[arm_index].span),
                arm_index,
            })
            .collect();

        let otherwise = match wildcards.len() {
            0 => OtherwisePlan::Unreachable,
            1 => OtherwisePlan::Arm(wildcards[0]),
            _ => OtherwisePlan::Chain(wildcards),
        };

        Ok(Some(MatchSwitchPlan {
            discriminant: disc,
            arms: switch_arms,
            otherwise,
        }))
    }

    fn build_int_switch_plan(
        &mut self,
        arms: &[HirMatchArm],
        scrutinee: AmirOperand,
        symbols: &SymbolTable,
    ) -> Result<Option<MatchSwitchPlan>, Diagnostic> {
        let mut literals = Vec::new();
        let mut rest = Vec::new();

        for (index, arm) in arms.iter().enumerate() {
            match self.classify_int_arm(arm.pattern, symbols)? {
                ArmClass::IntLiteral(v) => literals.push((v, index)),
                ArmClass::Wildcard => rest.push(index),
                ArmClass::UnitVariant(_) | ArmClass::Complex => rest.push(index),
            }
        }

        if literals.is_empty() {
            return Ok(None);
        }

        let switch_arms: Vec<SwitchArm> = literals
            .into_iter()
            .map(|(value, arm_index)| SwitchArm {
                value,
                block: self.new_block_at(arms[arm_index].span),
                arm_index,
            })
            .collect();

        let otherwise = match rest.len() {
            0 => OtherwisePlan::Unreachable,
            1 => OtherwisePlan::Arm(rest[0]),
            _ => OtherwisePlan::Chain(rest),
        };

        Ok(Some(MatchSwitchPlan {
            discriminant: scrutinee,
            arms: switch_arms,
            otherwise,
        }))
    }

    fn emit_match_switch(
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

    fn emit_match_switch_stmt(
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

    fn lower_match_chain(
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

    fn lower_match_chain_stmt(
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

    fn pattern_has_bindings(&self, pattern: &HirPattern) -> bool {
        match pattern {
            HirPattern::Bind { .. } => true,
            HirPattern::Enum { payload, .. } | HirPattern::TypeTuple { payload, .. } => self
                .hir
                .pool
                .pattern_list(*payload)
                .iter()
                .any(|id| self.pattern_has_bindings(self.hir.pool.pattern(*id))),
            HirPattern::Struct { fields, .. } => {
                self.hir.pool.field_pattern_list(*fields).iter().any(|id| {
                    let field = self.hir.pool.field_pattern(*id);
                    field.pattern.is_none_or(|pattern| {
                        self.pattern_has_bindings(self.hir.pool.pattern(pattern))
                    })
                })
            }
            HirPattern::Tuple { items, .. } | HirPattern::Or { alts: items, .. } => self
                .hir
                .pool
                .pattern_list(*items)
                .iter()
                .any(|id| self.pattern_has_bindings(self.hir.pool.pattern(*id))),
            HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
                false
            }
        }
    }

    fn scrutinee_source_place(&self, scrutinee: AmirOperand) -> Option<AmirPlace> {
        let (AmirOperand::Copy(temp) | AmirOperand::Move(temp)) = scrutinee else {
            return None;
        };
        self.temp_place_origins
            .get(temp.as_usize())
            .cloned()
            .flatten()
    }

    fn pattern_bindings_are_copy(&self, pattern: &HirPattern) -> bool {
        match pattern {
            HirPattern::Bind { symbol, .. } => self
                .tc
                .type_info
                .decl_type_id(*symbol)
                .is_some_and(|ty| self.tc.type_info.is_copy(ty)),
            HirPattern::Enum { payload, .. } | HirPattern::TypeTuple { payload, .. } => self
                .hir
                .pool
                .pattern_list(*payload)
                .iter()
                .all(|id| self.pattern_bindings_are_copy(self.hir.pool.pattern(*id))),
            HirPattern::Struct { fields, .. } => {
                self.hir.pool.field_pattern_list(*fields).iter().all(|id| {
                    let field = self.hir.pool.field_pattern(*id);
                    if let Some(pattern) = field.pattern {
                        self.pattern_bindings_are_copy(self.hir.pool.pattern(pattern))
                    } else {
                        self.tc
                            .resolved
                            .definitions
                            .get(&crate::NodeKey::from(field.span))
                            .and_then(|symbol| self.tc.type_info.decl_type_id(*symbol))
                            .is_some_and(|ty| self.tc.type_info.is_copy(ty))
                    }
                })
            }
            HirPattern::Tuple { items, .. } | HirPattern::Or { alts: items, .. } => self
                .hir
                .pool
                .pattern_list(*items)
                .iter()
                .all(|id| self.pattern_bindings_are_copy(self.hir.pool.pattern(*id))),
            HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
                true
            }
        }
    }

    fn commit_match_bindings(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let scrutinee_ty = self.operand_type(&scrutinee);
        if self.pattern_bindings_are_copy(pattern)
            && !matches!(
                scrutinee_ty,
                ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
            )
        {
            // Copy payload extraction must use the enum-aware rvalue. Besides
            // handling ordinary aggregate payloads, this decodes pointer-tag
            // enums whose payload is carried in the tag word itself.
            let _ = self.lower_pattern_match(scrutinee, pattern, symbols)?;
            return Ok(());
        }

        let root = self.scrutinee_source_place(scrutinee).ok_or_else(|| {
            Diagnostic::ice(
                DiagCode::ICEGEN001,
                "selected match scrutinee was not materialized as a place",
                self.diag_span(pattern.span()),
            )
        })?;
        self.lower_pattern_bindings_from_place(root, pattern, scrutinee_ty, symbols)
    }

    fn lower_guard_pattern_borrows(
        &mut self,
        root: AmirPlace,
        pattern: &HirPattern,
        value_ty: ArType,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.lower_pattern_places_at(root, pattern, value_ty, symbols, false)
    }

    fn lower_pattern_bindings_from_place(
        &mut self,
        root: AmirPlace,
        pattern: &HirPattern,
        value_ty: ArType,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.lower_pattern_places_at(root, pattern, value_ty, symbols, true)
    }

    /// Traverse the pattern against a stable source place. Guard mode creates
    /// shared aliases for tests; commit mode moves each binding from its exact
    /// projected place after the whole pattern and guard have succeeded.
    fn lower_pattern_places_at(
        &mut self,
        mut place: AmirPlace,
        pattern: &HirPattern,
        mut value_ty: ArType,
        symbols: &SymbolTable,
        commit: bool,
    ) -> Result<(), Diagnostic> {
        match pattern {
            HirPattern::Bind { symbol, span, .. } => {
                let bound_ty = self
                    .tc
                    .type_info
                    .decl_type_id(*symbol)
                    .unwrap_or_else(|| self.intern_ty_ref(&value_ty));
                if commit {
                    let local = self.new_local_id(bound_ty, *symbol, *span);
                    let bound_type = self.tc.type_info.resolve_type_id(bound_ty);
                    let source_ty_id = self.tc.type_info.type_interner.intern(value_ty.clone());
                    let source_is_already_reference = bound_ty == source_ty_id;
                    let value = if matches!(bound_type, ArType::Ref(_) | ArType::RefMut(_))
                        && !source_is_already_reference
                    {
                        let borrowed = self.new_temp_id(bound_ty);
                        let borrow = if matches!(
                            self.tc.type_info.resolve_type_id(bound_ty),
                            ArType::RefMut(_)
                        ) {
                            AmirRvalue::BorrowMut(place.clone())
                        } else {
                            AmirRvalue::Borrow(place.clone())
                        };
                        self.emit_assign_temp(borrowed, borrow);
                        AmirOperand::Copy(borrowed)
                    } else {
                        self.load_place(&place, bound_ty)?
                    };
                    self.emit_store_place(
                        AmirPlace {
                            local,
                            projections: smallvec::SmallVec::new(),
                        },
                        value,
                    )?;
                } else {
                    let borrowed_ty = match self.tc.type_info.resolve_type_id(bound_ty) {
                        ArType::Ref(_) | ArType::RefMut(_) => bound_ty,
                        _ => self.intern_ty(ArType::Ref(bound_ty)),
                    };
                    let borrowed = self.new_temp_id(borrowed_ty);
                    self.emit_assign_temp(borrowed, AmirRvalue::Borrow(place));
                    let local = self.new_compiler_local_id(borrowed_ty, *span);
                    self.emit_store_place(
                        AmirPlace {
                            local,
                            projections: smallvec::SmallVec::new(),
                        },
                        AmirOperand::Copy(borrowed),
                    )?;
                    self.guard_borrows.insert(*symbol, (local, bound_ty));
                }
            }
            HirPattern::Struct {
                struct_symbol,
                fields,
                ..
            } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let struct_fields = self.tc.type_info.struct_fields.get(struct_symbol);
                let field_ids = self.hir.pool.field_pattern_list(*fields);
                for &field_id in field_ids {
                    let field = self.hir.pool.field_pattern(field_id);
                    let Some(info) =
                        struct_fields.and_then(|fields| fields.get(field.name.as_str()))
                    else {
                        continue;
                    };
                    let Some(field_symbol) = info.symbol else {
                        continue;
                    };
                    let field_ty = self.tc.type_info.resolve_type_id(info.ty);
                    let mut field_place = place.clone();
                    self.peel_guard_reference(&mut value_ty, &mut field_place);
                    field_place
                        .projections
                        .push(AmirProjection::Field(field_symbol));
                    if let Some(pattern_id) = field.pattern {
                        let nested = self.hir.pool.pattern(pattern_id);
                        self.lower_pattern_places_at(
                            field_place,
                            nested,
                            field_ty,
                            symbols,
                            commit,
                        )?;
                    } else if let Some(symbol) = self
                        .tc
                        .resolved
                        .definitions
                        .get(&crate::NodeKey::from(field.span))
                        .copied()
                    {
                        self.lower_pattern_places_at(
                            field_place,
                            &HirPattern::Bind {
                                span: field.span,
                                name: field.name.clone(),
                                symbol,
                            },
                            field_ty,
                            symbols,
                            commit,
                        )?;
                    }
                }
            }
            HirPattern::Tuple { items, .. } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let ArType::Tuple(tuple_args) = &value_ty else {
                    return Ok(());
                };
                let item_types = self.tc.type_info.type_interner.type_args(*tuple_args);
                for (index, &pattern_id) in self.hir.pool.pattern_list(*items).iter().enumerate() {
                    let Some(&field_ty) = item_types.get(index) else {
                        continue;
                    };
                    let mut field_place = place.clone();
                    self.peel_guard_reference(&mut value_ty, &mut field_place);
                    field_place
                        .projections
                        .push(AmirProjection::TupleField(index));
                    let nested = self.hir.pool.pattern(pattern_id);
                    self.lower_pattern_places_at(
                        field_place,
                        nested,
                        self.tc.type_info.resolve_type_id(field_ty),
                        symbols,
                        commit,
                    )?;
                }
            }
            HirPattern::TypeTuple {
                span,
                name,
                payload,
            } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let payload_patterns = self.hir.pool.pattern_list(*payload);
                match (&value_ty, name.as_str()) {
                    (ArType::Option(inner), "Some") if payload_patterns.len() == 1 => {
                        let field_ty = *inner;
                        let mut payload_place = place;
                        payload_place.projections.push(AmirProjection::Variant(1));
                        payload_place.projections.push(AmirProjection::Payload {
                            variant_tag: 1,
                            index: 0,
                            field_ty,
                            tuple_ty: None,
                        });
                        let nested = self.hir.pool.pattern(payload_patterns[0]);
                        self.lower_pattern_places_at(
                            payload_place,
                            nested,
                            self.tc.type_info.resolve_type_id(field_ty),
                            symbols,
                            commit,
                        )?;
                    }
                    (ArType::Result(ok, err), "Ok" | "Err") if payload_patterns.len() == 1 => {
                        let (tag, field_ty) = if name.as_str() == "Ok" {
                            (0, *ok)
                        } else {
                            (1, *err)
                        };
                        let mut payload_place = place;
                        payload_place.projections.push(AmirProjection::Variant(tag));
                        payload_place.projections.push(AmirProjection::Payload {
                            variant_tag: tag,
                            index: 0,
                            field_ty,
                            tuple_ty: None,
                        });
                        let nested = self.hir.pool.pattern(payload_patterns[0]);
                        self.lower_pattern_places_at(
                            payload_place,
                            nested,
                            self.tc.type_info.resolve_type_id(field_ty),
                            symbols,
                            commit,
                        )?;
                    }
                    (ArType::Poll(inner), "Ready") if payload_patterns.len() == 1 => {
                        let field_ty = *inner;
                        let mut payload_place = place;
                        payload_place.projections.push(AmirProjection::Variant(0));
                        payload_place.projections.push(AmirProjection::Payload {
                            variant_tag: 0,
                            index: 0,
                            field_ty,
                            tuple_ty: None,
                        });
                        let nested = self.hir.pool.pattern(payload_patterns[0]);
                        self.lower_pattern_places_at(
                            payload_place,
                            nested,
                            self.tc.type_info.resolve_type_id(field_ty),
                            symbols,
                            commit,
                        )?;
                    }
                    _ => {
                        let _ = span;
                    }
                }
            }
            HirPattern::Enum {
                type_symbol,
                variant,
                variant_symbol,
                payload,
                ..
            } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let payload_patterns = self.hir.pool.pattern_list(*payload);
                let Some(variant_id) =
                    variant_symbol.or_else(|| {
                        self.tc.type_info.enum_variants.iter().find_map(
                            |(candidate, (owner, _))| {
                                (*owner == *type_symbol
                                    && symbols.get(*candidate).name.rsplit('.').next()
                                        == Some(variant.as_str()))
                                .then_some(*candidate)
                            },
                        )
                    })
                else {
                    return Ok(());
                };
                let Some(&tag) = self.tc.type_info.enum_variant_tags.get(&variant_id) else {
                    return Ok(());
                };
                let Some((_, crate::passes::type_checker::EnumPayloadShape::Tuple(payload_tys))) =
                    self.tc.type_info.enum_variants.get(&variant_id)
                else {
                    return Ok(());
                };
                let enum_args = match &value_ty {
                    ArType::Named(id, args) if id == type_symbol => {
                        self.tc.type_info.type_interner.type_args(*args)
                    }
                    _ => Vec::new(),
                };
                let payload_tys = self.instantiate_enum_payload_types(
                    *type_symbol,
                    variant_id,
                    &enum_args,
                    payload_tys,
                );
                let tuple_ty = if payload_tys.len() > 1 {
                    let tuple = ArType::tuple(&payload_tys, &self.tc.type_info.type_interner);
                    Some(self.tc.type_info.type_interner.intern(tuple))
                } else {
                    None
                };
                for (index, (&pattern_id, &field_ty)) in
                    payload_patterns.iter().zip(&payload_tys).enumerate()
                {
                    let mut payload_place = place.clone();
                    payload_place.projections.push(AmirProjection::Variant(tag));
                    payload_place.projections.push(AmirProjection::Payload {
                        variant_tag: tag,
                        index,
                        field_ty,
                        tuple_ty,
                    });
                    let nested = self.hir.pool.pattern(pattern_id);
                    self.lower_pattern_places_at(
                        payload_place,
                        nested,
                        self.tc.type_info.resolve_type_id(field_ty),
                        symbols,
                        commit,
                    )?;
                }
            }
            HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
            }
            HirPattern::Or { alts, .. } => {
                let alternatives = self.hir.pool.pattern_list(*alts);
                if alternatives
                    .iter()
                    .any(|id| self.pattern_has_bindings(self.hir.pool.pattern(*id)))
                {
                    return Err(Diagnostic::error(
                        DiagCode::U001FeatureNotSupported,
                        "guarded or-pattern bindings cannot yet be borrowed transactionally",
                        pattern.span(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn peel_guard_reference(&mut self, ty: &mut ArType, place: &mut AmirPlace) {
        for _ in 0..4 {
            match ty {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    *ty = self.resolve_ty(*inner);
                    place.projections.push(AmirProjection::Deref);
                    // A projected place is addressed through this scalar
                    // reference local after dummy SSA loads/stores are pruned.
                    self.mark_local_materialized(place.local);
                }
                _ => break,
            }
        }
    }

    fn lower_option_variant_test(&mut self, scrutinee: AmirOperand, tag: i128) -> AmirOperand {
        let tag_temp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_temp, AmirRvalue::Discriminant { value: scrutinee });
        let expected = AmirOperand::Constant(self.intern_literal_int(tag.to_string()));
        let matches = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            matches,
            AmirRvalue::Binary {
                op: crate::ops::BinaryOp::Equal,
                left: AmirOperand::Copy(tag_temp),
                right: expected,
            },
        );
        AmirOperand::Copy(matches)
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

    fn classify_enum_arm(
        &self,
        enum_id: SymbolId,
        pattern_id: HirPatternId,
        symbols: &SymbolTable,
    ) -> Result<ArmClass, Diagnostic> {
        let pattern = self.hir.pool.pattern(pattern_id);
        match pattern {
            crate::hir::HirPattern::Wildcard { .. } => Ok(ArmClass::Wildcard),
            // HirPattern::Enum carries a pre-resolved variant_symbol, enabling
            // the O(1) fast path in enum_variant_tag.
            crate::hir::HirPattern::Enum {
                variant,
                variant_symbol,
                payload,
                ..
            } => {
                if !payload.is_empty() {
                    return Ok(ArmClass::Complex);
                }
                Ok(ArmClass::UnitVariant(self.enum_variant_tag(
                    enum_id,
                    variant,
                    *variant_symbol,
                    symbols,
                )?))
            }
            // HirPattern::TypeTuple has no pre-resolved symbol; fall back to
            // the string-based lookup path.
            crate::hir::HirPattern::TypeTuple { name, payload, .. } => {
                if !payload.is_empty() {
                    return Ok(ArmClass::Complex);
                }
                Ok(ArmClass::UnitVariant(
                    self.enum_variant_tag(enum_id, name, None, symbols)?,
                ))
            }
            _ => Ok(ArmClass::Complex),
        }
    }

    fn classify_int_arm(
        &self,
        pattern_id: HirPatternId,
        _symbols: &SymbolTable,
    ) -> Result<ArmClass, Diagnostic> {
        let pattern = self.hir.pool.pattern(pattern_id);
        match pattern {
            crate::hir::HirPattern::Wildcard { .. } => Ok(ArmClass::Wildcard),
            crate::hir::HirPattern::Literal { expr, .. } => {
                let lit_expr = self.hir.pool.expr(*expr);
                if let Some(v) = self.literal_to_i128(lit_expr) {
                    Ok(ArmClass::IntLiteral(v))
                } else {
                    Ok(ArmClass::Complex)
                }
            }
            crate::hir::HirPattern::Range { .. } => Ok(ArmClass::Complex),
            _ => Ok(ArmClass::Complex),
        }
    }

    fn enum_variant_tag(
        &self,
        enum_id: SymbolId,
        variant: &str,
        variant_symbol: Option<SymbolId>,
        symbols: &SymbolTable,
    ) -> Result<usize, Diagnostic> {
        // Fast path: O(1) lookup via the pre-computed tag map populated during
        // collect_type_shapes. This avoids scanning all HIR decls and doing
        // string comparisons for every match arm.
        if let Some(&tag) = variant_symbol
            .as_ref()
            .and_then(|sym| self.tc.type_info.enum_variant_tags.get(sym))
        {
            return Ok(tag);
        }

        if let Some((_, _, tag)) = self
            .tc
            .type_info
            .enum_variant_by_name(symbols, enum_id, variant)
        {
            return Ok(tag);
        }

        if symbols.is_option_type(enum_id) {
            match variant {
                "None" => return Ok(0),
                "Some" => return Ok(1),
                _ => {}
            }
        }
        if symbols.is_result_type(enum_id) {
            match variant {
                "Ok" => return Ok(0),
                "Err" => return Ok(1),
                _ => {}
            }
        }
        if symbols.is_poll_type(enum_id) {
            match variant {
                "Ready" => return Ok(0),
                "Pending" => return Ok(1),
                _ => {}
            }
        }

        // Slow fallback: resolve by name. Used when variant_symbol is None
        // (e.g. TypeTuple patterns) or when the pre-computed map was not
        // populated (should not happen in practice after collect_type_shapes).
        for &decl_id in &self.hir.decls {
            let decl = self.hir.pool.decl(decl_id);
            if let crate::hir::HirDecl::Enum(hir_enum) = decl
                && hir_enum.symbol == enum_id
            {
                for (index, v) in self
                    .hir
                    .pool
                    .enum_variants_list(hir_enum.variants)
                    .iter()
                    .enumerate()
                {
                    let name = &symbols.get(v.symbol).name;
                    if name == variant || name.ends_with(&format!(".{variant}")) {
                        return Ok(index);
                    }
                }
                break;
            }
        }
        Err(Diagnostic::error(
            crate::DiagCode::T018UndefinedField,
            format!("variant '{variant}' not found on enum"),
            crate::Span {
                file_id: 0,
                start: 0,
                end: 0,
            },
        ))
    }

    fn literal_to_i128(&self, expr: &crate::hir::HirExpr) -> Option<i128> {
        match &expr.kind {
            crate::hir::HirExprKind::Int(v) => v.parse().ok(),
            _ => None,
        }
    }
}
