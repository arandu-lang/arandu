use super::{ArmClass, LowerCtx, MatchSwitchPlan, OtherwisePlan, SwitchArm};
use crate::amir::{AmirOperand, AmirRvalue};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirMatchArm, HirPatternId};
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{SymbolId, SymbolTable};

impl LowerCtx<'_> {
    pub(super) fn build_match_switch_plan_from_ty(
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
            arandu_middle::Span {
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
