use super::LowerCtx;
use crate::SymbolId;
use crate::SymbolTable;
use crate::amir::{AmirOperand, AmirPlace, AmirProjection, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirExprKind};
use crate::ops::UnaryOp;
use crate::passes::type_checker::types::ArType;

impl LowerCtx<'_> {
    /// Keep dummy `Store`s for `local` so projected places see a defined SSA value.
    ///
    /// `prune_dummy_loads_stores` drops plain stores to non-`is_memory` locals (Ref/Ptr
    /// are scalar). A place with `Deref`/`Field` is addressed via that local's value in
    /// codegen — without a surviving store, `use_var` is undef (SIGSEGV). Setting
    /// `is_memory` is the same flag F2.0 already uses for address-taken homes; for
    /// pointer-sized types it does **not** force a stack slot (`needs_scalar_stack_home`
    /// is only true for primitive scalars).
    pub(crate) fn mark_local_materialized(&mut self, local: crate::amir::LocalId) {
        let idx = local.as_usize();
        if idx < self.locals.len() {
            self.locals[idx].is_memory = true;
        }
    }

    /// Lower an expression to an [`AmirPlace`] for Borrow/Load (F2.0 + BC.4a).
    ///
    /// Gold path: locals, `*p` through ptr/ref, and field chains.
    pub(crate) fn lower_expr_to_place(
        &mut self,
        expr_id: HirExprId,
        symbols: &SymbolTable,
    ) -> Result<AmirPlace, Diagnostic> {
        let expr = self.hir.pool.expr(expr_id);
        match &expr.kind {
            HirExprKind::Path { symbol } => {
                if let Some(&(local_id, _)) = self.guard_borrows.get(symbol) {
                    return Ok(AmirPlace {
                        local: local_id,
                        projections: smallvec::smallvec![AmirProjection::Deref],
                    });
                }
                if let Some(&local_id) = self.symbol_map.get(symbol) {
                    return Ok(AmirPlace {
                        local: local_id,
                        projections: smallvec::smallvec![],
                    });
                }
                Err(self.move_diag(format!(
                    "cannot take address of non-local `{}`",
                    symbols.get(*symbol).name
                )))
            }
            // BC.4a: `*p` where `p` is a local holding a pointer — place through that pointer.
            HirExprKind::Unary {
                op: UnaryOp::Deref,
                expr: inner,
            } => {
                let mut place = self.lower_expr_to_place(*inner, symbols)?;
                let base_ty = self.resolve_ty(self.hir.pool.expr(*inner).ty);
                if !matches!(
                    base_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                ) {
                    return Err(self.move_diag(
                        "BC.4a: can only form a place through `ptr[T]`, `&T`, or `&mut T`",
                    ));
                }
                place.projections.push(AmirProjection::Deref);
                // Projected places address through the local's *value*. Dummy Stores to
                // non-memory Ref/Ptr locals are pruned — keep this base materialised so
                // Cranelift `use_var` sees a defined pointer (BC.4a).
                self.mark_local_materialized(place.local);
                Ok(place)
            }
            HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
                let mut place = self.lower_expr_to_place(*base, symbols)?;
                let base_ty = self.resolve_ty(self.hir.pool.expr(*base).ty);
                if matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)) {
                    place.projections.push(AmirProjection::Deref);
                }
                let field_sym = self
                    .resolve_field_symbol(&base_ty, field.as_str())
                    .ok_or_else(|| {
                        self.move_diag(format!(
                            "cannot borrow field `{}`: symbol not resolved",
                            field
                        ))
                    })?;
                place.projections.push(AmirProjection::Field(field_sym));
                self.mark_local_materialized(place.local);
                Ok(place)
            }
            HirExprKind::Index { base, index } => {
                let mut place = self.lower_expr_to_place(*base, symbols)?;
                let base_ty = self.resolve_ty(self.hir.pool.expr(*base).ty);
                let container_ty = match &base_ty {
                    ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                        self.tc.type_info.type_interner.resolve(*inner)
                    }
                    _ => base_ty.clone(),
                };
                let is_vec = arandu_middle::types::is_vec_type(&container_ty, symbols);
                if matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)) {
                    place.projections.push(AmirProjection::Deref);
                }
                if !matches!(
                    container_ty,
                    ArType::Array(_, _) | ArType::Slice(_) | ArType::MutSlice(_)
                ) && !is_vec {
                    return Err(self.move_diag(
                        "can only borrow an indexed place from an array, slice, or vector",
                    ));
                }
                let index = self.lower_expr(*index, None, symbols)?;
                place.projections.push(AmirProjection::Index(index));
                self.mark_local_materialized(place.local);
                Ok(place)
            }
            // Keep the reference returned by a call as a pointer place. Copying
            // the projected payload instead would duplicate owned fields when
            // auto-borrowing `owner.get().field.method()`.
            HirExprKind::Call { .. }
                if matches!(self.resolve_ty(expr.ty), ArType::Ref(_) | ArType::RefMut(_)) =>
            {
                let value = self.lower_expr(expr_id, None, symbols)?;
                let local = self.new_compiler_local_id(expr.ty, expr.span);
                self.mark_local_materialized(local);
                self.write_variable_source(local, value)?;
                Ok(AmirPlace {
                    local,
                    projections: smallvec::SmallVec::new(),
                })
            }
            _ => Err(self.move_diag(
                "can only borrow locals, indexed array/slice elements, `*p` through a pointer, and field paths",
            )),
        }
    }

    /// Struct symbol for a base type (unwraps Ptr/Ref/RefMut/Nullable).
    fn struct_id_of_base(&self, base_ty: &ArType) -> Option<SymbolId> {
        let interner = &self.tc.type_info.type_interner;
        match base_ty {
            ArType::Named(id, _) => Some(*id),
            ArType::Nullable(inner)
            | ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner) => {
                let inner_ty = interner.resolve(*inner);
                match inner_ty {
                    ArType::Named(id, _) => Some(id),
                    ArType::Ptr(ptr_inner) | ArType::Ref(ptr_inner) | ArType::RefMut(ptr_inner) => {
                        match interner.resolve(ptr_inner) {
                            ArType::Named(id, _) => Some(id),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn resolve_field_symbol(&self, base_ty: &ArType, field: &str) -> Option<SymbolId> {
        let sid = self.struct_id_of_base(base_ty)?;
        self.tc
            .type_info
            .struct_fields
            .get(&sid)
            .and_then(|m| m.get(field).and_then(|f| f.symbol))
    }

    pub(crate) fn resolve_field_index(&self, base_ty: &ArType, field: &str) -> usize {
        if let Ok(idx) = field.parse::<usize>() {
            return idx;
        }
        if field.starts_with('_')
            && let Ok(idx) = field[1..].parse::<usize>()
        {
            return idx;
        }
        self.struct_id_of_base(base_ty)
            .and_then(|sid| self.tc.type_info.struct_fields.get(&sid))
            .and_then(|m| m.get(field))
            .map(|f| f.index)
            .unwrap_or(0)
    }

    pub(crate) fn lower_field(
        &mut self,
        base: HirExprId,
        field: &str,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        if let Ok(mut place) = self.lower_expr_to_place(base, symbols) {
            let base_ty = self.resolve_ty(self.hir.pool.expr(base).ty);
            let root_ty = self.resolve_ty(self.locals[place.local.as_usize()].ty);
            let through_shared_borrow = matches!(root_ty, ArType::Ref(_));
            if through_shared_borrow
                && crate::drop_elaborate::type_needs_drop(expr_ty, &self.tc.type_info)
            {
                return Err(Diagnostic::error(
                    crate::DiagCode::O002MoveWhileBorrowed,
                    "cannot move a non-Copy field out through a borrowed place",
                    self.diag_span(self.current_span),
                )
                .with_note("use an ownership-taking operation on the owner instead"));
            }
            if matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)) {
                place.projections.push(AmirProjection::Deref);
            }
            if let Some(field_sym) = self.resolve_field_symbol(&base_ty, field) {
                place.projections.push(AmirProjection::Field(field_sym));
                if !self.tc.type_info.is_copy(expr_ty)
                    || matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_))
                {
                    self.mark_local_materialized(place.local);
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                    self.emit_assign_temp(dest, AmirRvalue::Load(place));
                    return Ok(AmirOperand::Copy(dest));
                }
            }
        }

        // Fields on temporary rvalues are not addressable source places. Keep
        // the value projection for those expressions; named places take the
        // path above so the move checker can invalidate the selected field.
        let base_op = self.lower_expr(base, None, symbols)?;
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        let base_ty = self.resolve_ty(self.hir.pool.expr(base).ty);
        let field_idx = self.resolve_field_index(&base_ty, field);
        self.emit_assign_temp(
            dest,
            AmirRvalue::FieldAccess {
                base: base_op,
                field: field_idx,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_index(
        &mut self,
        base: HirExprId,
        index: HirExprId,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        if let Ok(mut place) = self.lower_expr_to_place(base, symbols) {
            let base_ty = self.resolve_ty(self.hir.pool.expr(base).ty);
            let through_reference = matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_));
            let root_ty = self.resolve_ty(self.locals[place.local.as_usize()].ty);
            let through_shared_borrow = matches!(root_ty, ArType::Ref(_));
            if through_shared_borrow
                && crate::drop_elaborate::type_needs_drop(expr_ty, &self.tc.type_info)
            {
                return Err(Diagnostic::error(
                    crate::DiagCode::O002MoveWhileBorrowed,
                    "cannot move a non-Copy indexed value out through a borrowed place",
                    self.diag_span(self.current_span),
                )
                .with_note("use an ownership-taking operation on the owner instead"));
            }
            if matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)) {
                place.projections.push(AmirProjection::Deref);
            }
            let container_ty = match base_ty {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    self.tc.type_info.type_interner.resolve(inner)
                }
                _ => base_ty,
            };
            let is_vec = arandu_middle::types::is_vec_type(&container_ty, symbols);
            if (matches!(
                container_ty,
                ArType::Array(_, _) | ArType::Slice(_) | ArType::MutSlice(_)
            ) || is_vec)
                && (!self.tc.type_info.is_copy(expr_ty) || through_reference)
            {
                let idx = self.lower_expr(index, None, symbols)?;
                place.projections.push(AmirProjection::Index(idx));
                self.mark_local_materialized(place.local);
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                self.emit_assign_temp(dest, AmirRvalue::Load(place));
                return Ok(AmirOperand::Copy(dest));
            }
        }
        let base_op = self.lower_expr(base, None, symbols)?;
        let idx_op = self.lower_expr(index, None, symbols)?;
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::IndexAccess {
                base: base_op,
                index: idx_op,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }
}
