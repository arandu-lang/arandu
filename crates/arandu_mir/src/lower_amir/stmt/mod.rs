use super::{DeferFrame, LowerCtx};
use crate::SymbolTable;
use crate::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, TempId,
};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirPlace, HirPlaceSuffix, HirSimpleStmt, HirStmt, HirStmtKind};
use crate::ops::{BinaryOp, SetOp};
use crate::passes::type_checker::types::{ArType, Primitive, result_ok_err_id};

mod inner;

impl LowerCtx<'_> {
    /// Lower Go-style `let ok, err = result` for `Result<T, E>`.
    ///
    /// On Ok (disc 0): `ok` ← payload, `err` ← nil.
    /// On Err (disc 1): `ok` left zero-init / unused, `err` ← payload.
    fn lower_result_multi_bind(
        &mut self,
        result_op: AmirOperand,
        ok_b: &crate::hir::HirBindingItem,
        err_b: &crate::hir::HirBindingItem,
        _symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let ok_local = self.new_local_id(ok_b.ty, ok_b.symbol, ok_b.span);
        let err_local = self.new_local_id(err_b.ty, err_b.symbol, err_b.span);

        let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: result_op });

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

        let bb_err = self.new_block();
        let bb_ok = self.new_block();
        let bb_join = self.new_block();

        self.set_bool_branch(AmirOperand::Copy(is_err), bb_err, bb_ok);
        self.seal_block(bb_err);
        self.seal_block(bb_ok);

        // Err branch: err = payload; ok = zero/nil so both locals are defined on all paths.
        self.builder.current_block = Some(bb_err);
        let err_tmp = self.new_temp_id(err_b.ty);
        self.lower_result_err_field(result_op, err_tmp);
        let err_consumed = self.consume_operand(AmirOperand::Copy(err_tmp))?;
        self.write_variable_source(err_local, err_consumed)?;
        let ok_zero = self.new_temp_id(ok_b.ty);
        self.emit_assign_temp(
            ok_zero,
            AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
        );
        let ok_zero_c = self.consume_operand(AmirOperand::Copy(ok_zero))?;
        self.write_variable_source(ok_local, ok_zero_c)?;
        self.emit_goto(bb_join);

        // Ok branch: ok = payload, err = nil.
        self.builder.current_block = Some(bb_ok);
        let ok_tmp = self.new_temp_id(ok_b.ty);
        self.lower_result_ok_field(result_op, ok_tmp);
        let ok_consumed = self.consume_operand(AmirOperand::Copy(ok_tmp))?;
        self.write_variable_source(ok_local, ok_consumed)?;
        let nil_tmp = self.new_temp_id(err_b.ty);
        self.emit_assign_temp(
            nil_tmp,
            AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
        );
        let nil_consumed = self.consume_operand(AmirOperand::Copy(nil_tmp))?;
        self.write_variable_source(err_local, nil_consumed)?;
        self.emit_goto(bb_join);

        self.seal_block(bb_join);
        self.builder.current_block = Some(bb_join);
        Ok(())
    }

    pub(crate) fn lower_stmt(
        &mut self,
        stmt: &HirStmt,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if self.builder.current_block.is_none() {
            return Ok(());
        }

        self.with_span(stmt.span, |this| this.lower_stmt_inner(stmt, symbols))
    }

    pub(crate) fn lower_simple_stmt(
        &mut self,
        stmt: &HirSimpleStmt,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if self.builder.current_block.is_none() {
            return Ok(());
        }
        match stmt {
            HirSimpleStmt::VarDecl { bindings, value } => {
                let bindings_slice = self.hir.pool.bindings_list(*bindings);
                if bindings_slice.len() == 1 {
                    let b = &bindings_slice[0];
                    let local_id = self.new_local_id(b.ty, b.symbol, b.span);
                    let val_op = self.lower_expr(*value, None, symbols)?;
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = &val_op {
                        self.owned_string_temps.remove(t);
                    }
                    let consumed = self.consume_operand(val_op)?;
                    self.write_variable_source(local_id, consumed)?;
                } else if bindings_slice.len() == 2 {
                    let val_expr = self.hir.pool.expr(*value);
                    let val_op = self.lower_expr(*value, None, symbols)?;
                    if result_ok_err_id(val_expr.ty, &self.tc.type_info.type_interner).is_some() {
                        self.lower_result_multi_bind(
                            val_op,
                            &bindings_slice[0],
                            &bindings_slice[1],
                            symbols,
                        )?;
                    } else {
                        for (i, b) in bindings_slice.iter().enumerate() {
                            let local_id = self.new_local_id(b.ty, b.symbol, b.span);
                            let temp = self.new_temp_id(b.ty);
                            self.emit_assign_temp(
                                temp,
                                AmirRvalue::FieldAccess {
                                    base: val_op,
                                    field: i,
                                },
                            );
                            let consumed = self.consume_operand(AmirOperand::Copy(temp))?;
                            self.write_variable_source(local_id, consumed)?;
                        }
                    }
                } else {
                    let val_op = self.lower_expr(*value, None, symbols)?;
                    for (i, b) in bindings_slice.iter().enumerate() {
                        let local_id = self.new_local_id(b.ty, b.symbol, b.span);
                        let temp = self.new_temp_id(b.ty);
                        self.emit_assign_temp(
                            temp,
                            AmirRvalue::FieldAccess {
                                base: val_op,
                                field: i,
                            },
                        );
                        let consumed = self.consume_operand(AmirOperand::Copy(temp))?;
                        self.write_variable_source(local_id, consumed)?;
                    }
                }
            }
            HirSimpleStmt::Set { places, op, value } => {
                let val_op = self.lower_expr(*value, None, symbols)?;
                self.lower_set_places(self.hir.pool.places_list(*places), op, &val_op, symbols)?;
            }
            HirSimpleStmt::Expr(expr) => {
                self.lower_expr_stmt(*expr, symbols)?;
            }
        }
        Ok(())
    }

    fn lower_expr_stmt(
        &mut self,
        expr_id: crate::hir::HirExprId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let start_owned = self.owned_string_temps.clone();
        self.lower_expr(expr_id, None, symbols)?;
        let to_free: Vec<TempId> = self
            .owned_string_temps
            .iter()
            .copied()
            .filter(|t| !start_owned.contains(t))
            .collect();
        for t in to_free {
            self.owned_string_temps.remove(&t);
            self.push_stmt(AmirStmt::Free(AmirOperand::Copy(t)));
        }
        Ok(())
    }

    pub(crate) fn lower_set_places(
        &mut self,
        places: &[HirPlace],
        op: &SetOp,
        val_op: &AmirOperand,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if places.len() == 1 {
            let place = &places[0];
            return self.with_span(place.span, |this| {
                this.lower_set_one_place(place, op, val_op, symbols)
            });
        }
        // Multi-place destructure: `a, b = pair` → field i of val_op into each place.
        for (i, place) in places.iter().enumerate() {
            self.with_span(place.span, |this| -> Result<(), Diagnostic> {
                let Some(&local_id) = this.symbol_map.get(&place.root_symbol) else {
                    return Ok(());
                };
                let projections: Result<Vec<_>, Diagnostic> = place
                    .suffixes
                    .iter()
                    .map(|s| match s {
                        HirPlaceSuffix::Field {
                            field_symbol: Some(symbol),
                            ..
                        } => Ok(AmirProjection::Field(*symbol)),
                        HirPlaceSuffix::Field { span, name, .. } => Err(crate::Diagnostic::error(
                            crate::DiagCode::L001LoweringUnresolvedSymbol,
                            format!("cannot lower field projection `{name}`: symbol not resolved"),
                            *span,
                        )),
                        HirPlaceSuffix::Index { expr, .. } => Ok(AmirProjection::Index(
                            this.lower_expr(*expr, None, symbols)?,
                        )),
                    })
                    .collect();
                let projections = projections?;
                if !projections.is_empty() {
                    this.mark_local_materialized(local_id);
                }
                let temp = this.new_temp_id(place.ty);
                this.emit_assign_temp(
                    temp,
                    AmirRvalue::FieldAccess {
                        base: *val_op,
                        field: i,
                    },
                );
                let val_to_store = this.consume_operand(AmirOperand::Copy(temp))?;
                if projections.is_empty() {
                    this.write_variable_source(local_id, val_to_store)?;
                } else {
                    this.emit_store_place(
                        AmirPlace {
                            local: local_id,
                            projections: projections.into(),
                        },
                        val_to_store,
                    )?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn lower_set_one_place(
        &mut self,
        place: &HirPlace,
        op: &SetOp,
        val_op: &AmirOperand,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let Some(&local_id) = self.symbol_map.get(&place.root_symbol) else {
            return Ok(());
        };
        let root_ty = self.resolve_ty(self.locals[local_id.as_usize()].ty);
        let mut projections = Vec::new();
        let starts_with_named_field =
            matches!(place.suffixes.first(), Some(HirPlaceSuffix::Field { .. }));
        if starts_with_named_field
            && matches!(root_ty, ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_))
        {
            projections.push(AmirProjection::Deref);
        }
        let suffix_projections: Result<Vec<_>, Diagnostic> = place
            .suffixes
            .iter()
            .map(|s| match s {
                HirPlaceSuffix::Field {
                    field_symbol: Some(symbol),
                    ..
                } => Ok(AmirProjection::Field(*symbol)),
                HirPlaceSuffix::Field { span, name, .. } => Err(crate::Diagnostic::error(
                    crate::DiagCode::L001LoweringUnresolvedSymbol,
                    format!("cannot lower field projection `{name}`: symbol not resolved"),
                    *span,
                )),
                HirPlaceSuffix::Index { expr, .. } => Ok(AmirProjection::Index(
                    self.lower_expr(*expr, None, symbols)?,
                )),
            })
            .collect();
        projections.extend(suffix_projections?);
        let amir_place = AmirPlace {
            local: local_id,
            projections: projections.into(),
        };

        // Projected stores address through the local's SSA value (e.g. `s.n = v`
        // where `s: &mut S`). That binding is a plain Store; if pruned as
        // non-`is_memory`, codegen uses an undef/null base (SIGSEGV). Mark the
        // local so prune keeps the param/init store (see `mark_local_materialized`).
        if !amir_place.projections.is_empty() {
            self.mark_local_materialized(local_id);
        }

        if amir_place.projections.is_empty() {
            let final_val = if *op == SetOp::Assign {
                *val_op
            } else {
                let bin_op = match op {
                    SetOp::AddAssign => BinaryOp::Add,
                    SetOp::SubAssign => BinaryOp::Sub,
                    SetOp::MulAssign => BinaryOp::Mul,
                    SetOp::DivAssign => BinaryOp::Div,
                    SetOp::ModAssign => BinaryOp::Mod,
                    SetOp::BitAndAssign => BinaryOp::BitAnd,
                    SetOp::BitOrAssign => BinaryOp::BitOr,
                    SetOp::BitXorAssign => BinaryOp::BitXor,
                    SetOp::ShiftLeftAssign => BinaryOp::ShiftLeft,
                    SetOp::ShiftRightAssign => BinaryOp::ShiftRight,
                    _ => BinaryOp::Add,
                };
                let old_val = self.read_variable_source(local_id)?;
                let temp = self.new_temp_id(place.ty);
                self.emit_assign_temp(
                    temp,
                    AmirRvalue::Binary {
                        op: bin_op,
                        left: old_val,
                        right: *val_op,
                    },
                );
                AmirOperand::Copy(temp)
            };
            let consumed = self.consume_operand(final_val)?;
            self.write_variable_source(local_id, consumed)?;
        } else if *op == SetOp::Assign {
            self.emit_store_place(amir_place, *val_op)?;
        } else {
            let bin_op = match op {
                SetOp::AddAssign => BinaryOp::Add,
                SetOp::SubAssign => BinaryOp::Sub,
                SetOp::MulAssign => BinaryOp::Mul,
                SetOp::DivAssign => BinaryOp::Div,
                SetOp::ModAssign => BinaryOp::Mod,
                SetOp::BitAndAssign => BinaryOp::BitAnd,
                SetOp::BitOrAssign => BinaryOp::BitOr,
                SetOp::BitXorAssign => BinaryOp::BitXor,
                SetOp::ShiftLeftAssign => BinaryOp::ShiftLeft,
                SetOp::ShiftRightAssign => BinaryOp::ShiftRight,
                _ => BinaryOp::Add,
            };
            let old_val = self.load_place(&amir_place, place.ty)?;
            let temp = self.new_temp_id(place.ty);
            self.emit_assign_temp(
                temp,
                AmirRvalue::Binary {
                    op: bin_op,
                    left: old_val,
                    right: *val_op,
                },
            );
            self.emit_store_place(amir_place, AmirOperand::Copy(temp))?;
        }
        Ok(())
    }

    pub(crate) fn lower_block(
        &mut self,
        block: crate::hir::HirBlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.begin_local_scope();
        self.defer_frames.push(DeferFrame {
            entries: Vec::new(),
        });
        let blk = self.hir.pool.block(block);
        for &stmt_id in self.hir.pool.stmt_list(blk.statements) {
            if self.builder.current_block.is_none() {
                break;
            }
            let stmt = self.hir.pool.stmt(stmt_id);
            self.lower_stmt(stmt, symbols)?;
        }
        if self.builder.current_block.is_some() {
            self.exit_current_defer_frame(false, symbols)?;
        } else {
            self.defer_frames.pop(); // Lexical exit; this edge emitted cleanup already.
        }
        self.end_local_scope();
        Ok(())
    }

    /// Lower a block as an expression: all statements are lowered, and if the
    /// last statement is an expression statement, its value is assigned to `target`.
    pub(crate) fn lower_block_as_expr(
        &mut self,
        block: crate::hir::HirBlockId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.lower_block_as_expr_inner(block, target, None, symbols)
    }

    /// SYN.1 + A3: async function body tail — last expr is bare payload `T`,
    /// stored as `CoroutineReady` into return temp `_0`.
    pub(crate) fn lower_block_as_expr_async_tail(
        &mut self,
        block: crate::hir::HirBlockId,
        payload_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.lower_block_as_expr_inner(block, Some(TempId(0)), Some(payload_ty), symbols)
    }

    fn lower_block_as_expr_inner(
        &mut self,
        block: crate::hir::HirBlockId,
        target: Option<TempId>,
        async_payload_ty: Option<crate::types::TypeId>,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.begin_local_scope();
        self.defer_frames.push(DeferFrame {
            entries: Vec::new(),
        });
        let blk = self.hir.pool.block(block);
        let statements_slice = self.hir.pool.stmt_list(blk.statements);
        if statements_slice.is_empty() {
            if let Some(dest) = target {
                if let Some(payload_ty) = async_payload_ty {
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::CoroutineReady {
                            value: AmirOperand::Constant(AmirConstant::Nil),
                            payload_ty,
                            stack: false,
                        },
                    );
                } else {
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
                    );
                }
            }
            if self.builder.current_block.is_some() {
                self.exit_current_defer_frame(false, symbols)?;
            } else {
                self.defer_frames.pop();
            }
            self.end_local_scope();
            return Ok(());
        }
        let last_idx = statements_slice.len() - 1;
        for (i, &stmt_id) in statements_slice.iter().enumerate() {
            if self.builder.current_block.is_none() {
                break;
            }
            let stmt = self.hir.pool.stmt(stmt_id);
            if i == last_idx {
                if let HirStmtKind::Expr(expr) = stmt.kind {
                    let start_owned = self.owned_string_temps.clone();
                    let res_op = if let (Some(dest), Some(payload_ty)) = (target, async_payload_ty)
                    {
                        let inner = self.lower_expr(expr, None, symbols)?;
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::CoroutineReady {
                                value: inner,
                                payload_ty,
                                stack: false,
                            },
                        );
                        AmirOperand::Copy(dest)
                    } else {
                        self.lower_expr(expr, target, symbols)?
                    };
                    let returned_temp = match res_op {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => Some(t),
                        _ => None,
                    };
                    let to_free: Vec<TempId> = self
                        .owned_string_temps
                        .iter()
                        .copied()
                        .filter(|t| {
                            !start_owned.contains(t)
                                && Some(*t) != returned_temp
                                && Some(*t) != target
                        })
                        .collect();
                    for t in to_free {
                        self.owned_string_temps.remove(&t);
                        self.push_stmt(AmirStmt::Free(AmirOperand::Copy(t)));
                    }
                } else {
                    self.lower_stmt(stmt, symbols)?;
                    if let Some(dest) = target
                        && self.builder.current_block.is_some()
                    {
                        if let Some(payload_ty) = async_payload_ty {
                            self.emit_assign_temp(
                                dest,
                                AmirRvalue::CoroutineReady {
                                    value: AmirOperand::Constant(AmirConstant::Nil),
                                    payload_ty,
                                    stack: false,
                                },
                            );
                        } else {
                            self.emit_assign_temp(
                                dest,
                                AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
                            );
                        }
                    }
                }
            } else {
                self.lower_stmt(stmt, symbols)?;
            }
        }
        if self.builder.current_block.is_some() {
            self.exit_current_defer_frame(false, symbols)?;
        } else {
            self.defer_frames.pop();
        }
        self.end_local_scope();
        Ok(())
    }
}
