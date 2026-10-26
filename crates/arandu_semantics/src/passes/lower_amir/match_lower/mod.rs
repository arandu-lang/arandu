mod emit;
mod pattern;
mod plan;

use super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirOperand, BlockId, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirMatchArm, IndexRange};
use arandu_middle::Span;

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
}
