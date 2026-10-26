use super::super::FuncTranslator;
use arandu_middle::amir::value::AmirRvalue;
use arandu_middle::types::TypeId;
use wasm_encoder::{BlockType, Instruction};

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_gen_rvalue(&mut self, rvalue: &AmirRvalue, _result_ty: TypeId) {
        match rvalue {
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                let size = self.layout_of_id(*payload_ty).size as i32;
                let size = size.max(1);
                self.alloc_cell(size);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                if self.is_owned_aggregate(*payload_ty) {
                    self.emit_operand(value, *payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_c));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_c));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                } else {
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 0);
                }
                self.emit_alloc_gen_tag(self.scratch_b);
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::LocalGet(self.scratch));
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => {
                let (ptr_local, gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.emit_validate_gen_ref(ptr_local, gen_local);

                if self.is_owned_aggregate(*payload_ty) {
                    self.code.push(Instruction::LocalGet(ptr_local));
                } else {
                    self.code.push(Instruction::LocalGet(ptr_local));
                    self.emit_load_value_at(*payload_ty, 0);
                }
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                let (ptr_local, gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.emit_validate_gen_ref(ptr_local, gen_local);

                if self.is_owned_aggregate(*payload_ty) {
                    let size = self.layout_of_id(*payload_ty).size as i32;
                    self.emit_operand(value, *payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_c));
                    self.code.push(Instruction::LocalGet(ptr_local));
                    self.code.push(Instruction::LocalGet(self.scratch_c));
                    self.code.push(Instruction::I32Const(size.max(0)));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                } else {
                    self.code.push(Instruction::LocalGet(ptr_local));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 0);
                }

                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::LocalGet(gen_local));
            }
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                let (ptr_local, gen_local) = self.resolve_gen_ref_slots(gen_ref);
                let size = self.layout_of_id(*payload_ty).size as i32;
                let size = size.max(1);

                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::LocalGet(gen_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::I32And);
                self.code.push(Instruction::If(BlockType::Empty));
                self.alloc_cell(size);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.emit_alloc_gen_tag(self.scratch_b);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::Else);
                self.emit_validate_gen_ref(ptr_local, gen_local);
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.code.push(Instruction::LocalGet(gen_local));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::End);

                if self.is_owned_aggregate(*payload_ty) {
                    self.emit_operand(value, *payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_d));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_d));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                } else {
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 0);
                }

                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::LocalGet(self.scratch_c));
            }
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                let (ptr_local, gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.emit_validate_gen_ref(ptr_local, gen_local);

                if self.is_owned_aggregate(*payload_ty) {
                    self.emit_invalidate_gen_cell(ptr_local);
                    self.code.push(Instruction::LocalGet(ptr_local));
                } else {
                    // Load the scalar payload onto the operand stack BEFORE freeing
                    // the cell so `__arandu_free` cannot clobber the payload bytes.
                    self.code.push(Instruction::LocalGet(ptr_local));
                    self.emit_load_value_at(*payload_ty, 0);
                    self.emit_invalidate_gen_cell(ptr_local);
                    self.code.push(Instruction::LocalGet(ptr_local));
                    self.code.push(Instruction::Call(self.free_func_idx));
                }
            }
            _ => unreachable!(),
        }
    }
    /// Increment the monotonic `GenRef` counter at [`crate::memory::GEN_COUNTER_ADDR`],
    /// write the odd generation tag `(gen << 1) | 1` into `*(cell_ptr_local - 4)`,
    /// and leave the new generation number in `self.scratch`.
    ///
    /// `__arandu_free` clears the reserved generation word independently of
    /// the free-list link. A freed cell therefore always fails the odd-tag
    /// check in [`Self::emit_validate_gen_ref`], and any
    /// recycled cell receives a strictly greater generation number.
    pub(super) fn emit_alloc_gen_tag(&mut self, cell_ptr_local: u32) {
        let counter_memarg = wasm_encoder::MemArg {
            offset: crate::memory::GEN_COUNTER_ADDR,
            align: 2,
            memory_index: 0,
        };
        // The low tag bit records liveness, leaving only 31 generation bits.
        // Exhaustion must trap rather than alias an earlier live or stale handle.
        self.code.extend([
            Instruction::I32Const(0),
            Instruction::I32Load(counter_memarg),
            Instruction::I32Const(i32::MAX),
            Instruction::I32GeU,
            Instruction::If(BlockType::Empty),
            Instruction::Unreachable,
            Instruction::End,
            Instruction::I32Const(0),
            Instruction::I32Const(0),
            Instruction::I32Load(counter_memarg),
            Instruction::I32Const(1),
            Instruction::I32Add,
            Instruction::LocalTee(self.scratch),
            Instruction::I32Store(counter_memarg),
            Instruction::LocalGet(cell_ptr_local),
            Instruction::I32Const(
                crate::memory::CELL_HEADER_SIZE - crate::memory::CELL_GENERATION_OFFSET,
            ),
            Instruction::I32Sub,
            Instruction::LocalGet(self.scratch),
            Instruction::I32Const(1),
            Instruction::I32Shl,
            Instruction::I32Const(1),
            Instruction::I32Or,
            Instruction::I32Store(crate::memory::noffset_memarg()),
        ]);
    }

    /// Trap if `(ptr_local, gen_local)` is null, points outside the heap, has
    /// been freed, or does not match the live generation tag at `*(ptr_local - 4)`.
    pub(super) fn emit_validate_gen_ref(&mut self, ptr_local: u32, gen_local: u32) {
        self.code.extend([
            Instruction::LocalGet(ptr_local),
            Instruction::GlobalGet(crate::memory::GLOBAL_HEAP_BASE),
            Instruction::I32Const(crate::memory::CELL_HEADER_SIZE),
            Instruction::I32Add,
            Instruction::I32LtU,
            Instruction::LocalGet(gen_local),
            Instruction::I32Eqz,
            Instruction::I32Or,
            Instruction::LocalGet(gen_local),
            Instruction::I32Const(i32::MAX),
            Instruction::I32GtU,
            Instruction::I32Or,
            Instruction::If(BlockType::Empty),
            Instruction::Unreachable,
            Instruction::End,
            Instruction::LocalGet(ptr_local),
            Instruction::I32Const(
                crate::memory::CELL_HEADER_SIZE - crate::memory::CELL_GENERATION_OFFSET,
            ),
            Instruction::I32Sub,
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::LocalGet(gen_local),
            Instruction::I32Const(1),
            Instruction::I32Shl,
            Instruction::I32Const(1),
            Instruction::I32Or,
            Instruction::I32Ne,
            Instruction::If(BlockType::Empty),
            Instruction::Unreachable,
            Instruction::End,
        ]);
    }

    /// Clear the generation tag at `*(ptr_local - 4)` before releasing or
    /// transferring a `GenRef` cell.
    pub(super) fn emit_invalidate_gen_cell(&mut self, ptr_local: u32) {
        self.code.extend([
            Instruction::LocalGet(ptr_local),
            Instruction::I32Const(
                crate::memory::CELL_HEADER_SIZE - crate::memory::CELL_GENERATION_OFFSET,
            ),
            Instruction::I32Sub,
            Instruction::I32Const(0),
            Instruction::I32Store(crate::memory::noffset_memarg()),
        ]);
    }
}
