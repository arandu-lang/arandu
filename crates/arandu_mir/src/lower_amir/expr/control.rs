//! Control flow lowering: if-expressions, try, safe-field/index, and null-coalesce.

use super::super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirBlockId, HirCondition, HirExpr, HirExprId};
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive, is_option_type, result_ok_err_id};

impl LowerCtx<'_> {
    pub(super) fn lower_if(
        &mut self,
        condition: &HirCondition,
        then_block: HirBlockId,
        else_block: HirBlockId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let bb_then = self.new_block_at(self.hir.pool.block(then_block).span);
        let bb_else = self.new_block_at(self.hir.pool.block(else_block).span);
        let bb_join = self.new_block();

        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));

        self.lower_condition_branch(condition, bb_then, bb_else, symbols)?;
        self.seal_block(bb_then);
        self.seal_block(bb_else);

        // Then branch
        self.builder.current_block = Some(bb_then);
        self.lower_block_as_expr(then_block, Some(dest), symbols)?;
        if self.builder.current_block.is_some() {
            self.emit_goto(bb_join);
        }

        // Else branch
        self.builder.current_block = Some(bb_else);
        self.lower_block_as_expr(else_block, Some(dest), symbols)?;
        if self.builder.current_block.is_some() {
            self.emit_goto(bb_join);
        }

        // Join
        self.seal_block(bb_join);
        self.builder.current_block = Some(bb_join);
        Ok(AmirOperand::Copy(dest))
    }

    pub(super) fn lower_try(
        &mut self,
        inner: HirExprId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let inner_expr = self.hir.pool.expr(inner);
        if result_ok_err_id(inner_expr.ty, &self.tc.type_info.type_interner).is_some() {
            self.lower_try_result(inner, target, expr.ty, symbols)
        } else if self.with_ty(inner_expr.ty, is_option_type) {
            self.lower_try_option(inner, target, expr.ty, symbols)
        } else if self.with_ty(inner_expr.ty, |t| matches!(t, ArType::Nullable(_))) {
            self.lower_try_nullable(inner, target, expr.ty, symbols)
        } else {
            self.lower_try_result(inner, target, expr.ty, symbols)
        }
    }

    pub(super) fn lower_safe_field(
        &mut self,
        base: HirExprId,
        field: &str,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        let base_expr = self.hir.pool.expr(base);
        let base_is_option = self.with_ty(base_expr.ty, is_option_type);
        let base_op = self.lower_expr(base, None, symbols)?;
        if self.builder.current_block.is_none() {
            return Ok(AmirOperand::Copy(dest));
        }

        let bb_null = self.new_block();
        let bb_access = self.new_block();
        let bb_join = self.new_block();

        if base_is_option {
            // Option: tag OPTION_NONE_TAG is None, OPTION_SOME_TAG is Some
            let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
            self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: base_op });
            let zero_lit =
                self.intern_literal_int(arandu_middle::amir::OPTION_NONE_TAG.to_string());
            let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
            self.emit_assign_temp(
                cond_tmp,
                AmirRvalue::Binary {
                    op: BinaryOp::Equal,
                    left: AmirOperand::Copy(tag_tmp),
                    right: AmirOperand::Constant(zero_lit),
                },
            );
            self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_null, bb_access);
            self.seal_block(bb_null);
            self.seal_block(bb_access);

            self.builder.current_block = Some(bb_null);
            self.emit_assign_temp(
                dest,
                AmirRvalue::EnumConstruct {
                    variant_tag: arandu_middle::amir::OPTION_NONE_TAG,
                    payload: None,
                },
            );
            self.emit_goto(bb_join);

            self.builder.current_block = Some(bb_access);
            let payload_ty = match self.resolve_ty(base_expr.ty) {
                ArType::Option(inner) => inner,
                _ => base_expr.ty,
            };
            let payload_tmp = self.new_temp_id(payload_ty);
            self.lower_result_ok_field(base_op, payload_tmp);
            let payload_resolved = self.resolve_ty(payload_ty);
            let field_idx = self.resolve_field_index(&payload_resolved, field);
            let field_val_tmp = self.new_temp(ArType::Error); // will be used as payload
            self.emit_assign_temp(
                field_val_tmp,
                AmirRvalue::FieldAccess {
                    base: AmirOperand::Copy(payload_tmp),
                    field: field_idx,
                },
            );
            self.emit_assign_temp(
                dest,
                AmirRvalue::EnumConstruct {
                    variant_tag: arandu_middle::amir::OPTION_SOME_TAG,
                    payload: Some(AmirOperand::Copy(field_val_tmp)),
                },
            );
            self.emit_goto(bb_join);
        } else {
            let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
            self.emit_assign_temp(
                cond_tmp,
                AmirRvalue::Binary {
                    op: BinaryOp::Equal,
                    left: base_op,
                    right: AmirOperand::Constant(AmirConstant::Nil),
                },
            );

            self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_null, bb_access);
            self.seal_block(bb_null);
            self.seal_block(bb_access);

            self.builder.current_block = Some(bb_null);
            self.emit_assign_temp(
                dest,
                AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
            );
            self.emit_goto(bb_join);

            self.builder.current_block = Some(bb_access);
            let base_tmp = self.new_temp_id(base_expr.ty);
            self.emit_assign_temp(base_tmp, AmirRvalue::Use(base_op));
            let base_ty = self.resolve_ty(base_expr.ty);
            let field_idx = self.resolve_field_index(&base_ty, field);
            self.emit_assign_temp(
                dest,
                AmirRvalue::FieldAccess {
                    base: AmirOperand::Copy(base_tmp),
                    field: field_idx,
                },
            );
            self.emit_goto(bb_join);
        }

        self.seal_block(bb_join);
        self.builder.current_block = Some(bb_join);
        Ok(AmirOperand::Copy(dest))
    }

    pub(super) fn lower_safe_index(
        &mut self,
        base: HirExprId,
        index: HirExprId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        let base_op = self.lower_expr(base, None, symbols)?;
        if self.builder.current_block.is_none() {
            return Ok(AmirOperand::Copy(dest));
        }

        let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            cond_tmp,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: base_op,
                right: AmirOperand::Constant(AmirConstant::Nil),
            },
        );

        let bb_null = self.new_block();
        let bb_access = self.new_block();
        let bb_join = self.new_block();

        self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_null, bb_access);
        self.seal_block(bb_null);
        self.seal_block(bb_access);

        self.builder.current_block = Some(bb_null);
        self.emit_assign_temp(
            dest,
            AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
        );
        self.emit_goto(bb_join);

        self.builder.current_block = Some(bb_access);
        let index_op = self.lower_expr(index, None, symbols)?;
        self.emit_assign_temp(
            dest,
            AmirRvalue::IndexAccess {
                base: base_op,
                index: index_op,
            },
        );
        if self.builder.current_block.is_some() {
            self.emit_goto(bb_join);
        }

        self.seal_block(bb_join);
        self.builder.current_block = Some(bb_join);
        Ok(AmirOperand::Copy(dest))
    }

    pub(super) fn lower_null_coalesce(
        &mut self,
        left: HirExprId,
        right: HirExprId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        let left_expr = self.hir.pool.expr(left);
        let left_is_option = self.with_ty(left_expr.ty, is_option_type);
        let left_op = self.lower_expr(left, None, symbols)?;
        if self.builder.current_block.is_none() {
            return Ok(AmirOperand::Copy(dest));
        }

        let bb_left = self.new_block();
        let bb_right = self.new_block();
        let bb_join = self.new_block();

        if left_is_option {
            // Option: tag OPTION_SOME_TAG is Some, OPTION_NONE_TAG is None.
            let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
            self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: left_op });
            let one_lit = self.intern_literal_int(arandu_middle::amir::OPTION_SOME_TAG.to_string());
            let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
            self.emit_assign_temp(
                cond_tmp,
                AmirRvalue::Binary {
                    op: BinaryOp::Equal,
                    left: AmirOperand::Copy(tag_tmp),
                    right: AmirOperand::Constant(one_lit),
                },
            );
            self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_left, bb_right);
            self.seal_block(bb_left);
            self.seal_block(bb_right);

            self.builder.current_block = Some(bb_left);
            self.lower_result_ok_field(left_op, dest);
            self.emit_goto(bb_join);
        } else {
            let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
            self.emit_assign_temp(
                cond_tmp,
                AmirRvalue::Binary {
                    op: BinaryOp::NotEqual,
                    left: left_op,
                    right: AmirOperand::Constant(AmirConstant::Nil),
                },
            );
            self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_left, bb_right);
            self.seal_block(bb_left);
            self.seal_block(bb_right);

            self.builder.current_block = Some(bb_left);
            self.emit_assign_temp(dest, AmirRvalue::Use(left_op));
            self.emit_goto(bb_join);
        }

        self.builder.current_block = Some(bb_right);
        self.lower_expr(right, Some(dest), symbols)?;
        if self.builder.current_block.is_some() {
            self.emit_goto(bb_join);
        }

        self.seal_block(bb_join);
        self.builder.current_block = Some(bb_join);
        Ok(AmirOperand::Copy(dest))
    }

    pub(super) fn lower_async_block(
        &mut self,
        block: HirBlockId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        let payload_ty = match self.resolve_ty(expr.ty) {
            ArType::Coroutine(inner) => inner,
            _ => expr.ty,
        };
        let payload_tmp = self.new_temp_id(payload_ty);
        self.coroutine_depth = self.coroutine_depth.saturating_add(1);
        let lower_res = self.lower_block_as_expr(block, Some(payload_tmp), symbols);
        self.coroutine_depth = self.coroutine_depth.saturating_sub(1);
        lower_res?;
        // A3.3: stack state unless this is the return register of a coroutine-
        // returning function (must outlive the callee).
        let stack = dest != TempId(0);
        self.emit_assign_temp(
            dest,
            AmirRvalue::CoroutineReady {
                value: AmirOperand::Copy(payload_tmp),
                payload_ty,
                stack,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }
}
