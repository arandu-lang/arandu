mod builtin;
mod enum_pat;
mod match_mode;

use super::LowerCtx;
use crate::amir::{AmirOperand, BlockId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::{HirCondition, HirPattern};
use crate::passes::type_checker::types::ArType;
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;

pub(crate) struct EnumPatternInput<'a> {
    scrutinee: AmirOperand,
    span: Span,
    type_symbol: SymbolId,
    variant: &'a str,
    variant_symbol: Option<SymbolId>,
    /// Payload pattern ids (looked up by ref — no HirPattern clone).
    payload: &'a [crate::hir::HirPatternId],
    symbols: &'a SymbolTable,
    bind: bool,
}

impl LowerCtx<'_> {
    /// Peel `&` / `&mut` / `*` **types** on a match scrutinee so enum/Result/Option
    /// patterns classify correctly for `shared self` methods.
    ///
    /// Keeps the operand as-is: Cranelift `Discriminant` / `FieldAccess` already
    /// treat a Ref/Ptr-typed base as a pointer and load through it. Materializing
    /// `*ref` into an aggregate temp breaks that ABI (JIT crash on enum match).
    pub(crate) fn peel_ref_scrutinee_type(&self, scrutinee: AmirOperand) -> (AmirOperand, ArType) {
        let mut ty = self.operand_type(&scrutinee);
        for _ in 0..4 {
            match ty {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    ty = self.resolve_ty(inner);
                }
                _ => break,
            }
        }
        (scrutinee, ty)
    }

    pub(crate) fn lower_condition(
        &mut self,
        cond: &HirCondition,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        match cond {
            HirCondition::Expr(expr_id) => self.lower_expr(*expr_id, None, symbols),
            HirCondition::Is { expr, pattern } => {
                let scrutinee = self.lower_expr(*expr, None, symbols)?;
                let pat = self.hir.pool.pattern(*pattern);
                self.lower_pattern_match(scrutinee, pat, symbols)
            }
            HirCondition::And(_) => Err(Diagnostic::ice(
                DiagCode::ICEGEN001,
                "compound condition reached scalar AMIR lowering",
                self.diag_span(self.current_span),
            )),
        }
    }

    /// Lowers a condition directly into short-circuit CFG edges.
    pub(crate) fn lower_condition_branch(
        &mut self,
        cond: &HirCondition,
        if_true: BlockId,
        if_false: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if let HirCondition::And(conditions) = cond {
            let Some((last, prefix)) = conditions.split_last() else {
                self.emit_goto(if_true);
                return Ok(());
            };
            for condition in prefix {
                let next = self.new_block();
                self.lower_condition_branch(condition, next, if_false, symbols)?;
                self.seal_block(next);
                self.builder.current_block = Some(next);
            }
            return self.lower_condition_branch(last, if_true, if_false, symbols);
        }

        let condition = self.lower_condition(cond, symbols)?;
        if self.builder.current_block.is_some() {
            self.set_bool_branch(condition, if_true, if_false);
        }
        Ok(())
    }

    pub(crate) fn lower_pattern_match(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        self.lower_pattern_match_mode(scrutinee, pattern, symbols, true)
    }

    pub(crate) fn lower_pattern_test(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        self.lower_pattern_match_mode(scrutinee, pattern, symbols, false)
    }
}
