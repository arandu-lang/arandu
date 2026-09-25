use super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirTerminator, TempId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::{HirExpr, HirExprId, HirExprKind, ResultCtorVariant};
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive, result_ok_err_id};

impl LowerCtx<'_> {
    pub(crate) fn expr_is_nil(expr: &HirExpr) -> bool {
        matches!(expr.kind, HirExprKind::Nil)
    }

    pub(crate) fn expr_is_result_err(&self, expr: &HirExpr) -> bool {
        match &expr.kind {
            HirExprKind::ResultCtor {
                variant: ResultCtorVariant::Err,
                ..
            } => true,
            HirExprKind::ResultCtor {
                variant:
                    ResultCtorVariant::Ok
                    | ResultCtorVariant::Some
                    | ResultCtorVariant::None
                    | ResultCtorVariant::PollReady
                    | ResultCtorVariant::PollPending,
                ..
            } => false,
            HirExprKind::Nil => false,
            _ => self.with_ty(expr.ty, |t| matches!(t, ArType::Err)) && !Self::expr_is_nil(expr),
        }
    }

    pub(crate) fn is_error_return(&self, values: &[HirExprId]) -> bool {
        if result_ok_err_id(self.func_return_type, &self.tc.type_info.type_interner).is_none() {
            return false;
        }
        match values.len() {
            0 => false,
            1 => self.expr_is_result_err(self.hir.pool.expr(values[0])),
            _ => false,
        }
    }

    pub(crate) fn lower_result_ok_field(&mut self, base: AmirOperand, dest: TempId) {
        self.emit_assign_temp(
            dest,
            AmirRvalue::FieldAccess {
                base,
                field: arandu_middle::amir::ENUM_PAYLOAD_FIELD,
            },
        );
    }

    pub(crate) fn lower_result_err_field(&mut self, base: AmirOperand, dest: TempId) {
        self.emit_assign_temp(
            dest,
            AmirRvalue::FieldAccess {
                base,
                field: arandu_middle::amir::ENUM_PAYLOAD_FIELD,
            },
        );
    }

    pub(crate) fn lower_try_result(
        &mut self,
        inner_id: HirExprId,
        target: Option<TempId>,
        expr_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let inner = self.hir.pool.expr(inner_id);
        let ok_err = result_ok_err_id(inner.ty, &self.tc.type_info.type_interner);
        let (_, err_ty) = match ok_err {
            Some(tup) => tup,
            None => {
                if self.tc.type_info.type_interner.is_error(inner.ty) {
                    // Tipo Error = diagnóstico já emitido antes (type checker já rejeitou).
                    // NÃO é bug interno — bail silenciosamente com um valor poison,
                    // sem duplicar erro nem gerar ICE.
                    let dest = target.unwrap_or_else(|| self.new_temp(ArType::Error));
                    return Ok(AmirOperand::Copy(dest));
                }
                return Err(Diagnostic::ice(
                    DiagCode::ICEGEN001,
                    "try_result: tipo não-Error mas result_ok_err falhou",
                    inner.span,
                ));
            }
        };
        let base = self.lower_expr(inner_id, None, symbols)?;
        if self.builder.current_block.is_none() {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            return Ok(AmirOperand::Copy(dest));
        }

        let err_tmp = self.new_temp_ref(&err_ty);
        self.lower_result_err_field(base, err_tmp);

        let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: base });

        let one_lit = self.intern_literal_int("1");
        let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            cond_tmp,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tag_tmp),
                right: AmirOperand::Constant(one_lit),
            },
        );

        let bb_return_err = self.new_block();
        let bb_continue = self.new_block();

        self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_return_err, bb_continue);
        self.seal_block(bb_return_err);
        self.seal_block(bb_continue);

        self.builder.current_block = Some(bb_return_err);
        self.exit_all_defer_frames(true, symbols)?;
        // Clone once: new_temp_ref needs &mut self + &ArType simultaneously.
        let err_ctor_tmp = self.new_temp_id(self.func_return_type);
        self.emit_assign_temp(
            err_ctor_tmp,
            AmirRvalue::EnumConstruct {
                variant_tag: 1,
                payload: Some(AmirOperand::Copy(err_tmp)),
            },
        );
        self.emit_assign_temp(TempId(0), AmirRvalue::Use(AmirOperand::Copy(err_ctor_tmp)));
        self.set_terminator(AmirTerminator::Return);

        self.builder.current_block = None;

        self.builder.current_block = Some(bb_continue);
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.lower_result_ok_field(base, dest);
        Ok(AmirOperand::Copy(dest))
    }

    /// Lower `expr catch handler` — like `?` but recover with handler instead of return.
    pub(crate) fn lower_catch(
        &mut self,
        inner_id: HirExprId,
        handler: &crate::hir::HirCatchHandler,
        target: Option<TempId>,
        expr_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        use crate::hir::HirCatchHandler;

        let inner = self.hir.pool.expr(inner_id);
        let ok_err = result_ok_err_id(inner.ty, &self.tc.type_info.type_interner);
        let (ok_ty, err_ty) = match ok_err {
            Some(tup) => tup,
            None => {
                if self.tc.type_info.type_interner.is_error(inner.ty) {
                    let dest = target.unwrap_or_else(|| self.new_temp(ArType::Error));
                    return Ok(AmirOperand::Copy(dest));
                }
                return Err(Diagnostic::ice(
                    DiagCode::ICEGEN001,
                    "catch: expected Result type",
                    inner.span,
                ));
            }
        };
        let _ = ok_ty;

        let base = self.lower_expr(inner_id, None, symbols)?;
        if self.builder.current_block.is_none() {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            return Ok(AmirOperand::Copy(dest));
        }

        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));

        let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: base });

        let one_lit = self.intern_literal_int("1");
        let is_err = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            is_err,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tag_tmp),
                right: AmirOperand::Constant(one_lit),
            },
        );

        let bb_err = match handler {
            HirCatchHandler::Block { block, .. } => {
                self.new_block_at(self.hir.pool.block(*block).span)
            }
            HirCatchHandler::Expr(_) => self.new_block(),
        };
        let bb_ok = self.new_block();
        let bb_join = self.new_block();

        self.set_bool_branch(AmirOperand::Copy(is_err), bb_err, bb_ok);
        self.seal_block(bb_err);
        self.seal_block(bb_ok);

        // Err arm: evaluate handler (optionally bind error payload).
        self.builder.current_block = Some(bb_err);
        if let HirCatchHandler::Block {
            error_symbol: Some(err_sym),
            ..
        } = handler
        {
            let err_tmp = self.new_temp_ref(&err_ty);
            self.lower_result_err_field(base, err_tmp);
            let err_local = self.new_local_ref(&err_ty, *err_sym, inner.span);
            let consumed = self.consume_operand(AmirOperand::Copy(err_tmp))?;
            self.write_variable_source(err_local, consumed)?;
        }
        match handler {
            HirCatchHandler::Expr(h_expr) => {
                self.lower_expr(*h_expr, Some(dest), symbols)?;
            }
            HirCatchHandler::Block { block, .. } => {
                self.lower_block_as_expr(*block, Some(dest), symbols)?;
            }
        }
        if self.builder.current_block.is_some() {
            self.emit_goto(bb_join);
        }

        // Ok arm: unwrap payload.
        self.builder.current_block = Some(bb_ok);
        self.lower_result_ok_field(base, dest);
        self.emit_goto(bb_join);

        self.seal_block(bb_join);
        self.builder.current_block = Some(bb_join);
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_try_nullable(
        &mut self,
        inner_id: HirExprId,
        target: Option<TempId>,
        expr_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let base = self.lower_expr(inner_id, None, symbols)?;
        if self.builder.current_block.is_none() {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            return Ok(AmirOperand::Copy(dest));
        }

        let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            cond_tmp,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: base,
                right: AmirOperand::Constant(AmirConstant::Nil),
            },
        );

        let bb_return_nil = self.new_block();
        let bb_continue = self.new_block();

        self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_return_nil, bb_continue);
        self.seal_block(bb_return_nil);
        self.seal_block(bb_continue);

        self.builder.current_block = Some(bb_return_nil);
        self.exit_all_defer_frames(true, symbols)?;
        self.emit_assign_temp(
            TempId(0),
            AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
        );
        self.set_terminator(AmirTerminator::Return);
        self.builder.current_block = None;

        self.builder.current_block = Some(bb_continue);
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.emit_assign_temp(dest, AmirRvalue::Use(base));
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_try_option(
        &mut self,
        inner_id: HirExprId,
        target: Option<TempId>,
        expr_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let base = self.lower_expr(inner_id, None, symbols)?;
        if self.builder.current_block.is_none() {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            return Ok(AmirOperand::Copy(dest));
        }

        let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: base });

        let zero_lit = self.intern_literal_int(arandu_middle::amir::OPTION_NONE_TAG.to_string());
        let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            cond_tmp,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tag_tmp),
                right: AmirOperand::Constant(zero_lit),
            },
        );

        let bb_return_none = self.new_block();
        let bb_continue = self.new_block();

        self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_return_none, bb_continue);
        self.seal_block(bb_return_none);
        self.seal_block(bb_continue);

        // None branch (tag == 0): return Option.None
        self.builder.current_block = Some(bb_return_none);
        self.exit_all_defer_frames(true, symbols)?;
        let none_tmp = self.new_temp_id(self.func_return_type);
        self.emit_assign_temp(
            none_tmp,
            AmirRvalue::EnumConstruct {
                variant_tag: arandu_middle::amir::OPTION_NONE_TAG,
                payload: None,
            },
        );
        self.emit_assign_temp(TempId(0), AmirRvalue::Use(AmirOperand::Copy(none_tmp)));
        self.set_terminator(AmirTerminator::Return);
        self.builder.current_block = None;

        // Some branch (tag == 1): extract payload
        self.builder.current_block = Some(bb_continue);
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.lower_result_ok_field(base, dest);
        Ok(AmirOperand::Copy(dest))
    }
}
