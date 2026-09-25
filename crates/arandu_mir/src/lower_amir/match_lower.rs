use super::LowerCtx;
use crate::amir::{AmirOperand, AmirRvalue, AmirTerminator, BlockId, TempId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::IndexRange;
use crate::hir::{HirExprId, HirMatchArm, HirMatchArmBody, HirPatternId};
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
        let value_span = value_expr.span;

        let scrutinee = self.lower_expr(value_id, None, symbols)?;
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        let bb_end = self.new_block();

        let arms = self.hir.pool.match_arms_list(*arms_range).to_vec();

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
        let value_span = value_expr.span;

        let scrutinee = self.lower_expr(value_id, None, symbols)?;

        let arms = self.hir.pool.match_arms_list(*arms_range).to_vec();

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
            self.lower_match_arm_body(&ctx.arms[sw.arm_index], dest, ctx.bb_end, ctx.symbols)?;
        }

        self.builder.current_block = Some(otherwise_bb);
        match plan.otherwise {
            OtherwisePlan::Arm(idx) => {
                self.lower_match_arm_body(&ctx.arms[idx], dest, ctx.bb_end, ctx.symbols)?;
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
            self.lower_match_arm_stmt(&ctx.arms[sw.arm_index], ctx.bb_end, ctx.symbols)?;
        }
        self.builder.current_block = Some(otherwise_bb);
        match plan.otherwise {
            OtherwisePlan::Arm(idx) => {
                self.lower_match_arm_stmt(&ctx.arms[idx], ctx.bb_end, ctx.symbols)?;
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
            let is_match = self.lower_pattern_match(scrutinee, pattern, symbols)?;
            if let Some(guard) = arm.guard {
                let bb_guard = self.new_block();
                self.set_bool_branch(is_match, bb_guard, bb_next);
                self.seal_block(bb_guard);
                self.builder.current_block = Some(bb_guard);
                let guard_res = self.lower_expr(guard, None, symbols)?;
                self.set_bool_branch(guard_res, bb_match, bb_next);
                self.seal_block(bb_match);
            } else {
                self.set_bool_branch(is_match, bb_match, bb_next);
                self.seal_block(bb_match);
            }
            self.builder.current_block = Some(bb_match);
            self.lower_match_arm_stmt(arm, bb_end, symbols)?;
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
            let is_match = self.lower_pattern_match(scrutinee, pattern, symbols)?;

            if let Some(guard) = arm.guard {
                let bb_guard = self.new_block();
                self.set_bool_branch(is_match, bb_guard, bb_next);
                self.seal_block(bb_guard);
                self.builder.current_block = Some(bb_guard);
                let guard_res = self.lower_expr(guard, None, symbols)?;
                self.set_bool_branch(guard_res, bb_match, bb_next);
                self.seal_block(bb_match);
            } else {
                self.set_bool_branch(is_match, bb_match, bb_next);
                self.seal_block(bb_match);
            }

            self.builder.current_block = Some(bb_match);
            self.lower_match_arm_body(arm, dest, bb_end, symbols)?;
            self.builder.current_block = Some(bb_next);
            self.seal_block(bb_next);

            if i + 1 == indices.len() {
                self.set_terminator(AmirTerminator::Unreachable);
            }
        }
        Ok(())
    }

    fn lower_match_arm_body(
        &mut self,
        arm: &HirMatchArm,
        dest: TempId,
        bb_end: BlockId,
        symbols: &SymbolTable,
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
