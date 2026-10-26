use arandu_middle::amir::value::AmirOperand;
use arandu_middle::layout::TagEncoding;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::Instruction;

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    /// Heap-allocate a cell sized `size` and record the address in `scratch`.
    ///
    /// Delegates to the module-wide runtime allocator (`__arandu_alloc`), so
    /// every cell shares the `{magic, size, next}` block format that
    /// `__arandu_free` understands — `free`/`Destroy` can actually reclaim it.
    pub(in crate::translator) fn alloc_cell(&mut self, size: i32) {
        if let Some(slot) = self.current_heap_home.take() {
            self.alloc_heap_home(slot, size);
            return;
        }
        if let Some(offset) = self.current_cell_home.take() {
            let Ok(offset) = i32::try_from(offset) else {
                self.code.push(Instruction::Unreachable);
                return;
            };
            self.code.push(Instruction::LocalGet(self.frame_base));
            self.code.push(Instruction::I32Const(offset));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalSet(self.scratch));
            return;
        }
        self.code.push(Instruction::I32Const(size.max(0)));
        self.code.push(Instruction::Call(self.alloc_func_idx));
        self.code.push(Instruction::LocalSet(self.scratch));
    }

    /// Push the current literal cell address (stable across nested allocations).
    pub(in crate::translator) fn push_cell_addr(&mut self) {
        self.code.push(Instruction::LocalGet(self.scratch_b));
    }

    /// Heap-allocate a dynamic buffer of `size_op` bytes with auto-grow.
    pub(in crate::translator) fn emit_bump_alloc(&mut self, size_op: &AmirOperand) {
        let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
        // Delegate to the shared runtime allocator so that `free` on the
        // returned `ptr[T]` finds a tracked block with a valid header.
        self.emit_operand(size_op, int_ty);
        self.code.push(Instruction::Call(self.alloc_func_idx));
    }

    /// Emit an enum construct: allocate layout-driven cell, store tag + payload.
    pub(in crate::translator) fn emit_bump_alloc_enum(
        &mut self,
        variant_tag: usize,
        payload: Option<AmirOperand>,
        result_ty: TypeId,
    ) {
        let layout = self.layout_of_id(result_ty);
        let size = layout.size.max(1) as i32;
        self.alloc_cell(size);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));

        let tag_is_niche = matches!(layout.tag_encoding, Some(TagEncoding::Niche { .. }));

        if let Some(TagEncoding::Niche {
            niche_offset,
            niche_value,
            tagged_variant,
            ..
        }) = layout.tag_encoding
        {
            if variant_tag == tagged_variant {
                self.push_cell_addr();
                self.code.push(Instruction::I32Const(niche_value as i32));
                self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: niche_offset,
                    align: 2,
                    memory_index: 0,
                }));
            }
        } else if let Some(TagEncoding::PointerTag {
            tag_mask,
            pointer_offset,
            ..
        }) = layout.tag_encoding
        {
            if let Some(op) = &payload {
                let payload_ty = self.operand_arity_ty(op);
                self.push_cell_addr();
                self.emit_operand(op, payload_ty);
                let tag_val = (variant_tag as u64) & tag_mask;
                let mask_inv = !(tag_mask as i32);
                self.code.push(Instruction::I32Const(mask_inv));
                self.code.push(Instruction::I32And);
                self.code.push(Instruction::I32Const(tag_val as i32));
                self.code.push(Instruction::I32Or);
                self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: pointer_offset,
                    align: 2,
                    memory_index: 0,
                }));
            } else {
                self.push_cell_addr();
                let tag_val = (variant_tag as u64) & tag_mask;
                self.code.push(Instruction::I32Const(tag_val as i32));
                self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: pointer_offset,
                    align: 2,
                    memory_index: 0,
                }));
            }
        } else {
            // Direct tag stored at offset 0.
            self.push_cell_addr();
            self.code.push(Instruction::I32Const(variant_tag as i32));
            self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
        }

        // Store payload at its layout offset (for non-PointerTag or if payload not inlined with tag).
        if let Some(op) = payload {
            let is_pointer_tag =
                matches!(layout.tag_encoding, Some(TagEncoding::PointerTag { .. }));
            if !is_pointer_tag {
                let payload_offset = if tag_is_niche {
                    0
                } else {
                    layout.field_offsets.get(1).copied().unwrap_or(4)
                };
                let payload_ty = self.operand_arity_ty(&op);
                self.push_cell_addr();
                if self.is_owned_aggregate(payload_ty) {
                    let p_size = self.layout_of_id(payload_ty).size as i32;
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.emit_operand(&op, payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::I32Const(payload_offset as i32));
                    self.code.push(Instruction::I32Add);
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::I32Const(p_size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                } else {
                    self.emit_operand(&op, payload_ty);
                    self.emit_store_value_at(payload_ty, payload_offset);
                }
            }
        }
        // Return the cell address (from the backup, since nested allocations
        // may have overwritten self.scratch).
        self.code.push(Instruction::LocalGet(self.scratch_b));
    }
}
