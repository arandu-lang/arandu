use super::LowerCtx;
use crate::SymbolId;
use crate::SymbolTable;
use crate::amir::{AmirOperand, AmirPlace, AmirProjection, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirExprKind};
use crate::ops::{BinaryOp, UnaryOp};
use crate::passes::lower_amir::amir_unsupported;
use crate::passes::type_checker::types::ArType;
use crate::types::Primitive;

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
                let index_ty = self.hir.pool.expr(*index).ty;
                let index = self.lower_expr(*index, None, symbols)?;
                self.check_vec_index(&place, &container_ty, index, index_ty, symbols)?;
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
                && arandu_mir::drop_elaborate::type_needs_drop(expr_ty, &*self.tc.type_info)
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
                // A field of an indexed element is read in place: copying the
                // whole element out first would move an owning element.
                if !self.tc.type_info.is_copy(expr_ty)
                    || matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_))
                    || place
                        .projections
                        .iter()
                        .any(|projection| matches!(projection, AmirProjection::Index(_)))
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
        if let HirExprKind::Binary {
            op: op @ (BinaryOp::RangeExclusive | BinaryOp::RangeInclusive),
            left,
            right,
        } = self.hir.pool.expr(index).kind
        {
            let inclusive = op == BinaryOp::RangeInclusive;
            return self.lower_range_index(base, left, right, inclusive, expr_ty, target, symbols);
        }
        // The container keeps owning the element and destroys it later, so a
        // moved-out copy would be destroyed twice (Rust's E0507).
        if arandu_mir::drop_elaborate::type_needs_drop(expr_ty, &*self.tc.type_info) {
            return Err(Diagnostic::error(
                crate::DiagCode::O015MoveOutOfIndex,
                "cannot move a value with a destructor out of an indexed place",
                self.diag_span(self.current_span),
            )
            .with_note("borrow the element (`ref base[i]`) or remove it with `pop`/`swap`"));
        }
        if let Ok(mut place) = self.lower_expr_to_place(base, symbols) {
            let base_ty = self.resolve_ty(self.hir.pool.expr(base).ty);
            let through_reference = matches!(base_ty, ArType::Ref(_) | ArType::RefMut(_));
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
            // A `Vec` element is always read through its place, where its
            // length is checked.
            if is_vec
                || matches!(
                    container_ty,
                    ArType::Array(_, _) | ArType::Slice(_) | ArType::MutSlice(_)
                ) && (!self.tc.type_info.is_copy(expr_ty) || through_reference)
            {
                let index_ty = self.hir.pool.expr(index).ty;
                let idx = self.lower_expr(index, None, symbols)?;
                self.check_vec_index(&place, &container_ty, idx, index_ty, symbols)?;
                place.projections.push(AmirProjection::Index(idx));
                self.mark_local_materialized(place.local);
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                self.emit_assign_temp(dest, AmirRvalue::Load(place));
                return Ok(AmirOperand::Copy(dest));
            }
        }
        if matches!(
            self.resolve_ty(self.hir.pool.expr(index).ty),
            ArType::Range(_)
        ) {
            return Err(amir_unsupported(
                self.diag_span(self.current_span),
                "indexing with a range value",
                "write the range literally: `s[start..end]`",
            ));
        }
        let base_ty_id = self.hir.pool.expr(base).ty;
        let base_op = self.lower_expr(base, None, symbols)?;
        let index_ty = self.hir.pool.expr(index).ty;
        let idx_op = self.lower_expr(index, None, symbols)?;
        let base_ty = self.resolve_ty(base_ty_id);
        if arandu_middle::types::is_vec_type(&base_ty, symbols) {
            // A `Vec` value with no place (e.g. a call result): give it one so
            // its length can be read and checked.
            let local = self.new_compiler_local_id(base_ty_id, self.current_span);
            self.mark_local_materialized(local);
            self.write_variable_source(local, base_op)?;
            let mut place = AmirPlace {
                local,
                projections: smallvec::SmallVec::new(),
            };
            self.check_vec_index(&place, &base_ty, idx_op, index_ty, symbols)?;
            place.projections.push(AmirProjection::Index(idx_op));
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            self.emit_assign_temp(dest, AmirRvalue::Load(place));
            return Ok(AmirOperand::Copy(dest));
        }
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::IndexAccess {
                base: base_op,
                index: idx_op,
                in_bounds: false,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    /// Lowers `base[start..end]` (or `..=`) on an array or slice to a checked
    /// [`AmirRvalue::SliceSubslice`]. Out-of-range bounds reach an
    /// `Unreachable` block, which every backend lowers to a trap, like an
    /// out-of-bounds element index.
    #[allow(clippy::too_many_arguments)]
    fn lower_range_index(
        &mut self,
        base: HirExprId,
        start: HirExprId,
        end: HirExprId,
        inclusive: bool,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let base_ty_id = self.hir.pool.expr(base).ty;
        let (slice, len) = match self.resolve_ty(base_ty_id) {
            ArType::Slice(_) | ArType::MutSlice(_) => {
                let slice = self.lower_expr(base, None, symbols)?;
                let len = self.new_temp(ArType::Primitive(Primitive::USize));
                self.emit_assign_temp(len, AmirRvalue::Len(slice));
                (slice, AmirOperand::Copy(len))
            }
            ArType::Array(count, _) => {
                let place = self.lower_expr_to_place(base, symbols)?;
                self.mark_local_materialized(place.local);
                let owner = self.new_temp(ArType::Ref(base_ty_id));
                self.emit_assign_temp(owner, AmirRvalue::Borrow(place));
                let len = AmirOperand::Constant(self.intern_literal_int(count.to_string()));
                let slice = self.new_temp_id(expr_ty);
                self.emit_assign_temp(
                    slice,
                    AmirRvalue::SliceView {
                        owner: AmirOperand::Copy(owner),
                        data: AmirOperand::Copy(owner),
                        len,
                    },
                );
                (AmirOperand::Copy(slice), len)
            }
            _ => {
                return Err(amir_unsupported(
                    self.diag_span(self.current_span),
                    "range index outside arrays and slices",
                    "take a slice first, e.g. `v.asSlice()[start..end]`",
                ));
            }
        };
        let bound_ty = self.resolve_ty(self.hir.pool.expr(start).ty);
        let start = self.lower_expr(start, None, symbols)?;
        let end = self.lower_expr(end, None, symbols)?;
        let trap = self.new_block();
        if bound_ty.is_signed() {
            let zero = AmirOperand::Constant(self.intern_literal_int("0"));
            let nonnegative = self.range_binary(BinaryOp::GtEqual, start, zero, Primitive::Bool);
            self.trap_unless(nonnegative, trap);
        }
        let start = self.range_bound(start);
        let end = self.range_bound(end);
        let ordered = self.range_binary(BinaryOp::LtEqual, start, end, Primitive::Bool);
        self.trap_unless(ordered, trap);
        let fits_op = if inclusive {
            BinaryOp::Lt
        } else {
            BinaryOp::LtEqual
        };
        let fits = self.range_binary(fits_op, end, len, Primitive::Bool);
        self.trap_unless(fits, trap);
        self.seal_block(trap);
        let mut count = self.range_binary(BinaryOp::Sub, end, start, Primitive::USize);
        if inclusive {
            let one = AmirOperand::Constant(self.intern_literal_int("1"));
            count = self.range_binary(BinaryOp::Add, count, one, Primitive::USize);
        }
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::SliceSubslice {
                slice,
                start,
                len: count,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    /// Traps unless `index` addresses an initialized element of the `Vec` at
    /// `container`. Backends check arrays and slices themselves, but a `Vec`
    /// element is a raw buffer access whose bound is the `len` field, so the
    /// check lives here, like Rust's `Index for Vec`.
    pub(crate) fn check_vec_index(
        &mut self,
        container: &AmirPlace,
        container_ty: &ArType,
        index: AmirOperand,
        index_ty: crate::types::TypeId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let mut len_place = container.clone();
        let container_ty = match container_ty {
            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                len_place.projections.push(AmirProjection::Deref);
                self.tc.type_info.type_interner.resolve(*inner)
            }
            other => other.clone(),
        };
        if !arandu_middle::types::is_vec_type(&container_ty, symbols) {
            return Ok(());
        }
        let Some(len_field) = self.resolve_field_symbol(&container_ty, "len") else {
            return Err(Diagnostic::ice(
                crate::diagnostics::DiagCode::ICEL001,
                "Vec has no `len` field to bound an index",
                self.diag_span(self.current_span),
            ));
        };
        len_place.projections.push(AmirProjection::Field(len_field));
        self.mark_local_materialized(len_place.local);
        let len = self.new_temp(ArType::Primitive(Primitive::USize));
        self.emit_assign_temp(len, AmirRvalue::Load(len_place));
        let trap = self.new_block();
        if self.resolve_ty(index_ty).is_signed() {
            let zero = AmirOperand::Constant(self.intern_literal_int("0"));
            let nonnegative = self.range_binary(BinaryOp::GtEqual, index, zero, Primitive::Bool);
            self.trap_unless(nonnegative, trap);
        }
        let index = self.range_bound(index);
        let below = self.range_binary(BinaryOp::Lt, index, AmirOperand::Copy(len), Primitive::Bool);
        self.trap_unless(below, trap);
        self.seal_block(trap);
        Ok(())
    }

    fn range_binary(
        &mut self,
        op: BinaryOp,
        left: AmirOperand,
        right: AmirOperand,
        result: Primitive,
    ) -> AmirOperand {
        let temp = self.new_temp(ArType::Primitive(result));
        self.emit_assign_temp(temp, AmirRvalue::Binary { op, left, right });
        AmirOperand::Copy(temp)
    }

    /// The bound converted to `usize`; signed bounds were checked nonnegative.
    fn range_bound(&mut self, bound: AmirOperand) -> AmirOperand {
        let temp = self.new_temp(ArType::Primitive(Primitive::USize));
        self.emit_assign_temp(temp, AmirRvalue::Use(bound));
        AmirOperand::Copy(temp)
    }

    /// Continues in a fresh block reached when `condition` holds; otherwise
    /// control enters `trap`.
    fn trap_unless(&mut self, condition: AmirOperand, trap: crate::amir::BlockId) {
        let ok = self.new_block();
        self.set_bool_branch(condition, ok, trap);
        self.seal_block(ok);
        self.builder.current_block = Some(ok);
    }
}
