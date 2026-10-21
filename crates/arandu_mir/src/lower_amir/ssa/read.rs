use super::super::{LowerCtx, MoveState};
use crate::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, AmirTemp, BlockId, BlockParam,
    LocalId, TempId,
};
use crate::diagnostics::Diagnostic;
use crate::passes::type_checker::types::{ArType, Primitive};
use arandu_lexer::Span;

impl LowerCtx<'_> {
    /// Untyped pool operands cannot replace typed integer SSA values:
    /// comparison results carry bool, not operand width or signedness.
    pub(in crate::lower_amir) fn integer_constant_needs_type(
        &self,
        ty: crate::types::TypeId,
        operand: AmirOperand,
    ) -> bool {
        matches!(operand, AmirOperand::Constant(AmirConstant::Pool(_)))
            && self.with_ty(ty, |ty| matches!(ty,
                ArType::Primitive(primitive) if primitive.is_integer() && *primitive != Primitive::Int))
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
        let is_mem = super::super::is_memory_type(&ty)
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
        self.temp_place_origins.push(Some(crate::amir::AmirPlace {
            local,
            projections: smallvec::SmallVec::new(),
        }));
        // For non-default integer types, keep the typed SSA register instead
        // of substituting an untyped pool literal. Address-taken locals must
        // still load their stack home; their initial SSA value can be stale.
        // For `T?`, never redirect a non-Nil constant into the use site: bare
        // `0` would collapse with `nil` under `ne 0, nil`. Materialize via
        // Assign so codegen can box the scalar into a handle.
        let keep_materialized = !is_mem
            && (self.integer_constant_needs_type(ty_id, val)
                || (is_nullable
                    && matches!(
                        &val,
                        AmirOperand::Constant(c) if !matches!(c, AmirConstant::Nil)
                    )));

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
}
