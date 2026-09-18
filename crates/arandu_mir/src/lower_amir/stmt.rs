use super::{DeferFrame, DeferKind, LowerCtx};
use crate::SymbolTable;
use crate::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator,
    TempId,
};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirForClause, HirPlace, HirPlaceSuffix, HirSimpleStmt, HirStmt, HirStmtKind};
use crate::ops::{BinaryOp, SetOp};
use crate::passes::type_checker::types::{ArType, Primitive, result_ok_err_id};

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

    fn lower_stmt_inner(
        &mut self,
        stmt: &HirStmt,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        match &stmt.kind {
            HirStmtKind::VarDecl { bindings, value } => {
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
                    // Go-style `let ok, err = f()` for Result<T, E>:
                    // disc 0 → ok = payload, err = nil; disc 1 → ok zeroed, err = payload.
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
            HirStmtKind::Set { places, op, value } => {
                let val_op = self.lower_expr(*value, None, symbols)?;
                self.lower_set_places(self.hir.pool.places_list(*places), op, &val_op, symbols)?;
            }
            HirStmtKind::Return { values } => {
                let values_slice = self.hir.pool.expr_list(*values);
                if values_slice.len() == 1 {
                    // A3: async body returns bare `T`; wrap as `CoroutineReady` into `_0`.
                    if self.func_is_async {
                        if let ArType::Coroutine(payload_ty) =
                            self.resolve_ty(self.func_return_type)
                        {
                            let inner = self.lower_expr(values_slice[0], None, symbols)?;
                            self.emit_assign_temp(
                                TempId(0),
                                AmirRvalue::CoroutineReady {
                                    value: inner,
                                    payload_ty,
                                    // A3.3: returned coroutine must outlive this frame → heap.
                                    stack: false,
                                },
                            );
                        } else {
                            self.lower_expr(values_slice[0], Some(TempId(0)), symbols)?;
                        }
                    } else {
                        self.lower_expr(values_slice[0], Some(TempId(0)), symbols)?;
                    }
                } else if values_slice.len() > 1 {
                    let mut ops = Vec::new();
                    for &v in values_slice {
                        ops.push(self.lower_expr(v, None, symbols)?);
                    }
                    if self.func_is_async {
                        if let ArType::Coroutine(payload_ty) =
                            self.resolve_ty(self.func_return_type)
                        {
                            let tup = self.new_temp_id(payload_ty);
                            self.emit_assign_temp(tup, AmirRvalue::Tuple { items: ops });
                            self.emit_assign_temp(
                                TempId(0),
                                AmirRvalue::CoroutineReady {
                                    value: AmirOperand::Copy(tup),
                                    payload_ty,
                                    stack: false,
                                },
                            );
                        } else {
                            self.emit_assign_temp(TempId(0), AmirRvalue::Tuple { items: ops });
                        }
                    } else {
                        self.emit_assign_temp(TempId(0), AmirRvalue::Tuple { items: ops });
                    }
                } else if self.func_is_async {
                    // `return` with no value in async void → ready unit coroutine.
                    if let ArType::Coroutine(payload_ty) = self.resolve_ty(self.func_return_type) {
                        self.emit_assign_temp(
                            TempId(0),
                            AmirRvalue::CoroutineReady {
                                value: AmirOperand::Constant(AmirConstant::Nil),
                                payload_ty,
                                stack: false,
                            },
                        );
                    }
                }

                let is_result =
                    result_ok_err_id(self.func_return_type, &self.tc.type_info.type_interner)
                        .is_some();
                let has_errdefer = self.defer_frames.iter().any(|frame| {
                    frame
                        .entries
                        .iter()
                        .any(|(_, kind)| *kind == DeferKind::ErrDefer)
                });

                if is_result && has_errdefer {
                    let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
                    self.emit_assign_temp(
                        tag_tmp,
                        AmirRvalue::Discriminant {
                            value: AmirOperand::Copy(TempId(0)),
                        },
                    );

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

                    let bb_err = self.new_block();
                    let bb_ok = self.new_block();

                    self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_err, bb_ok);
                    self.seal_block(bb_err);
                    self.seal_block(bb_ok);

                    // BB Err: runs errdefers AND defers
                    self.builder.current_block = Some(bb_err);
                    let saved_frames = self.defer_frames.clone();
                    self.exit_all_defer_frames(true, symbols)?;
                    self.set_terminator(AmirTerminator::Return);

                    // BB Ok: runs ONLY defers
                    self.defer_frames = saved_frames;
                    self.builder.current_block = Some(bb_ok);
                    self.exit_all_defer_frames(false, symbols)?;
                    self.set_terminator(AmirTerminator::Return);
                } else {
                    let is_error = self.is_error_return(values_slice);
                    self.exit_all_defer_frames(is_error, symbols)?;
                    self.set_terminator(AmirTerminator::Return);
                }

                self.builder.current_block = None;
            }

            HirStmtKind::Break => {
                if let Some((_, exit_block, defer_depth)) = self.loop_stack.last().copied() {
                    self.exit_defer_frames_from(defer_depth, false, symbols)?;
                    self.emit_goto(exit_block);
                    self.builder.current_block = None;
                }
            }
            HirStmtKind::Continue => {
                if let Some((cont_block, _, defer_depth)) = self.loop_stack.last().copied() {
                    self.exit_defer_frames_from(defer_depth, false, symbols)?;
                    self.emit_goto(cont_block);
                    self.builder.current_block = None;
                }
            }
            HirStmtKind::Expr(expr) => {
                self.lower_expr_stmt(*expr, symbols)?;
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let cond_op = self.lower_condition(condition, symbols)?;
                let bb_then = self.new_block();
                let bb_else = self.new_block();
                let bb_join = self.new_block();

                self.set_bool_branch(cond_op, bb_then, bb_else);
                self.seal_block(bb_then);
                self.seal_block(bb_else);

                // Then
                self.builder.current_block = Some(bb_then);
                self.lower_block(*then_block, symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_join);
                }

                // Else
                self.builder.current_block = Some(bb_else);
                if let Some(eb) = else_block {
                    self.lower_block(*eb, symbols)?;
                }
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_join);
                }

                self.finish_join(bb_join);
            }
            HirStmtKind::While { condition, body } => {
                let bb_cond = self.new_block();
                let bb_body = self.new_block();
                let bb_exit = self.new_block();

                self.emit_goto(bb_cond);

                self.builder.current_block = Some(bb_cond);
                let cond_op = self.lower_condition(condition, symbols)?;
                self.set_bool_branch(cond_op, bb_body, bb_exit);
                self.seal_block(bb_body);
                self.seal_block(bb_exit);

                let defer_depth = self.defer_frames.len();
                self.loop_stack.push((bb_cond, bb_exit, defer_depth));
                self.builder.current_block = Some(bb_body);
                self.lower_block(*body, symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_cond);
                }
                self.loop_stack.pop();
                self.seal_block(bb_cond);

                self.builder.current_block = Some(bb_exit);
            }
            HirStmtKind::For { clause, body } => match clause {
                HirForClause::In {
                    span: _,
                    bindings,
                    iterable,
                } => {
                    let iter_op = self.lower_expr(*iterable, None, symbols)?;

                    let idx_local = self.new_compiler_local(ArType::Primitive(Primitive::Int));
                    let zero_lit = self.intern_literal_int("0");
                    self.emit_store_place(
                        AmirPlace {
                            local: idx_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        AmirOperand::Constant(zero_lit),
                    )?;

                    let len_local = self.new_compiler_local(ArType::Primitive(Primitive::Int));
                    let len_temp = self.new_temp(ArType::Primitive(Primitive::Int));
                    self.emit_assign_temp(len_temp, AmirRvalue::Len(iter_op));
                    self.emit_store_place(
                        AmirPlace {
                            local: len_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        AmirOperand::Copy(len_temp),
                    )?;

                    let bb_cond = self.new_block();
                    let bb_body = self.new_block();
                    let bb_step = self.new_block();
                    let bb_exit = self.new_block();

                    self.emit_goto(bb_cond);

                    self.builder.current_block = Some(bb_cond);
                    let int_ty =
                        arandu_middle::types::TypeInterner::preinterned_primitive(Primitive::Int);
                    let idx_op = self.load_place(
                        &AmirPlace {
                            local: idx_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        int_ty,
                    )?;
                    let len_op = self.load_place(
                        &AmirPlace {
                            local: len_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        int_ty,
                    )?;
                    let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                    self.emit_assign_temp(
                        cond_tmp,
                        AmirRvalue::Binary {
                            op: BinaryOp::Lt,
                            left: idx_op,
                            right: len_op,
                        },
                    );
                    self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_body, bb_exit);
                    self.seal_block(bb_body);
                    self.seal_block(bb_exit);

                    let defer_depth = self.defer_frames.len();
                    self.loop_stack.push((bb_step, bb_exit, defer_depth));
                    self.builder.current_block = Some(bb_body);

                    let bindings_slice = self.hir.pool.for_bindings_list(*bindings);
                    if let Some(binding) = bindings_slice.first() {
                        let local_id = self
                            .symbol_map
                            .get(&binding.symbol)
                            .copied()
                            .unwrap_or_else(|| {
                                self.new_local_id(binding.ty, binding.symbol, binding.span)
                            });
                        let idx_op2 = self.load_place(
                            &AmirPlace {
                                local: idx_local,
                                projections: smallvec::SmallVec::new(),
                            },
                            arandu_middle::types::TypeInterner::preinterned_primitive(
                                Primitive::Int,
                            ),
                        )?;
                        let elem_temp = self.new_temp_id(binding.ty);
                        self.emit_assign_temp(
                            elem_temp,
                            AmirRvalue::IndexAccess {
                                base: iter_op,
                                index: idx_op2,
                            },
                        );
                        let consumed = self.consume_operand(AmirOperand::Copy(elem_temp))?;
                        self.write_variable_source(local_id, consumed)?;
                    }

                    self.lower_block(*body, symbols)?;
                    if self.builder.current_block.is_some() {
                        self.emit_goto(bb_step);
                    }
                    self.loop_stack.pop();

                    self.builder.current_block = Some(bb_step);
                    self.seal_block(bb_step);
                    let idx_op3 = self.load_place(
                        &AmirPlace {
                            local: idx_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        arandu_middle::types::TypeInterner::preinterned_primitive(Primitive::Int),
                    )?;
                    let one_lit = self.intern_literal_int("1");
                    let next_idx = self.new_temp(ArType::Primitive(Primitive::Int));
                    self.emit_assign_temp(
                        next_idx,
                        AmirRvalue::Binary {
                            op: BinaryOp::Add,
                            left: idx_op3,
                            right: AmirOperand::Constant(one_lit),
                        },
                    );
                    self.emit_store_place(
                        AmirPlace {
                            local: idx_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        AmirOperand::Copy(next_idx),
                    )?;
                    self.emit_goto(bb_cond);
                    self.seal_block(bb_cond);

                    self.builder.current_block = Some(bb_exit);
                }
                HirForClause::CStyle {
                    init,
                    condition,
                    step,
                    ..
                } => {
                    if let Some(i) = init {
                        self.lower_simple_stmt(i, symbols)?;
                    }

                    let bb_cond = self.new_block();
                    let bb_body = self.new_block();
                    let bb_step = self.new_block();
                    let bb_exit = self.new_block();

                    self.emit_goto(bb_cond);

                    self.builder.current_block = Some(bb_cond);
                    let cond_op = if let Some(c) = condition {
                        self.lower_expr(*c, None, symbols)?
                    } else {
                        AmirOperand::Constant(AmirConstant::Bool(true))
                    };
                    self.set_bool_branch(cond_op, bb_body, bb_exit);
                    self.seal_block(bb_body);
                    self.seal_block(bb_exit);

                    let defer_depth = self.defer_frames.len();
                    self.loop_stack.push((bb_step, bb_exit, defer_depth));
                    self.builder.current_block = Some(bb_body);
                    self.lower_block(*body, symbols)?;
                    if self.builder.current_block.is_some() {
                        self.emit_goto(bb_step);
                    }
                    self.loop_stack.pop();

                    self.builder.current_block = Some(bb_step);
                    self.seal_block(bb_step);
                    if let Some(s) = step {
                        self.lower_simple_stmt(s, symbols)?;
                    }
                    if self.builder.current_block.is_some() {
                        self.emit_goto(bb_cond);
                    }
                    self.seal_block(bb_cond);

                    self.builder.current_block = Some(bb_exit);
                }
            },
            HirStmtKind::Match { value, arms } => {
                let bb_end = self.new_block();
                self.lower_match_stmt(*value, arms, bb_end, symbols)?;
            }
            HirStmtKind::Unsafe(b) => {
                self.lower_block(*b, symbols)?;
            }
            HirStmtKind::Defer(block) => {
                self.register_defer(self.hir.pool.block(*block), DeferKind::Defer);
            }
            HirStmtKind::ErrDefer(block) => {
                self.register_defer(self.hir.pool.block(*block), DeferKind::ErrDefer);
            }
            HirStmtKind::Free(expr) => {
                let op = self.lower_expr(*expr, None, symbols)?;
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                    self.note_temp_origin_use(t);
                }
                self.push_stmt(AmirStmt::Free(op));
            }
            HirStmtKind::Error => {}
        }
        Ok(())
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
                    self.lower_expr(*expr, None, symbols)?,
                )),
            })
            .collect();
        let amir_place = AmirPlace {
            local: local_id,
            projections: projections?.into(),
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
        }
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
            }
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
        }
        Ok(())
    }
}
