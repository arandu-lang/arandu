//! Control flow lowering: if-expressions, try, safe-field/index, and null-coalesce.

use super::super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirBlockId, HirCondition, HirExpr, HirExprId};
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive, is_option_type, result_ok_err_id};

impl LowerCtx<'_> {
    pub(crate) fn coerce_to_option(
        &mut self,
        value: AmirOperand,
        value_ty: crate::types::TypeId,
        expected_ty: crate::types::TypeId,
    ) -> AmirOperand {
        if !self.with_ty(expected_ty, is_option_type)
            || self.with_ty(value_ty, is_option_type)
            || matches!(value, AmirOperand::Constant(AmirConstant::Nil))
        {
            return value;
        }
        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = value
            && self.with_ty(self.temps[t.as_usize()].ty, is_option_type)
        {
            return value;
        }
        let wrapped = self.new_temp_id(expected_ty);
        self.emit_assign_temp(
            wrapped,
            AmirRvalue::EnumConstruct {
                variant_tag: arandu_middle::amir::OPTION_SOME_TAG,
                payload: Some(value),
            },
        );
        AmirOperand::Copy(wrapped)
    }

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

        self.lower_condition_branch(condition, bb_then, bb_else, symbols)?;
        self.seal_block(bb_then);
        self.seal_block(bb_else);

        if matches!(self.resolve_ty(expr.ty), ArType::Void) {
            self.builder.current_block = Some(bb_then);
            self.lower_block(then_block, symbols)?;
            if self.builder.current_block.is_some() {
                self.emit_goto(bb_join);
            }

            self.builder.current_block = Some(bb_else);
            self.lower_block(else_block, symbols)?;
            if self.builder.current_block.is_some() {
                self.emit_goto(bb_join);
            }

            self.finish_join(bb_join);
            return if let Some(dest) = target {
                if self.builder.current_block.is_some() {
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
                    );
                }
                Ok(AmirOperand::Copy(dest))
            } else {
                Ok(AmirOperand::Constant(AmirConstant::Nil))
            };
        }

        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));

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

        if base_is_option {
            let bb_null = self.new_block();
            let bb_access = self.new_block();
            let bb_join = self.new_block();

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
            let mut payload_resolved = self.resolve_ty(payload_ty);
            let mut field_base_op = AmirOperand::Copy(payload_tmp);
            for _ in 0..4 {
                match payload_resolved {
                    ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                        let deref_tmp = self.new_temp_id(inner);
                        self.emit_assign_temp(
                            deref_tmp,
                            AmirRvalue::Unary {
                                op: crate::ops::UnaryOp::Deref,
                                operand: field_base_op,
                            },
                        );
                        field_base_op = AmirOperand::Copy(deref_tmp);
                        payload_resolved = self.resolve_ty(inner);
                    }
                    _ => break,
                }
            }
            let field_idx = self.resolve_field_index(&payload_resolved, field);
            let field_ty = arandu_middle::layout::instantiated_field_type(
                &payload_resolved,
                field,
                &self.tc.type_info.type_interner,
                &*self.tc.type_info,
            )
            .unwrap_or(expr.ty);
            let field_val_tmp = self.new_temp_id(field_ty);
            self.emit_assign_temp(
                field_val_tmp,
                AmirRvalue::FieldAccess {
                    base: field_base_op,
                    field: field_idx,
                },
            );
            if self.with_ty(field_ty, is_option_type) {
                self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Copy(field_val_tmp)));
            } else {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: arandu_middle::amir::OPTION_SOME_TAG,
                        payload: Some(AmirOperand::Copy(field_val_tmp)),
                    },
                );
            }
            self.emit_goto(bb_join);

            self.seal_block(bb_join);
            self.builder.current_block = Some(bb_join);
            return Ok(AmirOperand::Copy(dest));
        }

        // Non-optional base with `?.`: access field directly and wrap as `Option<U>`.
        let mut base_resolved = self.resolve_ty(base_expr.ty);
        let mut field_base_op = base_op;
        for _ in 0..4 {
            match base_resolved {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    let deref_tmp = self.new_temp_id(inner);
                    self.emit_assign_temp(
                        deref_tmp,
                        AmirRvalue::Unary {
                            op: crate::ops::UnaryOp::Deref,
                            operand: field_base_op,
                        },
                    );
                    field_base_op = AmirOperand::Copy(deref_tmp);
                    base_resolved = self.resolve_ty(inner);
                }
                _ => break,
            }
        }
        let field_idx = self.resolve_field_index(&base_resolved, field);
        let field_ty = arandu_middle::layout::instantiated_field_type(
            &base_resolved,
            field,
            &self.tc.type_info.type_interner,
            &*self.tc.type_info,
        )
        .unwrap_or(expr.ty);
        let field_tmp = self.new_temp_id(field_ty);
        self.emit_assign_temp(
            field_tmp,
            AmirRvalue::FieldAccess {
                base: field_base_op,
                field: field_idx,
            },
        );
        if self.with_ty(field_ty, is_option_type) {
            self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Copy(field_tmp)));
        } else {
            self.emit_assign_temp(
                dest,
                AmirRvalue::EnumConstruct {
                    variant_tag: arandu_middle::amir::OPTION_SOME_TAG,
                    payload: Some(AmirOperand::Copy(field_tmp)),
                },
            );
        }
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
        let base_expr = self.hir.pool.expr(base);
        let base_is_option = self.with_ty(base_expr.ty, is_option_type);
        let base_op = self.lower_expr(base, None, symbols)?;
        if self.builder.current_block.is_none() {
            return Ok(AmirOperand::Copy(dest));
        }

        if base_is_option {
            let bb_null = self.new_block();
            let bb_access = self.new_block();
            let bb_join = self.new_block();

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
            let index_ty = self.hir.pool.expr(index).ty;
            let index_op = self.lower_expr(index, None, symbols)?;
            let payload_resolved = self.resolve_ty(payload_ty);
            let elem_ty = arandu_middle::types::index_elem_type(
                &payload_resolved,
                symbols,
                &self.tc.type_info.type_interner,
            )
            .unwrap_or(expr.ty);
            let elem_tmp = self.new_temp_id(elem_ty);
            if arandu_middle::types::is_vec_type(&payload_resolved, symbols) {
                let local = self.new_compiler_local_id(payload_ty, expr.span);
                self.mark_local_materialized(local);
                self.write_variable_source(local, AmirOperand::Copy(payload_tmp))?;
                let mut place = crate::amir::AmirPlace {
                    local,
                    projections: smallvec::SmallVec::new(),
                };
                self.check_vec_index(&place, &payload_resolved, index_op, index_ty, symbols)?;
                place
                    .projections
                    .push(crate::amir::AmirProjection::Index(index_op));
                self.emit_assign_temp(elem_tmp, AmirRvalue::Load(place));
            } else {
                self.emit_assign_temp(
                    elem_tmp,
                    AmirRvalue::IndexAccess {
                        base: AmirOperand::Copy(payload_tmp),
                        index: index_op,
                        in_bounds: false,
                    },
                );
            }
            if self.builder.current_block.is_some() {
                if self.with_ty(elem_ty, is_option_type) {
                    self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Copy(elem_tmp)));
                } else {
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: arandu_middle::amir::OPTION_SOME_TAG,
                            payload: Some(AmirOperand::Copy(elem_tmp)),
                        },
                    );
                }
                self.emit_goto(bb_join);
            }

            self.seal_block(bb_join);
            self.builder.current_block = Some(bb_join);
            return Ok(AmirOperand::Copy(dest));
        }

        // Non-optional container with `?[i]`: index directly and wrap as `Option<E>`.
        let index_ty = self.hir.pool.expr(index).ty;
        let index_op = self.lower_expr(index, None, symbols)?;
        let base_resolved = self.resolve_ty(base_expr.ty);
        let elem_ty = arandu_middle::types::index_elem_type(
            &base_resolved,
            symbols,
            &self.tc.type_info.type_interner,
        )
        .unwrap_or(expr.ty);
        let elem_tmp = self.new_temp_id(elem_ty);
        if arandu_middle::types::is_vec_type(&base_resolved, symbols) {
            let local = self.new_compiler_local_id(base_expr.ty, expr.span);
            self.mark_local_materialized(local);
            self.write_variable_source(local, base_op)?;
            let mut place = crate::amir::AmirPlace {
                local,
                projections: smallvec::SmallVec::new(),
            };
            self.check_vec_index(&place, &base_resolved, index_op, index_ty, symbols)?;
            place
                .projections
                .push(crate::amir::AmirProjection::Index(index_op));
            self.emit_assign_temp(elem_tmp, AmirRvalue::Load(place));
        } else {
            self.emit_assign_temp(
                elem_tmp,
                AmirRvalue::IndexAccess {
                    base: base_op,
                    index: index_op,
                    in_bounds: false,
                },
            );
        }
        if self.builder.current_block.is_some() {
            if self.with_ty(elem_ty, is_option_type) {
                self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Copy(elem_tmp)));
            } else {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: arandu_middle::amir::OPTION_SOME_TAG,
                        payload: Some(AmirOperand::Copy(elem_tmp)),
                    },
                );
            }
        }
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
