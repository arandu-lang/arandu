use super::super::FuncTranslator;
use crate::types::{self, Shape};
use arandu_middle::amir::value::{AmirOperand, AmirRvalue};
use arandu_middle::layout::TagEncoding;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_memory_rvalue(&mut self, rvalue: &AmirRvalue, result_ty: TypeId) {
        match rvalue {
            AmirRvalue::Discriminant { value } => {
                let operand_ty = self.operand_arity_ty(value);
                let enum_ty = self.strip_ref(operand_ty).unwrap_or(operand_ty);
                let layout = self.layout_of_id(enum_ty);
                if let Some(TagEncoding::Niche {
                    niche_offset,
                    niche_value,
                    untagged_variant,
                    tagged_variant,
                    ..
                }) = layout.tag_encoding
                {
                    self.code.push(Instruction::I32Const(tagged_variant as i32));
                    self.code
                        .push(Instruction::I32Const(untagged_variant as i32));
                    self.emit_operand(value, operand_ty);
                    self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                        offset: niche_offset,
                        align: 2,
                        memory_index: 0,
                    }));
                    self.code.push(Instruction::I32Const(niche_value as i32));
                    self.code.push(Instruction::I32Eq);
                    self.code.push(Instruction::Select);
                } else if let Some(TagEncoding::PointerTag {
                    tag_mask,
                    pointer_offset,
                    ..
                }) = layout.tag_encoding
                {
                    self.emit_operand(value, operand_ty);
                    self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                        offset: pointer_offset,
                        align: 2,
                        memory_index: 0,
                    }));
                    self.code.push(Instruction::I32Const(tag_mask as i32));
                    self.code.push(Instruction::I32And);
                } else {
                    self.emit_operand(value, operand_ty);
                    self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                }
            }
            AmirRvalue::Len(op) => {
                let ty = self.operand_arity_ty(op);
                if let ArType::Array(length, _) = self.interner.resolve(ty) {
                    // Fixed arrays are addresses, not fat pointers. Their
                    // length belongs to the type; local+1 is unrelated storage.
                    if let Ok(length) = u32::try_from(length) {
                        self.code.push(Instruction::I32Const(i32::from_le_bytes(
                            length.to_le_bytes(),
                        )));
                    } else {
                        // An unrepresentable wasm32 length must never truncate
                        // into a valid length or read an unrelated local.
                        self.code.push(Instruction::Unreachable);
                    }
                } else if types::shape(ty, self.interner, self.layout_engine.data_layout)
                    == Shape::Fat
                {
                    // Load both words through the operand path so literal
                    // strings and temporary views obey the same ABI.
                    self.emit_operand(op, ty);
                    self.code.push(Instruction::LocalSet(self.scratch));
                    self.code.push(Instruction::Drop);
                    self.code.push(Instruction::LocalGet(self.scratch));
                } else {
                    self.code.push(Instruction::Unreachable);
                }
            }
            AmirRvalue::SliceData(op) => {
                // Data pointer is the first slot.
                if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = op
                    && let Some(&base) = self.temp_local.get(temp)
                {
                    self.code.push(Instruction::LocalGet(base));
                    return;
                }
                self.code.push(Instruction::I32Const(0));
            }
            AmirRvalue::SliceView { data, len, .. } => {
                // Push data, then len.
                self.emit_operand(data, result_ty);
                self.emit_operand(len, result_ty);
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                // Return the address of the local.
                self.emit_place_address(place);
            }
            AmirRvalue::Load(place) => {
                self.emit_load(place, result_ty);
            }
            AmirRvalue::Alloc(size_op) => {
                self.emit_bump_alloc(size_op);
            }
            AmirRvalue::StrView { owner } => {
                // Pass through the owner fat pointer (data, len).
                self.emit_operand(owner, result_ty);
            }
            AmirRvalue::StrBytes { source } => {
                // `str` and `[]u8` share the target fat-pointer representation.
                self.emit_operand(source, result_ty);
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let elem_ty = match self.interner.resolve(result_ty) {
                    ArType::Slice(inner) | ArType::MutSlice(inner) => inner,
                    ArType::Ref(inner) | ArType::RefMut(inner) => {
                        match self.interner.resolve(inner) {
                            ArType::Slice(elem) | ArType::MutSlice(elem) => elem,
                            _ => result_ty,
                        }
                    }
                    _ => result_ty,
                };
                let elem_size = self.layout_of(&self.interner.resolve(elem_ty)).size.max(1) as i32;
                let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));

                let (slice_ptr_local, slice_len_local) = match slice {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                        let base = self.temp_local.get(t).copied().unwrap_or(0);
                        (base, base + 1)
                    }
                    _ => {
                        self.emit_operand(slice, result_ty);
                        self.code.push(Instruction::LocalSet(self.scratch_c));
                        self.code.push(Instruction::LocalSet(self.scratch_b));
                        (self.scratch_b, self.scratch_c)
                    }
                };

                self.emit_operand(start, int_ty);
                self.code.push(Instruction::LocalSet(self.scratch));
                self.emit_operand(len, int_ty);
                self.code.push(Instruction::LocalSet(self.scratch_d));

                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch_d));
                self.code.push(Instruction::I32Add);
                self.code.push(Instruction::LocalGet(slice_len_local));
                self.code.push(Instruction::I32GtU);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

                // Push new_ptr, then new_len (fat pointer stack convention)
                self.code.push(Instruction::LocalGet(slice_ptr_local));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(elem_size));
                self.code.push(Instruction::I32Mul);
                self.code.push(Instruction::I32Add);
                self.code.push(Instruction::LocalGet(self.scratch_d));
            }
            AmirRvalue::RelativeBorrow { local, .. } => {
                self.code
                    .push(Instruction::I32Const(local.as_usize() as i32));
            }
            _ => unreachable!(),
        }
    }
}
