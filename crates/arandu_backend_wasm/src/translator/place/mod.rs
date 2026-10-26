use arandu_middle::amir::value::{AmirOperand, AmirPlace};
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{Instruction, ValType};

use super::FuncTranslator;
use crate::types::{self, Shape};

mod alloc;
mod projection;
mod store;

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

    /// Check whether a type is an owned aggregate backed by a heap cell.
    pub(super) fn is_owned_aggregate(&self, ty: TypeId) -> bool {
        self.interner.with_type(ty, |ar| {
            matches!(
                ar,
                ArType::Named(..)
                    | ArType::Array(..)
                    | ArType::Tuple(..)
                    | ArType::Option(..)
                    | ArType::Result(..)
                    | ArType::Poll(..)
            )
        })
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
                Some(ValType::V128) => self.code.push(Instruction::V128Store(memarg(4))),
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
                Some(ValType::V128) => self.code.push(Instruction::V128Load(memarg(4))),
                _ => self.code.push(Instruction::I32Load(memarg(2))),
            },
        }
    }

    /// Emit a Load: read from a place into the wasm stack.
    pub(super) fn emit_load(&mut self, place: &AmirPlace, result_ty: TypeId) {
        if place.projections.is_empty() {
            let local = &self.func.locals[place.local.as_usize()];
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
            if local.is_memory && !pointer_like && !self.is_owned_aggregate(result_ty) {
                let slot = self.local_slot(local.id).unwrap_or(0);
                self.code.push(Instruction::LocalGet(slot));
                self.emit_load_value_at(result_ty, 0);
                return;
            }
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
        if self.is_owned_aggregate(elem_ty) {
            self.emit_copy_aggregate_at(elem_ty);
        } else {
            self.emit_load_value_at(elem_ty, 0);
        }
    }

    /// Copy inline aggregate bytes at the address on the stack into a value cell.
    pub(super) fn emit_copy_aggregate_at(&mut self, ty: TypeId) {
        let size = self.layout_of_id(ty).size as i32;
        self.code.push(Instruction::LocalSet(self.scratch_c));
        self.alloc_cell(size);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_c));
        self.code.push(Instruction::I32Const(size));
        self.code.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        self.code.push(Instruction::LocalGet(self.scratch_b));
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
        let mut from_memory = false;
        for proj in &place.projections {
            cur_ty = self.emit_place_projection(cur_ty, proj, base_slot, &mut from_memory);
        }
        cur_ty
    }
}
