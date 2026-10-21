use super::super::FuncTranslator;
use arandu_middle::amir::value::AmirRvalue;
use arandu_middle::layout::TagEncoding;
use arandu_middle::types::TypeId;
use wasm_encoder::Instruction;

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_aggregate_rvalue(&mut self, rvalue: &AmirRvalue, result_ty: TypeId) {
        match rvalue {
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                self.emit_struct_literal(*struct_symbol, fields, result_ty);
            }
            AmirRvalue::Tuple { items } => {
                self.emit_tuple_literal(items, result_ty);
            }
            AmirRvalue::Array { items } => {
                self.emit_array_literal(items, result_ty);
            }
            AmirRvalue::FieldAccess { base, field } => {
                self.emit_field_access(base, *field, result_ty);
            }
            AmirRvalue::IndexAccess { base, index } => {
                self.emit_index_access(base, index, result_ty);
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                // Allocate layout-driven cell, store tag + payload.
                self.emit_bump_alloc_enum(*variant_tag, *payload, result_ty);
            }
            AmirRvalue::EnumPayload { value, .. } => {
                // Load the payload field from an enum cell at its layout offset.
                let operand_ty = self.operand_arity_ty(value);
                let enum_ty = self.strip_ref(operand_ty).unwrap_or(operand_ty);
                let layout = self.layout_of_id(enum_ty);
                if let Some(TagEncoding::PointerTag {
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
                    let mask_inv = !(tag_mask as i32);
                    self.code.push(Instruction::I32Const(mask_inv));
                    self.code.push(Instruction::I32And);
                    return;
                }
                let payload_offset =
                    if matches!(layout.tag_encoding, Some(TagEncoding::Niche { .. })) {
                        0
                    } else {
                        layout.field_offsets.get(1).copied().unwrap_or(4)
                    };
                if self.is_owned_aggregate(result_ty) {
                    let size = self.layout_of_id(result_ty).size as i32;
                    self.emit_operand(value, operand_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_c));
                    self.alloc_cell(size);
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_c));
                    self.code.push(Instruction::I32Const(payload_offset as i32));
                    self.code.push(Instruction::I32Add);
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                } else {
                    self.emit_operand(value, operand_ty);
                    self.emit_load_value_at(result_ty, payload_offset);
                }
            }
            _ => unreachable!(),
        }
    }
}
