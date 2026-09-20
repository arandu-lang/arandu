use arandu_middle::SymbolId;
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::{AmirOperand, AmirPlace, AmirProjection};
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction, ValType};

use super::FuncTranslator;
use crate::types::{self, Shape};

impl<'a> FuncTranslator<'a> {
    /// Emit `free(op)`: release the tracked heap block behind an owned cell or
    /// raw allocation through the runtime `__arandu_free`.
    ///
    /// Only the pointer slot (slot 0) is pushed: for fat operands (`str`/`list`)
    /// the allocation is the data pointer, never the length. `__arandu_free`
    /// validates the block magic, so rodata-backed payloads and `0` degrade to
    /// a safe no-op.
    pub(super) fn emit_free(&mut self, op: &AmirOperand) {
        match op {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                let local = self.temp_local.get(temp).copied().unwrap_or(0);
                self.code.push(Instruction::LocalGet(local));
            }
            AmirOperand::Constant(_) | AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => {
                // No meaningful pointer to release; push a null pointer and let
                // the runtime treat it as a no-op.
                self.code.push(Instruction::I32Const(0));
            }
        }
        self.code.push(Instruction::Call(self.free_func_idx));
    }

    /// Emit `Destroy(place)`: run the nominal type's `@Destructor` (if any) on
    /// the value and reclaim the heap cell that backs it.
    ///
    /// `Destroy` is produced by drop elaboration for an owned composite place,
    /// and recurses into fields only when the type has no explicit destructor —
    /// so the places seen here have at most `Field` projections, all of them
    /// memory-backed aggregates. The destructor receives the *value* of the
    /// place, which for a heap-backed aggregate is its cell pointer: the root
    /// local already holds it, while a projected field stores its own cell
    /// pointer inline (fields are boxed), so it must be loaded from the field
    /// address.
    ///
    /// Releasing the cell is what makes `Destroy` the storage cleanup for the
    /// whole value, not just its user-defined resource: types without a
    /// registered destructor still free their cell. `__arandu_free` validates
    /// the block magic, so a pointer that was never allocated here (rodata,
    /// null) degrades to a safe no-op.
    pub(super) fn emit_destroy(&mut self, place: &AmirPlace) {
        let Some(ty_id) = self.place_resolved_ty(place) else {
            return;
        };
        if !matches!(self.interner.resolve(ty_id), ArType::Named(..)) {
            return;
        }
        // Materialize the cell pointer once and stash it: it is both the
        // destructor receiver and the block released afterwards. `scratch` is
        // not touched by the call instruction, so it survives the destructor.
        if place.projections.is_empty() {
            let Some(slot) = self.local_slot(place.local) else {
                return;
            };
            self.code.push(Instruction::LocalGet(slot));
        } else {
            self.emit_place_address(place);
            self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
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
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::Call(self.free_func_idx));
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
                AmirProjection::Deref => {
                    cur_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                }
                AmirProjection::Index(_) => {
                    let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                    let owner = self.interner.resolve(owner_ty);
                    match owner {
                        ArType::Array(_, inner)
                        | ArType::Slice(inner)
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

    /// Check whether a type is an owned aggregate backed by a heap cell.
    pub(super) fn is_owned_aggregate(&self, ty: TypeId) -> bool {
        self.interner.with_type(ty, |ar| {
            matches!(
                ar,
                ArType::Named(..) | ArType::Array(..) | ArType::Tuple(..)
            )
        })
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
    pub(super) fn emit_store(&mut self, lhs: &AmirPlace, rhs: &AmirOperand) {
        let local_ty = self.func.locals[lhs.local.as_usize()].ty;

        if lhs.projections.is_empty() {
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
        self.emit_operand(rhs, store_ty);
        self.emit_store_value_at(store_ty, 0);
    }

    /// Emit a typed store of the value on the stack top into the address below
    /// it, at a fixed cell offset. Stack order: `[addr, value]`. For fat values
    /// the stack is `[addr, ptr, len]`: both words are written into the cell
    /// (`ptr` at `offset`, `len` at `offset + 4`).
    pub(super) fn emit_store_value_at(&mut self, value_ty: TypeId, offset: u64) {
        let shape = types::shape(value_ty, self.interner, self.layout_engine.data_layout);
        if matches!(shape, types::Shape::Fat) {
            let memarg = |o: u64| wasm_encoder::MemArg {
                offset: o,
                align: 2,
                memory_index: 0,
            };
            // Reorder the stack into the three words, then emit two stores.
            self.code.push(Instruction::LocalSet(self.scratch_d));
            self.code.push(Instruction::LocalSet(self.scratch_c));
            self.code.push(Instruction::LocalSet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Store(memarg(offset)));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Store(memarg(offset + 4)));
            return;
        }
        let size = self.layout_of_id(value_ty).size;
        let vt = types::slot_valtype(value_ty, 0, self.interner, self.layout_engine.data_layout);
        let memarg = |align: u32| wasm_encoder::MemArg {
            offset,
            align,
            memory_index: 0,
        };
        match size {
            1 => self.code.push(Instruction::I32Store8(memarg(0))),
            2 => self.code.push(Instruction::I32Store16(memarg(1))),
            _ => match vt {
                Some(ValType::I64) => self.code.push(Instruction::I64Store(memarg(3))),
                Some(ValType::F32) => self.code.push(Instruction::F32Store(memarg(2))),
                Some(ValType::F64) => self.code.push(Instruction::F64Store(memarg(3))),
                _ => self.code.push(Instruction::I32Store(memarg(2))),
            },
        }
    }

    /// Emit a typed load from the address on the stack top, at a fixed offset.
    pub(super) fn emit_load_value_at(&mut self, value_ty: TypeId, offset: u64) {
        let shape = types::shape(value_ty, self.interner, self.layout_engine.data_layout);
        if matches!(shape, types::Shape::Fat) {
            let memarg = |o: u64| wasm_encoder::MemArg {
                offset: o,
                align: 2,
                memory_index: 0,
            };
            self.code.push(Instruction::LocalSet(self.scratch));
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Load(memarg(offset)));
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Load(memarg(offset + 4)));
            return;
        }
        let size = self.layout_of_id(value_ty).size;
        let signed = !types::ar_is_unsigned(value_ty, self.interner);
        let vt = types::slot_valtype(value_ty, 0, self.interner, self.layout_engine.data_layout);
        let memarg = |align: u32| wasm_encoder::MemArg {
            offset,
            align,
            memory_index: 0,
        };
        match size {
            1 => {
                if signed {
                    self.code.push(Instruction::I32Load8S(memarg(0)));
                } else {
                    self.code.push(Instruction::I32Load8U(memarg(0)));
                }
            }
            2 => {
                if signed {
                    self.code.push(Instruction::I32Load16S(memarg(1)));
                } else {
                    self.code.push(Instruction::I32Load16U(memarg(1)));
                }
            }
            _ => match vt {
                Some(ValType::I64) => self.code.push(Instruction::I64Load(memarg(3))),
                Some(ValType::F32) => self.code.push(Instruction::F32Load(memarg(2))),
                Some(ValType::F64) => self.code.push(Instruction::F64Load(memarg(3))),
                _ => self.code.push(Instruction::I32Load(memarg(2))),
            },
        }
    }

    /// Emit a Load: read from a place into the wasm stack.
    pub(super) fn emit_load(&mut self, place: &AmirPlace, result_ty: TypeId) {
        if place.projections.is_empty() {
            // Empty-projection loads are plain local value copies.
            let shape = types::shape(result_ty, self.interner, self.layout_engine.data_layout);
            if let Some(base) = self.local_slot(place.local) {
                match shape {
                    Shape::Empty => {}
                    Shape::Scalar => {
                        self.code.push(Instruction::LocalGet(base));
                    }
                    Shape::Fat => {
                        self.code.push(Instruction::LocalGet(base));
                        self.code.push(Instruction::LocalGet(base + 1));
                    }
                }
            }
            return;
        }
        let elem_ty = self.emit_place_address(place);
        self.emit_load_value_at(elem_ty, 0);
    }

    /// Compute the real cell address of a place and return its element type.
    ///
    /// Starts from the base local's stored pointer (or value for a
    /// pointer-like scalar) and walks every projection, adding field/index
    /// offsets as required by the layout engine.
    pub(super) fn emit_place_address(&mut self, place: &AmirPlace) -> TypeId {
        let Some(local) = self.func.locals.get(place.local.as_usize()) else {
            let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
            self.code.push(Instruction::I32Const(0));
            return int_ty;
        };
        let base_slot = self.local_slot(local.id);
        match base_slot {
            Some(slot) => self.code.push(Instruction::LocalGet(slot)),
            None => self.code.push(Instruction::I32Const(0)),
        }
        let mut cur_ty = local.ty;
        for proj in &place.projections {
            cur_ty = self.emit_place_projection(cur_ty, proj, base_slot);
        }
        cur_ty
    }

    /// Emit a single projection on the current address and return the element
    /// type after the projection.
    fn emit_place_projection(
        &mut self,
        cur_ty: TypeId,
        proj: &AmirProjection,
        base_slot: Option<u32>,
    ) -> TypeId {
        match proj {
            AmirProjection::Deref => {
                // Address unchanged; unwrap one pointer layer.
                self.strip_ref(cur_ty).unwrap_or(cur_ty)
            }
            AmirProjection::Field(symbol_id) => {
                let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                let owner = self.interner.resolve(owner_ty);
                let ArType::Named(struct_id, _) = owner else {
                    return cur_ty;
                };
                let Some(sym) = self.symbols.try_get(*symbol_id) else {
                    return cur_ty;
                };
                let name = &sym.name;
                let Some(fields) = self.layout_provider.get_struct_fields(struct_id) else {
                    return cur_ty;
                };
                let Some(field_info) = fields.get(name) else {
                    return cur_ty;
                };
                let struct_layout = self.layout_of(&owner);
                let offset = struct_layout
                    .field_offsets
                    .get(field_info.index)
                    .copied()
                    .unwrap_or(0);
                if offset > 0 {
                    self.code.push(Instruction::I32Const(offset as i32));
                    self.code.push(Instruction::I32Add);
                }
                match arandu_middle::layout::instantiated_field_type(
                    &owner,
                    name,
                    self.interner,
                    self.layout_provider,
                ) {
                    Some(field_ty) => field_ty,
                    None => cur_ty,
                }
            }
            AmirProjection::Index(index_op) => {
                let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                let owner = self.interner.resolve(owner_ty);
                let elem_ty = match owner {
                    ArType::Slice(inner) | ArType::Array(_, inner) => inner,
                    _ => return cur_ty,
                };
                let elem_size = self.layout_of(&self.interner.resolve(elem_ty)).size;
                let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                // Bounds check: index < len.
                self.emit_operand(index_op, int_ty);
                match owner {
                    ArType::Array(len, _) => {
                        self.code.push(Instruction::I32Const(len as i32));
                    }
                    ArType::Slice(_) => {
                        let len_slot = base_slot.unwrap_or(0) + 1;
                        self.code.push(Instruction::LocalGet(len_slot));
                    }
                    _ => {}
                }
                self.code.push(Instruction::I32GeU);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);
                // address = base + index * elem_size
                self.emit_operand(index_op, int_ty);
                self.code.push(Instruction::I32Const(elem_size as i32));
                self.code.push(Instruction::I32Mul);
                self.code.push(Instruction::I32Add);
                elem_ty
            }
        }
    }

    /// Heap-allocate a cell sized `size` and record the address in `scratch`.
    ///
    /// Delegates to the module-wide runtime allocator (`__arandu_alloc`), so
    /// every cell shares the `{magic, size, next}` block format that
    /// `__arandu_free` understands — `free`/`Destroy` can actually reclaim it.
    pub(super) fn alloc_cell(&mut self, size: i32) {
        self.code.push(Instruction::I32Const(size.max(0)));
        self.code.push(Instruction::Call(self.alloc_func_idx));
        self.code.push(Instruction::LocalSet(self.scratch));
    }

    /// Push the current literal cell address (stable across nested allocations).
    pub(super) fn push_cell_addr(&mut self) {
        self.code.push(Instruction::LocalGet(self.scratch_b));
    }

    /// Heap-allocate a dynamic buffer of `size_op` bytes with auto-grow.
    pub(super) fn emit_bump_alloc(&mut self, size_op: &AmirOperand) {
        let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
        // Delegate to the shared runtime allocator so that `free` on the
        // returned `ptr[T]` finds a tracked block with a valid header.
        self.emit_operand(size_op, int_ty);
        self.code.push(Instruction::Call(self.alloc_func_idx));
    }

    /// Emit an enum construct: allocate layout-driven cell, store tag + payload.
    pub(super) fn emit_bump_alloc_enum(
        &mut self,
        variant_tag: usize,
        payload: Option<AmirOperand>,
        result_ty: TypeId,
    ) {
        let layout = self.layout_of_id(result_ty);
        let size = layout.size as i32;
        let payload_offset = layout.field_offsets.get(1).copied().unwrap_or(4);
        self.alloc_cell(size);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));
        // Store tag at offset 0.
        self.push_cell_addr();
        self.code.push(Instruction::I32Const(variant_tag as i32));
        self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        // Store payload at its layout offset.
        if let Some(op) = payload {
            let payload_ty = self.operand_arity_ty(&op);
            self.push_cell_addr();
            self.emit_operand(&op, payload_ty);
            self.emit_store_value_at(payload_ty, payload_offset);
        }
        // Return the cell address (from the backup, since nested allocations
        // may have overwritten self.scratch).
        self.code.push(Instruction::LocalGet(self.scratch_b));
    }
}
