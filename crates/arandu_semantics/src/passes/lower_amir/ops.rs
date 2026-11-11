use super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirExprKind};
use crate::ops::{BinaryOp, UnaryOp};
use crate::passes::type_checker::types::{ArType, Primitive};

impl LowerCtx<'_> {
    pub(crate) fn lower_binary(
        &mut self,
        op: BinaryOp,
        left: HirExprId,
        right: HirExprId,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let l_op = self.lower_expr(left, None, symbols)?;
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            if self.builder.current_block.is_none() {
                return Ok(AmirOperand::Copy(dest));
            }

            let bb_short = self.new_block();
            let bb_right = self.new_block();
            let bb_join = self.new_block();
            if matches!(op, BinaryOp::And) {
                self.set_bool_branch(l_op, bb_right, bb_short);
            } else {
                self.set_bool_branch(l_op, bb_short, bb_right);
            }
            self.seal_block(bb_short);
            self.seal_block(bb_right);

            self.builder.current_block = Some(bb_short);
            let result_local = self.new_compiler_local(ArType::Primitive(Primitive::Bool));
            self.write_variable(
                bb_short,
                result_local,
                AmirOperand::Constant(AmirConstant::Bool(matches!(op, BinaryOp::Or))),
            );
            self.emit_goto(bb_join);

            self.builder.current_block = Some(bb_right);
            let start_owned = self.owned_string_temps.clone();
            let right_op = self.lower_expr(right, None, symbols)?;
            // Temporaries of the right operand exist only on this edge; free
            // them here rather than at the join, where they are undefined.
            self.free_new_owned_string_temps(&start_owned, None);
            if self.builder.current_block.is_some() {
                let right_block = self.require_block()?;
                self.write_variable(right_block, result_local, right_op);
                self.emit_goto(bb_join);
            }

            self.seal_block(bb_join);
            self.builder.current_block = Some(bb_join);
            let result = self.read_variable(bb_join, result_local);
            self.emit_assign_temp(dest, AmirRvalue::Use(result));
            return Ok(AmirOperand::Copy(dest));
        }
        let r_op = self.lower_expr(right, None, symbols)?;
        let l_op = self.typed_integer_constant(left, l_op);
        let r_op = self.typed_integer_constant(right, r_op);
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
            && matches!(
                self.resolve_ty(self.hir.pool.expr(left).ty),
                ArType::Option(_) | ArType::Result(_, _)
            )
        {
            let equal = self.lower_value_equality(l_op, r_op, self.hir.pool.expr(left).ty)?;
            let value = if op == BinaryOp::NotEqual {
                AmirRvalue::Unary {
                    op: UnaryOp::Not,
                    operand: equal,
                }
            } else {
                AmirRvalue::Use(equal)
            };
            self.emit_assign_temp(dest, value);
            return Ok(AmirOperand::Copy(dest));
        }
        if matches!(op, BinaryOp::Add | BinaryOp::Sub)
            && let ArType::Ptr(pointee_id) = self.resolve_ty(self.hir.pool.expr(left).ty)
        {
            if matches!(op, BinaryOp::Sub)
                && matches!(
                    self.resolve_ty(self.hir.pool.expr(right).ty),
                    ArType::Ptr(_)
                )
            {
                let isize_ty = self
                    .tc
                    .type_info
                    .type_interner
                    .intern(ArType::Primitive(Primitive::ISize));
                let l_int = self.new_temp_id(isize_ty);
                self.emit_assign_temp(l_int, AmirRvalue::Use(l_op));
                let r_int = self.new_temp_id(isize_ty);
                self.emit_assign_temp(r_int, AmirRvalue::Use(r_op));
                let diff = self.new_temp_id(isize_ty);
                self.emit_assign_temp(
                    diff,
                    AmirRvalue::Binary {
                        op: BinaryOp::Sub,
                        left: AmirOperand::Copy(l_int),
                        right: AmirOperand::Copy(r_int),
                    },
                );
                let pointee_ty = self.resolve_ty(pointee_id);
                let stride = arandu_middle::layout::LayoutEngine::from_data_layout(
                    self.tc.type_info.target_layout,
                )
                .layout_of_type(
                    &pointee_ty,
                    &self.tc.type_info.type_interner,
                    &*self.tc.type_info,
                )
                .map(|l| l.size)
                .unwrap_or(1)
                .max(1);
                if stride > 1 {
                    let stride_lit = self.intern_literal_int(stride.to_string());
                    let stride_temp = self.new_temp_id(isize_ty);
                    self.emit_assign_temp(
                        stride_temp,
                        AmirRvalue::Use(AmirOperand::Constant(stride_lit)),
                    );
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::Binary {
                            op: BinaryOp::Div,
                            left: AmirOperand::Copy(diff),
                            right: AmirOperand::Copy(stride_temp),
                        },
                    );
                } else {
                    self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Copy(diff)));
                }
                return Ok(AmirOperand::Copy(dest));
            }
            return self.lower_ptr_arith_offset(op, l_op, r_op, pointee_id, expr_ty, Some(dest));
        }
        self.emit_assign_temp(
            dest,
            AmirRvalue::Binary {
                op,
                left: l_op,
                right: r_op,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_ptr_arith_offset(
        &mut self,
        op: BinaryOp,
        ptr_op: AmirOperand,
        offset_op: AmirOperand,
        pointee_id: crate::types::TypeId,
        ptr_ty_id: crate::types::TypeId,
        target: Option<TempId>,
    ) -> Result<AmirOperand, Diagnostic> {
        let usize_ty = self
            .tc
            .type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::USize));
        let ptr_int = self.new_temp_id(usize_ty);
        self.emit_assign_temp(ptr_int, AmirRvalue::Use(ptr_op));
        let off_int = self.new_temp_id(usize_ty);
        self.emit_assign_temp(off_int, AmirRvalue::Use(offset_op));
        let pointee_ty = self.resolve_ty(pointee_id);
        let stride =
            arandu_middle::layout::LayoutEngine::from_data_layout(self.tc.type_info.target_layout)
                .layout_of_type(
                    &pointee_ty,
                    &self.tc.type_info.type_interner,
                    &*self.tc.type_info,
                )
                .map(|l| l.size)
                .unwrap_or(1)
                .max(1);
        let scaled_off = if stride > 1 {
            let stride_lit = self.intern_literal_int(stride.to_string());
            let stride_temp = self.new_temp_id(usize_ty);
            self.emit_assign_temp(
                stride_temp,
                AmirRvalue::Use(AmirOperand::Constant(stride_lit)),
            );
            let scaled = self.new_temp_id(usize_ty);
            self.emit_assign_temp(
                scaled,
                AmirRvalue::Binary {
                    op: BinaryOp::Mul,
                    left: AmirOperand::Copy(off_int),
                    right: AmirOperand::Copy(stride_temp),
                },
            );
            AmirOperand::Copy(scaled)
        } else {
            AmirOperand::Copy(off_int)
        };
        let sum_int = self.new_temp_id(usize_ty);
        self.emit_assign_temp(
            sum_int,
            AmirRvalue::Binary {
                op,
                left: AmirOperand::Copy(ptr_int),
                right: scaled_off,
            },
        );
        let dest = target.unwrap_or_else(|| self.new_temp_id(ptr_ty_id));
        self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Copy(sum_int)));
        Ok(AmirOperand::Copy(dest))
    }

    /// Pool literals have spelling but no type. A bool destination cannot
    /// supply the operand width/sign of a comparison, and a shift count may
    /// have a different integer type from the result. Retain explicit integer
    /// types in SSA instead of making each consumer guess from the magnitude.
    fn typed_integer_constant(&mut self, expr: HirExprId, operand: AmirOperand) -> AmirOperand {
        let source = self.hir.pool.expr(expr);
        let ty = source.ty;
        let span = source.span;
        if self.integer_constant_needs_type(ty, operand) {
            let temp = self.with_span(span, |this| this.new_temp_id(ty));
            self.emit_assign_temp(temp, AmirRvalue::Use(operand));
            AmirOperand::Copy(temp)
        } else {
            operand
        }
    }

    pub(crate) fn lower_unary(
        &mut self,
        op: UnaryOp,
        sub_expr: HirExprId,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        // The checker validates a signed minimum as one negative literal.
        // Its positive magnitude is not a representable value of that type;
        // do not manufacture it as a VM/runtime intermediate before Neg. This
        // is literal normalization only, not wrapping a computed negation.
        if op == UnaryOp::Neg
            && let HirExprKind::Int(text) = &self.hir.pool.expr(sub_expr).kind
            && let ArType::Primitive(primitive) = self.resolve_ty(expr_ty)
            && primitive.is_signed()
            && let Ok(ty) = arandu_middle::ctfe::IntegerType::new(primitive, self.layout)
            && crate::literal_pool::parse_int_literal(text).and_then(i128::checked_neg)
                == Some(ty.min())
        {
            let operand = AmirOperand::Constant(self.intern_literal_int(ty.min().to_string()));
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            self.emit_assign_temp(dest, AmirRvalue::Use(operand));
            return Ok(AmirOperand::Copy(dest));
        }
        // F2.0: `&`/`&mut` lower to place borrows; `*` on a ref loads through the pointer.
        match op {
            UnaryOp::Ref | UnaryOp::RefMut => {
                // Borrowing a value with no place (`ref Point { .. }`,
                // `ref f()`) borrows a temporary local, as Rust does for
                // `&Point { .. }`; the scope destroys it.
                let place = match self.lower_expr_to_place(sub_expr, symbols) {
                    Ok(place) => place,
                    Err(_) => {
                        let sub = self.hir.pool.expr(sub_expr);
                        let (sub_ty, sub_span) = (sub.ty, sub.span);
                        let value = self.lower_expr(sub_expr, None, symbols)?;
                        let temporary = self.new_compiler_local_id(sub_ty, sub_span);
                        self.locals[temporary.as_usize()].is_memory = true;
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = value {
                            self.owned_string_temps.remove(&t);
                        }
                        self.write_variable_source(temporary, value)?;
                        crate::amir::AmirPlace {
                            local: temporary,
                            projections: smallvec::SmallVec::new(),
                        }
                    }
                };
                // F2.0: address-taken *stack* scalars need a stack home (`is_memory`).
                // BC.4a: a place that goes through `Deref` already has a materialised
                // pointer in the local's SSA value — do NOT force a stack slot for it
                // (stack_addr of the pointer cell ≠ the pointer itself).
                let through_ptr = place
                    .projections
                    .iter()
                    .any(|p| matches!(p, crate::amir::AmirProjection::Deref));
                if !through_ptr {
                    let idx = place.local.as_usize();
                    if idx < self.locals.len() {
                        self.locals[idx].is_memory = true;
                    }
                    self.borrowed_locals.insert(place.local);
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                let rv = if matches!(op, UnaryOp::RefMut) {
                    AmirRvalue::BorrowMut(place)
                } else {
                    AmirRvalue::Borrow(place)
                };
                self.emit_assign_temp(dest, rv);
                Ok(AmirOperand::Copy(dest))
            }
            UnaryOp::Deref => {
                // `*p` where p is `&T` / `&mut T` / local holding a ref: load pointee.
                // If sub is a place of the referent (`*&x` after fold would be x), use Load.
                // Otherwise treat the operand as a pointer value and load through it via
                // FieldAccess-free Load of a temporary place when possible.
                if let Ok(place) = self.lower_expr_to_place(sub_expr, symbols) {
                    // Local of type Ref/RefMut still needs one indirection — Load the place
                    // yields the reference bits; for stack locals of Ref, that *is* the
                    // pointer. Backend maps Load of Ref-typed local as "use pointer value"
                    // and for Borrow result we already have a pointer temp.
                    //
                    // Gold path: `*p` with p: &T → emit Load after reinterpreting.
                    // Use Unary Deref for pointer-valued operands so backends can load.
                    let sub_op = self.read_variable_source(place.local)?;
                    // A pointee with a destructor is read as a place so the move
                    // checker sees a consumption through the reference (O016).
                    if place.projections.is_empty()
                        && !arandu_mir::drop_elaborate::type_needs_drop(
                            expr_ty,
                            &*self.tc.type_info,
                        )
                    {
                        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::Unary {
                                op: UnaryOp::Deref,
                                operand: sub_op,
                            },
                        );
                        Ok(AmirOperand::Copy(dest))
                    } else {
                        self.mark_local_materialized(place.local);
                        // The projected place holds the reference itself
                        // (`*holder.field`); the pointee is one Deref further.
                        let mut pointee = place;
                        pointee.projections.push(crate::amir::AmirProjection::Deref);
                        Ok(self.load_into_target(target, pointee, expr_ty))
                    }
                } else {
                    let sub_op = self.lower_expr(sub_expr, None, symbols)?;
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::Unary {
                            op: UnaryOp::Deref,
                            operand: sub_op,
                        },
                    );
                    Ok(AmirOperand::Copy(dest))
                }
            }
            // A3.1: inside `async func` or `async { … }`, `await` is a CFG suspension
            // frontier. Sync drive-to-completion (await in non-coroutine context) stays
            // a plain unary await without Suspend.
            UnaryOp::Await if self.func_is_async || self.coroutine_depth > 0 => {
                let future_op = self.lower_expr(sub_expr, None, symbols)?;
                let resume = self.new_block();
                self.emit_suspend(future_op, resume)?;
                // Continue after the frontier; future still dominates resume (temps).
                self.builder.current_block = Some(resume);
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::Unary {
                        op: UnaryOp::Await,
                        operand: future_op,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
            _ => {
                let sub_op = self.lower_expr(sub_expr, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::Unary {
                        op,
                        operand: sub_op,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
        }
    }

    /// Lower a call argument with consume mode + W3 auto-ref materialization.
    ///
    /// Auto-ref only when the **argument value** is not already a ref and the
    /// formal is `&T`/`&mut T`. Re-borrowing an existing ref local would create
    /// a loan of the *pointer cell* (not the pointee) and breaks O003 on
    /// overlapping pointee loans (see cli_smoke O003 fixture).
    pub(crate) fn lower_call_arg(
        &mut self,
        arg: HirExprId,
        formal_index: usize,
        callee: crate::SymbolId,
        formal_ty: Option<&ArType>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let mode = self.arg_modes.kind(callee, formal_index);
        let arg_expr = self.hir.pool.expr(arg);
        let arg_ty = self.resolve_ty(arg_expr.ty);
        let formal_is_ref =
            formal_ty.is_some_and(|t| matches!(t, ArType::Ref(_) | ArType::RefMut(_)));
        let arg_is_ref = matches!(arg_ty, ArType::Ref(_) | ArType::RefMut(_));
        let exclusive = matches!(formal_ty, Some(ArType::RefMut(_))) || mode.is_exclusive();

        if let Some((binding, bound_ty)) = self.match_guard_binding_root(arg)
            && (exclusive || (!self.tc.type_info.is_copy(bound_ty) && !formal_is_ref))
        {
            let name = &symbols.get(binding).name;
            let (code, message) = if exclusive {
                (
                    crate::DiagCode::O003MutableBorrowConflict,
                    format!("cannot mutably borrow match-guard binding `{name}`"),
                )
            } else {
                (
                    crate::DiagCode::O002MoveWhileBorrowed,
                    format!("cannot move match-guard binding `{name}` before the guard succeeds"),
                )
            };
            return Err(Diagnostic::error(code, message, arg_expr.span).with_label(
                arg_expr.span,
                "guard bindings are shared views of the original scrutinee",
            ));
        }

        if formal_is_ref
            && !exclusive
            && let HirExprKind::Path { symbol } = &arg_expr.kind
            && let Some(&(local, _)) = self.guard_borrows.get(symbol)
        {
            return self.read_variable_source(local);
        }

        // W3.3 auto-ref: formal is ref, value is not — materialize Borrow of place (or temporary local for rvalue).
        if formal_is_ref && !arg_is_ref {
            let place = match self.lower_expr_to_place(arg, symbols) {
                Ok(place) => place,
                Err(_) => {
                    let val = self.lower_expr(arg, None, symbols)?;
                    let tmp = self.new_compiler_local_id(arg_expr.ty, arg_expr.span);
                    self.locals[tmp.as_usize()].is_memory = true;
                    // The scope-dropped local now owns an owned string, as a
                    // `let` binding does; the temp must not be freed again.
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = val {
                        self.owned_string_temps.remove(&t);
                    }
                    self.write_variable_source(tmp, val)?;
                    crate::amir::AmirPlace {
                        local: tmp,
                        projections: smallvec::SmallVec::new(),
                    }
                }
            };
            let through_ptr = place
                .projections
                .iter()
                .any(|p| matches!(p, crate::amir::AmirProjection::Deref));
            if !through_ptr {
                let idx = place.local.as_usize();
                if idx < self.locals.len() {
                    self.locals[idx].is_memory = true;
                }
            }
            let formal_tid = match formal_ty {
                Some(t) => self.intern_ty_ref(t),
                None => arg_expr.ty,
            };
            let dest = self.new_temp_id(formal_tid);
            self.borrowed_locals.insert(place.local);
            let rv = if exclusive {
                AmirRvalue::BorrowMut(place)
            } else {
                AmirRvalue::Borrow(place)
            };
            self.emit_assign_temp(dest, rv);
            return Ok(AmirOperand::Copy(dest));
        }

        // Preserve the source place for named-field move arguments. Ordinary field
        // value lowering produces `FieldAccess`, which intentionally carries only the
        // ordinal needed by codegen and loses the ownership path. A direct
        // `Load(place)` keeps stable field symbols without rebuilding them later.
        let projected_place = matches!(&arg_expr.kind, HirExprKind::Field { .. });
        let op = if !mode.is_borrow() && !arg_is_ref && projected_place {
            match self.lower_expr_to_place(arg, symbols) {
                Ok(place) => {
                    let root_has_destructor =
                        self.locals
                            .get(place.local.as_usize())
                            .is_some_and(|local| {
                                self.tc
                                    .type_info
                                    .destructor_instances
                                    .contains_key(&local.ty)
                            });
                    if !place.projections.is_empty()
                        && root_has_destructor
                        && !self.tc.type_info.is_copy(arg_expr.ty)
                    {
                        return Err(Diagnostic::error(
                            crate::DiagCode::U001FeatureNotSupported,
                            "cannot move a field out of a value with an explicit destructor",
                            arg_expr.span,
                        )
                        .with_note(
                            "the destructor requires the complete value; move the whole value or borrow the field",
                        ));
                    }
                    self.load_place(&place, arg_expr.ty)?
                }
                // A projection on a temporary is a value expression rather than a
                // place rooted in a local. Keep ordinary lowering for that case.
                Err(_) => self.lower_expr(arg, None, symbols)?,
            }
        } else {
            self.lower_expr(arg, None, symbols)?
        };
        if mode.is_borrow() || arg_is_ref {
            // shared/mut self or already a reference: do not move.
            Ok(op)
        } else {
            self.consume_operand(op)
        }
    }

    fn match_guard_binding_root(
        &self,
        expr_id: HirExprId,
    ) -> Option<(crate::SymbolId, crate::types::TypeId)> {
        let expr = self.hir.pool.expr(expr_id);
        match &expr.kind {
            HirExprKind::Path { symbol } => {
                self.guard_borrows.get(symbol).map(|(_, ty)| (*symbol, *ty))
            }
            HirExprKind::Field { base, .. }
            | HirExprKind::SafeField { base, .. }
            | HirExprKind::Index { base, .. }
            | HirExprKind::SafeIndex { base, .. } => self.match_guard_binding_root(*base),
            _ => None,
        }
    }
}
