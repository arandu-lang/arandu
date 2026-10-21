use arandu_middle::SymbolId;
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::{AmirOperand, AmirPlace, AmirProjection};
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::Instruction;

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    /// Emit `Destroy(place)`: run the nominal type's `@Destructor` (if any) on
    /// the value and reclaim the heap cell that backs it.
    ///
    /// `Destroy` is produced by drop elaboration for an owned composite place,
    /// and recurses into fields only when the type has no explicit destructor —
    /// so the places seen here have at most `Field` projections, all of them
    /// memory-backed aggregates. The destructor receives the address of the
    /// value: the root local stores its heap-cell pointer, while projected
    /// fields are stored inline and use the projected address directly.
    ///
    /// Only a root place owns a heap cell that can be released here. A
    /// projected field borrows storage from its containing cell, which is
    /// released when the root is destroyed. `__arandu_free` validates the
    /// block magic, so an unknown pointer degrades to a safe no-op.
    pub(in crate::translator) fn emit_destroy(&mut self, place: &AmirPlace) {
        let Some(ty_id) = self.place_resolved_ty(place) else {
            return;
        };
        let resolved_ty = self.interner.resolve(ty_id);
        if matches!(resolved_ty, ArType::Primitive(Primitive::Str)) {
            if place.projections.is_empty() {
                let Some(slot) = self.local_slot(place.local) else {
                    return;
                };
                let is_memory = self
                    .func
                    .locals
                    .get(place.local.as_usize())
                    .is_some_and(|local| local.is_memory);
                if is_memory {
                    self.code.push(Instruction::LocalGet(slot));
                    self.code
                        .push(Instruction::I32Load(crate::memory::noffset_memarg()));
                    self.code.push(Instruction::Call(self.free_func_idx));
                    self.code.push(Instruction::LocalGet(slot));
                    self.code.push(Instruction::I32Const(0));
                    self.code
                        .push(Instruction::I32Store(crate::memory::noffset_memarg()));
                } else {
                    self.code.push(Instruction::LocalGet(slot));
                    self.code.push(Instruction::Call(self.free_func_idx));
                    self.code.push(Instruction::I32Const(0));
                    self.code.push(Instruction::LocalSet(slot));
                }
            } else {
                self.emit_place_address(place);
                self.code.push(Instruction::LocalSet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code
                    .push(Instruction::I32Load(crate::memory::noffset_memarg()));
                self.code.push(Instruction::Call(self.free_func_idx));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(0));
                self.code
                    .push(Instruction::I32Store(crate::memory::noffset_memarg()));
            }
            return;
        }
        if !matches!(resolved_ty, ArType::Named(..)) {
            return;
        }
        // Materialize the value address once. `scratch` is not touched by the
        // call instruction, so it survives the destructor.
        if place.projections.is_empty() {
            let Some(slot) = self.local_slot(place.local) else {
                return;
            };
            self.code.push(Instruction::LocalGet(slot));
        } else {
            self.emit_place_address(place);
        }
        self.code.push(Instruction::LocalSet(self.scratch));
        // Defensive: an unregistered destructor would make the call target
        // invalid. It cannot happen for well-formed program functions, but a
        // missing entry must degrade to a safe no-op rather than bad code.
        if let Some(destructor) = self.layout_provider.destructor_for_type(ty_id)
            && let Some(&func_idx) = self.func_index_map.get(&destructor)
        {
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::Call(func_idx));
        }
        if place.projections.is_empty() {
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::Call(self.free_func_idx));
        }
    }

    /// Resolve the type of a place without emitting code.
    ///
    /// Walks the same projections [`Self::emit_place_address`] understands and
    /// returns `None` for an unknown local or an unsupported projection.
    fn place_resolved_ty(&self, place: &AmirPlace) -> Option<TypeId> {
        let local = self.func.locals.get(place.local.as_usize())?;
        let mut cur_ty = local.ty;
        for proj in &place.projections {
            match proj {
                AmirProjection::Field(symbol_id) => {
                    cur_ty = self.field_projection_ty(cur_ty, *symbol_id)?;
                }
                AmirProjection::Variant(_) => {}
                AmirProjection::Payload { field_ty, .. } => {
                    cur_ty = *field_ty;
                }
                AmirProjection::TupleField(index) => {
                    cur_ty = match self.interner.resolve(cur_ty) {
                        ArType::Tuple(args) => {
                            self.interner.type_args(args).get(*index).copied()?
                        }
                        _ => return None,
                    };
                }
                AmirProjection::Deref => {
                    cur_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                }
                AmirProjection::Index(_) | AmirProjection::IndexConstant(_) => {
                    let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                    let owner = self.interner.resolve(owner_ty);
                    match owner {
                        ArType::Array(_, inner)
                        | ArType::Slice(inner)
                        | ArType::MutSlice(inner)
                        | ArType::ConstArray(_, inner) => {
                            cur_ty = inner;
                        }
                        _ => return None,
                    }
                }
            }
        }
        Some(cur_ty)
    }

    /// Resolve the declared type of a `Field` projection.
    fn field_projection_ty(&self, owner_ty: TypeId, symbol_id: SymbolId) -> Option<TypeId> {
        let owner_ty = self.strip_ref(owner_ty).unwrap_or(owner_ty);
        let owner = self.interner.resolve(owner_ty);
        let ArType::Named(..) = owner else {
            return None;
        };
        let sym = self.symbols.try_get(symbol_id)?;
        let name = &sym.name;
        arandu_middle::layout::instantiated_field_type(
            &owner,
            name,
            self.interner,
            self.layout_provider,
        )
    }

    /// Whether `temp` is an entry parameter being bound into a local by move.
    ///
    /// The prologue emits `local = Copy(param_temp)` for every parameter, but
    /// the parameter temp is dead afterwards, so the binding is an ownership
    /// transfer rather than a value copy for non-`Copy` types. Aliasing the
    /// incoming cell avoids both a needless heap copy and a leaked temporary —
    /// most visibly in `@Destructor` receivers (`own self`), which are always
    /// non-`Copy`. `Copy` parameters keep the clone to preserve value semantics
    /// (the caller still owns and reads its own cell).
    fn binds_param_by_move(&self, temp: TempId) -> bool {
        self.func.params.contains(&temp)
            && !self
                .func
                .temps
                .get(temp.as_usize())
                .map(|t| t.is_copy)
                .unwrap_or(false)
    }

    /// Emit a Store: `lhs_place = rhs`.
    ///
    /// Empty-projection places are value copies into the local (`local.set`).
    /// For owned aggregates, copying creates a new cell with `memory.copy` to
    /// preserve Arandu value semantics. Projected places go through the real
    /// cell address produced by [`Self::emit_place_address`].
    pub(in crate::translator) fn emit_store(&mut self, lhs: &AmirPlace, rhs: &AmirOperand) {
        let local_ty = self.func.locals[lhs.local.as_usize()].ty;

        if lhs.projections.is_empty() {
            let local = &self.func.locals[lhs.local.as_usize()];
            let ty = self.interner.resolve(local.ty);
            let pointer_like = matches!(
                ty,
                ArType::Ptr(_)
                    | ArType::Ref(_)
                    | ArType::RefMut(_)
                    | ArType::Nullable(_)
                    | ArType::Slice(_)
                    | ArType::MutSlice(_)
            );
            if local.is_memory && !pointer_like && !self.is_owned_aggregate(local_ty) {
                let slot = self.local_slot(local.id).unwrap_or(0);
                self.code.push(Instruction::LocalGet(slot));
                self.emit_operand(rhs, local_ty);
                self.emit_store_value_at(local_ty, 0);
                return;
            }
            let Some(base) = self.local_slot(lhs.local) else {
                return;
            };
            if self.is_owned_aggregate(local_ty)
                && let AmirOperand::Copy(t) = rhs
                && !self.binds_param_by_move(*t)
            {
                let size = self.layout_of_id(local_ty).size as i32;
                if size > 0 {
                    self.alloc_cell(size);
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    let src_slot = self.temp_local.get(t).copied().unwrap_or(0);
                    self.code.push(Instruction::LocalGet(src_slot));
                    self.code.push(Instruction::LocalSet(self.scratch));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalSet(base));
                    return;
                }
            }
            self.emit_operand_to_local(rhs, local_ty, base);
            return;
        }

        let store_ty = self.emit_place_address(lhs);
        if self.is_owned_aggregate(store_ty) {
            let size = self.layout_of_id(store_ty).size as i32;
            if size > 0 {
                // Named aggregates are represented by pointers in locals, but
                // struct fields contain their complete inline layout. Copying
                // only the pointer-sized first word corrupts multi-word fields
                // such as Vec { data, len, capacity }.
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.emit_operand(rhs, store_ty);
                self.code.push(Instruction::LocalSet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(size));
                self.code.push(Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                // Moving an owned aggregate into an inline field transfers
                // its contents, not its heap cell. Reclaim that temporary
                // source cell after copying; the destination belongs to the
                // containing aggregate.
                if matches!(rhs, AmirOperand::Move(_)) {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::Call(self.free_func_idx));
                }
                return;
            }
        }
        self.emit_operand(rhs, store_ty);
        self.emit_store_value_at(store_ty, 0);
    }
}
