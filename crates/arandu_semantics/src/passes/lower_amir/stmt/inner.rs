use super::super::DeferKind;
use super::{
    AmirConstant, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, ArType, BinaryOp, Diagnostic,
    HirStmt, HirStmtKind, LowerCtx, Primitive, SymbolTable, TempId, result_ok_err_id,
};
use crate::amir::AmirTerminator;
use crate::hir::{HirExprKind, HirForClause};

impl LowerCtx<'_> {
    pub(super) fn lower_stmt_inner(
        &mut self,
        stmt: &HirStmt,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.charge_static_expansion(stmt.span)?;
        match &stmt.kind {
            HirStmtKind::VarDecl { bindings, value } => {
                let bindings_slice = self.hir.pool.bindings_list(*bindings);
                if bindings_slice.len() == 1 {
                    let b = &bindings_slice[0];
                    let local_id = self.new_local_id(b.ty, b.symbol, b.span);
                    let val_expr = self.hir.pool.expr(*value);
                    let val_op = self.lower_expr(*value, None, symbols)?;
                    let val_op = self.coerce_to_option(val_op, val_expr.ty, b.ty);
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
                let val_ty = self.hir.pool.expr(*value).ty;
                let mut val_op = self.lower_expr(*value, None, symbols)?;
                let places_slice = self.hir.pool.places_list(*places);
                if places_slice.len() == 1 && *op == crate::ops::SetOp::Assign {
                    val_op = self.coerce_to_option(val_op, val_ty, places_slice[0].ty);
                }
                self.lower_set_places(places_slice, op, &val_op, symbols)?;
            }
            HirStmtKind::Return { values } => self.lower_return_stmt(values, symbols)?,

            HirStmtKind::Break { label } => {
                let target = match label {
                    Some(lbl) => self
                        .loop_stack
                        .iter()
                        .rev()
                        .find(|f| f.label.as_deref() == Some(lbl.as_str()))
                        .cloned(),
                    None => self.loop_stack.last().cloned(),
                };
                if let Some(frame) = target {
                    self.exit_defer_frames_from(frame.defer_depth, false, symbols)?;
                    self.emit_local_scope_exit_from(frame.scope_depth);
                    self.emit_goto(frame.exit_block);
                    self.builder.current_block = None;
                }
            }
            HirStmtKind::Continue { label } => {
                let target = match label {
                    Some(lbl) => self
                        .loop_stack
                        .iter()
                        .rev()
                        .find(|f| f.label.as_deref() == Some(lbl.as_str()))
                        .cloned(),
                    None => self.loop_stack.last().cloned(),
                };
                if let Some(frame) = target {
                    self.exit_defer_frames_from(frame.defer_depth, false, symbols)?;
                    self.emit_local_scope_exit_from(frame.scope_depth);
                    self.emit_goto(frame.cont_block);
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
                let bb_then = self.new_block_at(self.hir.pool.block(*then_block).span);
                let bb_else = if let Some(block) = else_block {
                    self.new_block_at(self.hir.pool.block(*block).span)
                } else {
                    self.new_block()
                };
                let bb_join = self.new_block();

                self.lower_condition_branch(condition, bb_then, bb_else, symbols)?;
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
            HirStmtKind::While {
                label,
                condition,
                body,
            } => {
                let bb_cond = self.new_block();
                let bb_body = self.new_block_at(self.hir.pool.block(*body).span);
                let bb_exit = self.new_block();

                self.emit_goto(bb_cond);

                self.builder.current_block = Some(bb_cond);
                // `while true` leaves only through `break`, like Rust's
                // `loop`: no edge to the exit, so code after a loop that
                // never breaks is unreachable.
                if matches!(condition, crate::hir::HirCondition::Expr(expr)
                    if matches!(self.hir.pool.expr(*expr).kind, HirExprKind::Bool(true)))
                {
                    self.emit_goto(bb_body);
                } else {
                    self.lower_condition_branch(condition, bb_body, bb_exit, symbols)?;
                }
                self.seal_block(bb_body);
                self.seal_block(bb_exit);

                let defer_depth = self.defer_frames.len();
                let scope_depth = self.local_scopes.len();
                self.loop_stack.push(crate::passes::lower_amir::LoopFrame {
                    label: label.clone(),
                    cont_block: bb_cond,
                    exit_block: bb_exit,
                    defer_depth,
                    scope_depth,
                });
                self.builder.current_block = Some(bb_body);
                self.lower_block(*body, symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_cond);
                }
                self.loop_stack.pop();
                self.seal_block(bb_cond);

                self.builder.current_block =
                    self.builder.has_predecessor(bb_exit).then_some(bb_exit);
            }
            HirStmtKind::For {
                comptime_bounds: Some(bounds),
                clause,
                body,
                comptime_bodies,
                ..
            } => {
                self.lower_static_for(
                    *bounds,
                    clause,
                    *body,
                    comptime_bodies.as_deref(),
                    stmt.span,
                    symbols,
                )?;
            }
            HirStmtKind::For {
                comptime_bounds: None,
                label,
                clause,
                body,
                ..
            } => self.lower_runtime_for(clause, body, label.clone(), stmt.span, symbols)?,
            HirStmtKind::Match { value, arms } => {
                let bb_end = self.new_block();
                self.lower_match_stmt(*value, arms, bb_end, symbols)?;
            }
            HirStmtKind::Unsafe(b) | HirStmtKind::Scope(b) => {
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

    fn lower_return_stmt(
        &mut self,
        values: &crate::hir::IndexRange,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if let Some((dest, exit, defer_depth, scope_depth)) = self.value_returns.last().copied() {
            let values = self.hir.pool.expr_list(*values);
            match values {
                [value] => {
                    let dest_ty = self.temps[dest.as_usize()].ty;
                    let val_ty = self.hir.pool.expr(*value).ty;
                    if self.with_ty(dest_ty, crate::passes::type_checker::types::is_option_type)
                        && !self.with_ty(val_ty, crate::passes::type_checker::types::is_option_type)
                    {
                        let op = self.lower_expr(*value, None, symbols)?;
                        let coerced = self.coerce_to_option(op, val_ty, dest_ty);
                        self.emit_assign_temp(dest, AmirRvalue::Use(coerced));
                    } else {
                        self.lower_expr(*value, Some(dest), symbols)?;
                    }
                }
                [] => self.emit_assign_temp(
                    dest,
                    AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
                ),
                values => {
                    let mut items = Vec::new();
                    for &value in values {
                        items.push(self.lower_expr(value, None, symbols)?);
                    }
                    self.emit_assign_temp(dest, AmirRvalue::Tuple { items });
                }
            }
            self.exit_defer_frames_from(defer_depth, false, symbols)?;
            self.emit_local_scope_exit_from(scope_depth);
            self.set_terminator(AmirTerminator::Goto {
                target: exit,
                args: Vec::new(),
            });
            self.builder.current_block = None;
            return Ok(());
        }
        let values_slice = self.hir.pool.expr_list(*values);
        if values_slice.len() == 1 {
            // A3: async body returns bare `T`; wrap as `CoroutineReady` into `_0`.
            if self.func_is_async {
                if let ArType::Coroutine(payload_ty) = self.resolve_ty(self.func_return_type) {
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
                let val_ty = self.hir.pool.expr(values_slice[0]).ty;
                if self.with_ty(
                    self.func_return_type,
                    crate::passes::type_checker::types::is_option_type,
                ) && !self.with_ty(val_ty, crate::passes::type_checker::types::is_option_type)
                {
                    let op = self.lower_expr(values_slice[0], None, symbols)?;
                    let coerced = self.coerce_to_option(op, val_ty, self.func_return_type);
                    self.emit_assign_temp(TempId(0), AmirRvalue::Use(coerced));
                } else {
                    self.lower_expr(values_slice[0], Some(TempId(0)), symbols)?;
                }
            }
        } else if values_slice.len() > 1 {
            let mut ops = Vec::new();
            for &v in values_slice {
                ops.push(self.lower_expr(v, None, symbols)?);
            }
            if self.func_is_async {
                if let ArType::Coroutine(payload_ty) = self.resolve_ty(self.func_return_type) {
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
            result_ok_err_id(self.func_return_type, &self.tc.type_info.type_interner).is_some();
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
            self.exit_all_defer_frames(true, symbols)?;
            self.set_terminator(AmirTerminator::Return);

            // BB Ok: runs ONLY defers
            self.builder.current_block = Some(bb_ok);
            self.exit_all_defer_frames(false, symbols)?;
            self.set_terminator(AmirTerminator::Return);
        } else {
            let is_error = self.is_error_return(values_slice);
            self.exit_all_defer_frames(is_error, symbols)?;
            self.set_terminator(AmirTerminator::Return);
        }

        self.builder.current_block = None;
        Ok(())
    }

    fn lower_runtime_for(
        &mut self,
        clause: &HirForClause,
        body: &crate::hir::HirBlockId,
        label: Option<smol_str::SmolStr>,
        span: arandu_middle::Span,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        match clause {
            HirForClause::In {
                span: _,
                bindings,
                iterable,
            } => {
                // Ranges are loop domains, not indexable array values. Evaluate
                // both endpoints once in source order, preserving their type.
                let range = match self.hir.pool.expr(*iterable).kind {
                    HirExprKind::Binary {
                        op: op @ (BinaryOp::RangeExclusive | BinaryOp::RangeInclusive),
                        left,
                        right,
                    } => Some((left, right, op == BinaryOp::RangeInclusive)),
                    _ => None,
                };
                let (iter_op, start, bound, index_ty, inclusive) =
                    if let Some((left, right, inclusive)) = range {
                        let ty = self.hir.pool.expr(left).ty;
                        let start = self.lower_expr(left, None, symbols)?;
                        let bound = self.lower_expr(right, None, symbols)?;
                        (None, start, Some(bound), ty, inclusive)
                    } else if let Some((elem_ty, inclusive)) =
                        match self.resolve_ty(self.hir.pool.expr(*iterable).ty) {
                            ArType::Named(sym, args) => self
                                .tc
                                .type_info
                                .type_interner
                                .type_args(args)
                                .first()
                                .copied()
                                .zip(symbols.try_get(sym).and_then(|s| s.lang_item).and_then(
                                    |item| match item {
                                        arandu_middle::symbol_table::LangItem::Range => Some(false),
                                        arandu_middle::symbol_table::LangItem::RangeInclusive => {
                                            Some(true)
                                        }
                                        _ => None,
                                    },
                                )),
                            _ => None,
                        }
                    {
                        let range_op = self.lower_expr(*iterable, None, symbols)?;
                        let start_temp = self.new_temp_id(elem_ty);
                        self.emit_assign_temp(
                            start_temp,
                            AmirRvalue::FieldAccess {
                                base: range_op,
                                field: 0,
                            },
                        );
                        let bound_temp = self.new_temp_id(elem_ty);
                        self.emit_assign_temp(
                            bound_temp,
                            AmirRvalue::FieldAccess {
                                base: range_op,
                                field: 1,
                            },
                        );
                        (
                            None,
                            AmirOperand::Copy(start_temp),
                            Some(AmirOperand::Copy(bound_temp)),
                            elem_ty,
                            inclusive,
                        )
                    } else {
                        // Only a unit compiled without `std.core.range` has
                        // a built-in range type; its values have no fields.
                        if matches!(
                            self.resolve_ty(self.hir.pool.expr(*iterable).ty),
                            ArType::Range(_)
                        ) {
                            return Err(crate::passes::lower_amir::amir_unsupported(
                                span,
                                "looping over a range value without `std.core.range`",
                                "write the range in the loop: `for i in start..end`",
                            ));
                        }
                        let iter_op = self.lower_expr(*iterable, None, symbols)?;
                        let zero = self.intern_literal_int("0");
                        (
                            Some(iter_op),
                            AmirOperand::Constant(zero),
                            None,
                            arandu_middle::types::TypeInterner::preinterned_primitive(
                                Primitive::Int,
                            ),
                            false,
                        )
                    };
                let idx_local = self.new_compiler_local_id(index_ty, span);
                self.emit_store_place(
                    AmirPlace {
                        local: idx_local,
                        projections: smallvec::SmallVec::new(),
                    },
                    start,
                )?;

                let len_local = self.new_compiler_local_id(index_ty, span);
                let bound = if let Some(bound) = bound {
                    bound
                } else if let Some(iter_op) = iter_op {
                    let len_temp = self.new_temp_id(index_ty);
                    self.emit_assign_temp(len_temp, AmirRvalue::Len(iter_op));
                    AmirOperand::Copy(len_temp)
                } else {
                    return Err(Diagnostic::ice(
                        crate::diagnostics::DiagCode::ICEL001,
                        "loop domain has neither endpoints nor iterable",
                        span,
                    ));
                };
                self.emit_store_place(
                    AmirPlace {
                        local: len_local,
                        projections: smallvec::SmallVec::new(),
                    },
                    bound,
                )?;

                let bb_cond = self.new_block();
                let bb_body = self.new_block_at(self.hir.pool.block(*body).span);
                let bb_step = self.new_block();
                let bb_exit = self.new_block();

                self.emit_goto(bb_cond);

                self.builder.current_block = Some(bb_cond);
                let idx_op = self.read_variable_source(idx_local)?;
                let len_op = self.read_variable_source(len_local)?;
                let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    cond_tmp,
                    AmirRvalue::Binary {
                        op: if inclusive {
                            BinaryOp::LtEqual
                        } else {
                            BinaryOp::Lt
                        },
                        left: idx_op,
                        right: len_op,
                    },
                );
                self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_body, bb_exit);
                self.seal_block(bb_body);

                let defer_depth = self.defer_frames.len();
                let scope_depth = self.local_scopes.len();
                self.begin_local_scope();
                self.loop_stack.push(crate::passes::lower_amir::LoopFrame {
                    label: label.clone(),
                    cont_block: bb_step,
                    exit_block: bb_exit,
                    defer_depth,
                    scope_depth,
                });
                self.builder.current_block = Some(bb_body);

                let bindings_slice = self.hir.pool.for_bindings_list(*bindings);
                // `for i, x in xs`: the position, counted from zero.
                if let [position, _, ..] = bindings_slice {
                    let local_id = self
                        .symbol_map
                        .get(&position.symbol)
                        .copied()
                        .unwrap_or_else(|| {
                            self.new_local_id(position.ty, position.symbol, position.span)
                        });
                    let mut current = self.read_variable_source(idx_local)?;
                    if iter_op.is_none() {
                        let offset = self.new_temp_id(index_ty);
                        self.emit_assign_temp(
                            offset,
                            AmirRvalue::Binary {
                                op: BinaryOp::Sub,
                                left: current,
                                right: start,
                            },
                        );
                        current = AmirOperand::Copy(offset);
                    }
                    let position_temp = self.new_temp_id(position.ty);
                    self.emit_assign_temp(position_temp, AmirRvalue::Use(current));
                    self.write_variable_source(local_id, AmirOperand::Copy(position_temp))?;
                }
                let element = if bindings_slice.len() >= 2 {
                    bindings_slice.get(1)
                } else {
                    bindings_slice.first()
                };
                if let Some(binding) = element {
                    let local_id = self
                        .symbol_map
                        .get(&binding.symbol)
                        .copied()
                        .unwrap_or_else(|| {
                            self.new_local_id(binding.ty, binding.symbol, binding.span)
                        });
                    let idx_op2 = self.read_variable_source(idx_local)?;
                    let elem_temp = self.new_temp_id(binding.ty);
                    self.emit_assign_temp(
                        elem_temp,
                        if let Some(base) = iter_op {
                            AmirRvalue::IndexAccess {
                                base,
                                index: idx_op2,
                                in_bounds: false,
                            }
                        } else {
                            AmirRvalue::Use(idx_op2)
                        },
                    );
                    let consumed = self.consume_operand(AmirOperand::Copy(elem_temp))?;
                    self.write_variable_source(local_id, consumed)?;
                }

                self.lower_block(*body, symbols)?;
                self.end_local_scope();
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_step);
                }
                self.loop_stack.pop();

                // A returning/breaking body may leave no step predecessor.
                // Do not materialize loads or a back edge in that orphan.
                self.finish_join(bb_step);
                if self.builder.current_block.is_some() {
                    let idx_op3 = self.read_variable_source(idx_local)?;
                    // Inclusive MAX is a valid final value. Exit before
                    // incrementing it instead of overflowing or wrapping.
                    if inclusive {
                        let end = self.read_variable_source(len_local)?;
                        let done = self.new_temp(ArType::Primitive(Primitive::Bool));
                        self.emit_assign_temp(
                            done,
                            AmirRvalue::Binary {
                                op: BinaryOp::Equal,
                                left: idx_op3,
                                right: end,
                            },
                        );
                        let increment = self.new_block();
                        self.set_bool_branch(AmirOperand::Copy(done), bb_exit, increment);
                        self.seal_block(increment);
                        self.builder.current_block = Some(increment);
                    }
                    let one_lit = self.intern_literal_int("1");
                    let next_idx = self.new_temp_id(index_ty);
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
                }
                self.seal_block(bb_cond);
                self.seal_block(bb_exit);

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
                let bb_body = self.new_block_at(self.hir.pool.block(*body).span);
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
                let scope_depth = self.local_scopes.len();
                self.loop_stack.push(crate::passes::lower_amir::LoopFrame {
                    label: label.clone(),
                    cont_block: bb_step,
                    exit_block: bb_exit,
                    defer_depth,
                    scope_depth,
                });
                self.builder.current_block = Some(bb_body);
                self.lower_block(*body, symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_step);
                }
                self.loop_stack.pop();

                // A returning/breaking body may leave no step predecessor.
                // Do not materialize loads or a back edge in that orphan.
                self.finish_join(bb_step);
                if let Some(s) = step {
                    self.lower_simple_stmt(s, symbols)?;
                }
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_cond);
                }
                self.seal_block(bb_cond);

                self.builder.current_block = Some(bb_exit);
            }
        }
        Ok(())
    }
}
