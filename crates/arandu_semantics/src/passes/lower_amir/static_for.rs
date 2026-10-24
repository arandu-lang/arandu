//! Static loops become ordinary acyclic AMIR before any backend sees them.
//! The domain is a frozen VM result, not an AST expression interpreter.

use super::LowerCtx;
use arandu_middle::{
    DiagCode, Diagnostic, Span, SymbolTable,
    amir::{AmirOperand, AmirRvalue},
    ctfe::ConstInt,
    hir::{HirBlockId, HirForClause},
};

const MAX_PRODUCT: usize = 4096;

impl LowerCtx<'_> {
    /// Charge both statements and expressions, so one giant expression cannot
    /// bypass the finite residual-work budget of a static expansion.
    pub(super) fn charge_static_expansion(&mut self, span: Span) -> Result<(), Diagnostic> {
        self.charge_static_expansion_work(span, 1)
    }

    pub(super) fn charge_static_expansion_work(
        &mut self,
        span: Span,
        work: usize,
    ) -> Result<(), Diagnostic> {
        if self.static_expansion_depth == 0 {
            return Ok(());
        }
        self.static_expansion_remaining = self
            .static_expansion_remaining
            .checked_sub(work)
            .ok_or_else(|| {
                Diagnostic::error(
                    DiagCode::T045ComptimeLimitExceeded,
                    "static residual exceeds the lowering work budget",
                    span,
                )
            })?;
        Ok(())
    }

    pub(super) fn lower_static_for(
        &mut self,
        (lower, upper): (ConstInt, ConstInt),
        clause: &HirForClause,
        body: HirBlockId,
        occurrence_bodies: Option<&[HirBlockId]>,
        span: Span,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let invalid = || {
            Diagnostic::error(
                DiagCode::T042UnsupportedComptime,
                "static loop requires a finite integer range and one immutable binding",
                span,
            )
        };
        let HirForClause::In {
            bindings, iterable, ..
        } = clause
        else {
            return Err(invalid());
        };
        let [binding] = self.hir.pool.for_bindings_list(*bindings) else {
            return Err(invalid());
        };
        if lower.ty() != upper.ty() {
            return Err(invalid());
        }
        let count = upper
            .value()
            .checked_sub(lower.value())
            .and_then(|n| usize::try_from(n.max(0)).ok())
            .ok_or_else(|| {
                Diagnostic::error(
                    DiagCode::T045ComptimeLimitExceeded,
                    "static range is too large",
                    span,
                )
            })?;
        let product = self
            .static_expansion_product
            .checked_mul(count)
            .filter(|n| *n <= MAX_PRODUCT)
            .ok_or_else(|| {
                Diagnostic::error(
                    DiagCode::T045ComptimeLimitExceeded,
                    "nested static loops exceed the expansion limit",
                    span,
                )
                .with_primary_label("at most 4096 nested iteration occurrences")
            })?;
        if occurrence_bodies.is_some_and(|bodies| bodies.len() != count) {
            return Err(Diagnostic::ice(
                DiagCode::ICEL001,
                "static iteration body count differs from frozen domain",
                span,
            ));
        }
        if count == 0 {
            return Ok(());
        }
        let array_operand = match self
            .tc
            .type_info
            .type_interner
            .resolve(self.hir.pool.expr(*iterable).ty)
        {
            arandu_middle::types::ArType::Array(len, _) => {
                if lower.value() != 0 || i128::from(len) != upper.value() {
                    return Err(Diagnostic::ice(
                        DiagCode::ICEL001,
                        "static array length differs from its frozen domain",
                        span,
                    ));
                }
                Some(self.lower_expr(*iterable, None, symbols)?)
            }
            _ => None,
        };
        let saved_product = self.static_expansion_product;
        self.static_expansion_product = product;
        self.static_expansion_depth += 1;
        // Restore expansion state even on a source error. No partially lowered
        // function is published when its lowering returns Err.
        let result = (|| {
            let exit = self.new_block_at(span);
            for offset in 0..count {
                let body = occurrence_bodies.map_or(body, |bodies| bodies[offset]);
                let iteration = self.new_block_at(self.hir.pool.block(body).span);
                let next = self.new_block_at(span);
                self.emit_goto(iteration);
                self.seal_block(iteration);
                self.builder.current_block = Some(iteration);
                let defer_depth = self.defer_frames.len();
                let scope_depth = self.local_scopes.len();
                self.begin_local_scope();
                self.loop_stack.push((next, exit, defer_depth, scope_depth));
                // Each occurrence gets fresh locals/temps while retaining its
                // source symbol for diagnostics. SSA never reuses a definition.
                let local = self.new_local_id(binding.ty, binding.symbol, binding.span);
                let value = lower
                    .value()
                    .checked_add(i128::try_from(offset).map_err(|_| invalid())?)
                    .ok_or_else(invalid)?;
                let literal = self.intern_literal_int(value.to_string());
                let temp = self.new_temp_id(binding.ty);
                let initialization = if let Some(base) = array_operand {
                    AmirRvalue::IndexAccess {
                        base,
                        index: AmirOperand::Constant(literal),
                    }
                } else {
                    AmirRvalue::Use(AmirOperand::Constant(literal))
                };
                self.emit_assign_temp(temp, initialization);
                self.write_variable_source(local, AmirOperand::Copy(temp))?;
                self.lower_block(body, symbols)?;
                self.end_local_scope();
                self.loop_stack.pop();
                if self.builder.current_block.is_some() {
                    self.emit_goto(next);
                }
                self.seal_block(next);
                if self.builder.has_predecessor(next) {
                    self.builder.current_block = Some(next);
                } else {
                    self.builder.current_block = None;
                    break;
                }
            }
            if self.builder.current_block.is_some() {
                self.emit_goto(exit);
            }
            self.seal_block(exit);
            self.builder.current_block = self.builder.has_predecessor(exit).then_some(exit);
            Ok(())
        })();
        self.static_expansion_depth -= 1;
        self.static_expansion_product = saved_product;
        result
    }
}
