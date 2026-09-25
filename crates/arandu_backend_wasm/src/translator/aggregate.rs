use arandu_middle::SymbolId;
use arandu_middle::amir::value::AmirOperand;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

use super::FuncTranslator;
use crate::types;

impl<'a> FuncTranslator<'a> {
    /// Heap-allocate a cell for a struct literal, store each field.
    pub(super) fn emit_struct_literal(
        &mut self,
        struct_symbol: SymbolId,
        fields: &[(arandu_middle::SmolStr, AmirOperand)],
        result_ty: TypeId,
    ) {
        let layout = self.layout_of_id(result_ty);
        let size = layout.size as i32;
        self.alloc_cell(size);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));
        // Store each field.
        if let Some(fields_def) = self.layout_provider.get_struct_fields(struct_symbol) {
            let owner_ty = self.interner.resolve(result_ty);
            for (name, operand) in fields {
                let Some(field_info) = fields_def.get(name) else {
                    continue;
                };
                let field_ty = arandu_middle::layout::instantiated_field_type(
                    &owner_ty,
                    name.as_str(),
                    self.interner,
                    self.layout_provider,
                )
                .unwrap_or(field_info.ty);
                let offset = layout
                    .field_offsets
                    .get(field_info.index)
                    .copied()
                    .unwrap_or(0);
                self.push_cell_addr();
                if self.is_owned_aggregate(field_ty) {
                    let size = self.layout_of_id(field_ty).size as i32;
                    self.code.push(Instruction::I32Const(offset as i32));
                    self.code.push(Instruction::I32Add);
                    self.code.push(Instruction::LocalSet(self.scratch_d));
                    self.emit_operand(operand, field_ty);
                    self.code.push(Instruction::LocalSet(self.scratch));
                    self.code.push(Instruction::LocalGet(self.scratch_d));
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    if matches!(operand, AmirOperand::Move(_)) {
                        self.code.push(Instruction::LocalGet(self.scratch));
                        self.code.push(Instruction::Call(self.free_func_idx));
                    }
                } else {
                    self.emit_operand(operand, field_ty);
                    self.emit_store_value_at(field_ty, offset);
                }
            }
        }
        self.push_cell_addr();
    }

    /// Heap-allocate a cell for a tuple literal, store each element.
    pub(super) fn emit_tuple_literal(&mut self, items: &[AmirOperand], result_ty: TypeId) {
        let layout = self.layout_of_id(result_ty);
        let size = layout.size as i32;
        self.alloc_cell(size);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));
        for (i, item) in items.iter().enumerate() {
            let item_ty = self.operand_arity_ty(item);
            let offset = layout.field_offsets.get(i).copied().unwrap_or(0);
            self.push_cell_addr();
            if self.is_owned_aggregate(item_ty) {
                let size = self.layout_of_id(item_ty).size as i32;
                self.code.push(Instruction::I32Const(offset as i32));
                self.code.push(Instruction::I32Add);
                self.code.push(Instruction::LocalSet(self.scratch_d));
                self.emit_operand(item, item_ty);
                self.code.push(Instruction::LocalSet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch_d));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(size));
                self.code.push(Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                if matches!(item, AmirOperand::Move(_)) {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::Call(self.free_func_idx));
                }
            } else {
                self.emit_operand(item, item_ty);
                self.emit_store_value_at(item_ty, offset);
            }
        }
        self.push_cell_addr();
    }

    /// Heap-allocate a cell for an array literal, store each element.
    pub(super) fn emit_array_literal(&mut self, items: &[AmirOperand], result_ty: TypeId) {
        let resolved = self.interner.resolve(result_ty);
        let ArType::Array(_, elem_ty) = resolved else {
            self.emit_zero(result_ty);
            return;
        };
        let elem_layout = self.layout_of(&self.interner.resolve(elem_ty));
        let layout = self.layout_of_id(result_ty);
        let size = layout.size as i32;
        self.alloc_cell(size);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));
        for (i, item) in items.iter().enumerate() {
            let offset = (i as u64) * elem_layout.size;
            self.push_cell_addr();
            if self.is_owned_aggregate(elem_ty) {
                let size = elem_layout.size as i32;
                self.code.push(Instruction::I32Const(offset as i32));
                self.code.push(Instruction::I32Add);
                self.code.push(Instruction::LocalSet(self.scratch_d));
                self.emit_operand(item, elem_ty);
                self.code.push(Instruction::LocalSet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch_d));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(size));
                self.code.push(Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                if matches!(item, AmirOperand::Move(_)) {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::Call(self.free_func_idx));
                }
            } else {
                self.emit_operand(item, elem_ty);
                self.emit_store_value_at(elem_ty, offset);
            }
        }
        self.push_cell_addr();
    }

    /// Emit a field access: load a field from a (heap-allocated) aggregate.
    pub(super) fn emit_field_access(
        &mut self,
        base: &AmirOperand,
        field_index: usize,
        result_ty: TypeId,
    ) {
        let base_ty = self.operand_arity_ty(base);
        let owner_ty = self.strip_ref(base_ty).unwrap_or(base_ty);
        let owner = self.interner.resolve(owner_ty);
        let layout = self.layout_of(&owner);
        let offset = layout.field_offsets.get(field_index).copied().unwrap_or(0);
        self.emit_operand(base, base_ty);
        if self.is_owned_aggregate(result_ty) {
            let size = self.layout_of_id(result_ty).size as i32;
            self.code.push(Instruction::I32Const(offset as i32));
            self.code.push(Instruction::I32Add);
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
        } else {
            self.emit_load_value_at(result_ty, offset);
        }
    }

    /// Emit an index access: bounds-check then load the element.
    pub(super) fn emit_index_access(
        &mut self,
        base: &AmirOperand,
        index: &AmirOperand,
        result_ty: TypeId,
    ) {
        let base_ty = self.operand_arity_ty(base);
        let owner_ty = self.strip_ref(base_ty).unwrap_or(base_ty);
        let owner = self.interner.resolve(owner_ty);
        let base_local = match base {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_local.get(t).copied(),
            _ => None,
        };
        // Push the base value that addresses the elements. For a fat slice
        // only the first slot (the data pointer) addresses the elements; the
        // second slot holds the length and must not leak onto the stack.
        let base_shape = types::shape(base_ty, self.interner, self.layout_engine.data_layout);
        match (base_shape, base_local) {
            (types::Shape::Fat, Some(b)) => {
                self.code.push(Instruction::LocalGet(b));
            }
            (types::Shape::Fat, None) => {
                self.code.push(Instruction::I32Const(0));
            }
            _ => self.emit_operand(base, base_ty),
        }
        let (_elem_ty, elem_size) = match &owner {
            ArType::Array(_, inner) => {
                let size = self.layout_of(&self.interner.resolve(*inner)).size;
                (*inner, size)
            }
            ArType::Slice(inner) => {
                let size = self.layout_of(&self.interner.resolve(*inner)).size;
                (*inner, size)
            }
            _ => {
                // Not indexable: leave the pushed base on the stack.
                return;
            }
        };
        let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
        // Bounds check: index < len.
        self.emit_operand(index, int_ty);
        match (&owner, base_local) {
            (ArType::Array(len, _), _) => {
                self.code.push(Instruction::I32Const(*len as i32));
            }
            (ArType::Slice(_), Some(b)) => {
                self.code.push(Instruction::LocalGet(b + 1));
            }
            (_, None) => {
                // Unknown length: trap on every index.
                self.code.push(Instruction::I32Const(0));
            }
            _ => {}
        }
        self.code.push(Instruction::I32GeU);
        self.code.push(Instruction::If(BlockType::Empty));
        self.code.push(Instruction::Unreachable);
        self.code.push(Instruction::End);
        // address = base + index * elem_size
        self.emit_operand(index, int_ty);
        self.code.push(Instruction::I32Const(elem_size as i32));
        self.code.push(Instruction::I32Mul);
        self.code.push(Instruction::I32Add);
        if self.is_owned_aggregate(result_ty) {
            let size = self.layout_of_id(result_ty).size as i32;
            if size > 0 {
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.alloc_cell(size);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Const(size));
                self.code.push(Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                self.code.push(Instruction::LocalGet(self.scratch));
            }
        } else {
            self.emit_load_value_at(result_ty, 0);
        }
    }
}
