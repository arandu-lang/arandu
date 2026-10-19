//! Operand loading and rvalue emission into the wasm instruction buffer.

use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::{AmirOperand, AmirRvalue};
use arandu_middle::layout::TagEncoding;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction, ValType};

use super::FuncTranslator;
use crate::types::{self, Shape};

impl<'a> FuncTranslator<'a> {
    /// Emit an AMIR assign: `lhs = rhs`.
    pub(super) fn emit_assign(&mut self, lhs: TempId, rhs: &AmirRvalue) {
        let lhs_ty = self.func.temps[lhs.as_usize()].ty;
        let lhs_shape = types::shape(lhs_ty, self.interner, self.layout_engine.data_layout);

        match lhs_shape {
            Shape::Empty => {}
            Shape::Scalar => {
                let local = self.temp_local.get(&lhs).copied().unwrap_or(0);
                if self.is_owned_aggregate(lhs_ty)
                    && let AmirRvalue::Use(AmirOperand::Copy(t)) = rhs
                {
                    let size = self.layout_of_id(lhs_ty).size as i32;
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
                        self.code.push(Instruction::LocalSet(local));
                        return;
                    }
                }
                self.emit_rvalue(rhs, lhs_ty);
                // Apply narrowing for sub-byte/16-bit integers.
                if let Some(narrow) = types::narrow_info(lhs_ty, self.interner) {
                    self.emit_narrow(narrow);
                }
                self.code.push(Instruction::LocalSet(local));
            }
            Shape::Fat => {
                self.emit_fat_assign(lhs, lhs_ty, rhs);
            }
        }
    }

    /// Emit an rvalue: push the result onto the wasm stack.
    pub(super) fn emit_rvalue(&mut self, rvalue: &AmirRvalue, result_ty: TypeId) {
        match rvalue {
            AmirRvalue::Use(op) => {
                self.emit_operand(op, result_ty);
            }
            AmirRvalue::Binary { op, left, right } => {
                self.emit_binary(*op, left, right, result_ty);
            }
            AmirRvalue::Unary { op, operand } => {
                self.emit_unary(*op, operand, result_ty);
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                self.emit_vector_compare(*op, left, right);
            }
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => {
                self.emit_operand(if_true, result_ty);
                self.emit_operand(if_false, result_ty);
                let mask_ty = self
                    .interner
                    .intern(ArType::Vector(arandu_middle::types::VectorType::U8x16));
                self.emit_operand(mask, mask_ty);
                self.code.push(Instruction::V128Bitselect);
            }
            AmirRvalue::VectorOp { op, left, right } => {
                self.emit_vector_op(*op, left, right, result_ty);
            }
            AmirRvalue::VectorSplat { value } => self.emit_vector_splat(value, result_ty),
            AmirRvalue::VectorShuffle { left, right, mask } => {
                self.emit_operand(left, result_ty);
                self.emit_operand(right, result_ty);
                self.code.push(Instruction::I8x16Shuffle(*mask));
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                self.emit_vector_extract(vector, *lane);
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                self.emit_vector_insert(vector, *lane, value, result_ty);
            }
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
            AmirRvalue::RelativeBorrow { local, .. } => {
                self.code
                    .push(Instruction::I32Const(local.as_usize() as i32));
            }
            AmirRvalue::CoroutineReady {
                value, payload_ty, ..
            } => {
                let layout = self.layout_of_id(*payload_ty);
                let size = ((8 + layout.size).max(16)) as i32;
                self.alloc_cell(size);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                // disc = 0 (Ready) at offset 0
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                // magic = CO_MAGIC (0x4152434f) at offset 4
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Const(0x4152_434f));
                self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 4,
                    align: 2,
                    memory_index: 0,
                }));
                // payload at offset 8
                let shape =
                    types::shape(*payload_ty, self.interner, self.layout_engine.data_layout);
                if !matches!(shape, Shape::Empty) {
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 8);
                }
                self.code.push(Instruction::LocalGet(self.scratch_b));
            }
            AmirRvalue::StringInterp { parts } => {
                self.emit_string_interp(parts);
            }
            AmirRvalue::ToStr { value, src_ty } => {
                self.emit_to_str(value, *src_ty);
            }
            AmirRvalue::BlackBox { value, value_ty } => {
                self.emit_black_box_value(value, *value_ty);
            }
        }
    }

    /// Emit a fat-pointer assign (str, slice, genref).
    fn emit_fat_assign(&mut self, lhs: TempId, lhs_ty: TypeId, rhs: &AmirRvalue) {
        let local = self.temp_local.get(&lhs).copied().unwrap_or(0);
        match rhs {
            AmirRvalue::Use(op) => {
                // Copy from another fat pointer.
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
                    && let Some(&src) = self.temp_local.get(t)
                {
                    self.code.push(Instruction::LocalGet(src));
                    self.code.push(Instruction::LocalSet(local));
                    self.code.push(Instruction::LocalGet(src + 1));
                    self.code.push(Instruction::LocalSet(local + 1));
                    return;
                }
                // A constant (e.g. a `str` literal) materializes a `(ptr, len)`
                // pair on the stack; store both words into the local slots.
                if matches!(op, AmirOperand::Constant(_)) {
                    self.emit_operand(op, lhs_ty);
                    self.code.push(Instruction::LocalSet(local + 1));
                    self.code.push(Instruction::LocalSet(local));
                    return;
                }
                self.emit_zero_fat(local);
            }
            AmirRvalue::SliceView { data, len, .. } => {
                self.emit_operand(data, lhs_ty);
                self.code.push(Instruction::LocalSet(local));
                self.emit_operand(len, lhs_ty);
                self.code.push(Instruction::LocalSet(local + 1));
            }
            AmirRvalue::StrBytes { source } => {
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = source
                    && let Some(&src) = self.temp_local.get(t)
                {
                    self.code.push(Instruction::LocalGet(src));
                    self.code.push(Instruction::LocalSet(local));
                    self.code.push(Instruction::LocalGet(src + 1));
                    self.code.push(Instruction::LocalSet(local + 1));
                } else {
                    self.emit_operand(source, lhs_ty);
                    self.code.push(Instruction::LocalSet(local + 1));
                    self.code.push(Instruction::LocalSet(local));
                }
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let elem_ty = match self.interner.resolve(lhs_ty) {
                    ArType::Slice(inner) | ArType::MutSlice(inner) => inner,
                    ArType::Ref(inner) | ArType::RefMut(inner) => {
                        match self.interner.resolve(inner) {
                            ArType::Slice(elem) | ArType::MutSlice(elem) => elem,
                            _ => lhs_ty,
                        }
                    }
                    _ => lhs_ty,
                };
                let elem_size = self.layout_of(&self.interner.resolve(elem_ty)).size.max(1) as i32;
                let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));

                let (slice_ptr_local, slice_len_local) = match slice {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                        let base = self.temp_local.get(t).copied().unwrap_or(0);
                        (base, base + 1)
                    }
                    _ => {
                        self.emit_operand(slice, lhs_ty);
                        self.code.push(Instruction::LocalSet(self.scratch_c));
                        self.code.push(Instruction::LocalSet(self.scratch_b));
                        (self.scratch_b, self.scratch_c)
                    }
                };

                // Evaluate start into scratch
                self.emit_operand(start, int_ty);
                self.code.push(Instruction::LocalSet(self.scratch));

                // Evaluate len into scratch_d
                self.emit_operand(len, int_ty);
                self.code.push(Instruction::LocalSet(self.scratch_d));

                // Bounds check: (start + len) > slice_len ? trap
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalGet(self.scratch_d));
                self.code.push(Instruction::I32Add);
                self.code.push(Instruction::LocalGet(slice_len_local));
                self.code.push(Instruction::I32GtU);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

                // new_ptr = slice_ptr + start * elem_size
                self.code.push(Instruction::LocalGet(slice_ptr_local));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(elem_size));
                self.code.push(Instruction::I32Mul);
                self.code.push(Instruction::I32Add);
                self.code.push(Instruction::LocalSet(local));

                // new_len = len
                self.code.push(Instruction::LocalGet(self.scratch_d));
                self.code.push(Instruction::LocalSet(local + 1));
            }
            AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                self.emit_load(place, lhs_ty);
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
            AmirRvalue::Unary {
                op: arandu_middle::ops::UnaryOp::Deref,
                operand,
            } if self
                .interner
                .slice_abi_element(self.operand_arity_ty(operand))
                .is_some()
                || self.is_str_reference(self.operand_arity_ty(operand)) =>
            {
                let operand_ty = self.operand_arity_ty(operand);
                if self.is_str_reference(operand_ty) {
                    // `ref str` is a thin pointer to a `(data, len)` descriptor,
                    // unlike a reference to a slice, whose ABI is already fat.
                    let str_ty = self.interner.intern(ArType::Primitive(Primitive::Str));
                    self.emit_operand(operand, operand_ty);
                    self.emit_load_value_at(str_ty, 0);
                } else {
                    self.emit_operand(operand, lhs_ty);
                }
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
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
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(local + 1));
            }
            AmirRvalue::GenGet { gen_ref, .. } => {
                let (ptr_local, gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.emit_validate_gen_ref(ptr_local, gen_local);

                let memarg = |o: u64| wasm_encoder::MemArg {
                    offset: o,
                    align: 2,
                    memory_index: 0,
                };
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Load(memarg(0)));
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Load(memarg(4)));
                self.code.push(Instruction::LocalSet(local + 1));
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
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(gen_local));
                self.code.push(Instruction::LocalSet(local + 1));
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
                self.code.push(Instruction::LocalSet(local));
                self.emit_alloc_gen_tag(local);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::Else);
                self.emit_validate_gen_ref(ptr_local, gen_local);
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(gen_local));
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::End);

                if self.is_owned_aggregate(*payload_ty) {
                    self.emit_operand(value, *payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_c));
                    self.code.push(Instruction::LocalGet(local));
                    self.code.push(Instruction::LocalGet(self.scratch_c));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                } else {
                    self.code.push(Instruction::LocalGet(local));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 0);
                }
            }
            AmirRvalue::GenRemove { gen_ref, .. } => {
                let (ptr_local, gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.emit_validate_gen_ref(ptr_local, gen_local);

                let memarg = |o: u64| wasm_encoder::MemArg {
                    offset: o,
                    align: 2,
                    memory_index: 0,
                };
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Load(memarg(0)));
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::I32Load(memarg(4)));
                self.code.push(Instruction::LocalSet(local + 1));

                self.emit_invalidate_gen_cell(self.scratch_b);
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::Call(self.free_func_idx));
            }
            AmirRvalue::StringInterp { parts } => {
                self.emit_string_interp(parts);
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
            AmirRvalue::ToStr { value, src_ty } => {
                self.emit_to_str(value, *src_ty);
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
            AmirRvalue::BlackBox { value, value_ty } => {
                self.emit_black_box_value(value, *value_ty);
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
            AmirRvalue::EnumPayload { .. }
            | AmirRvalue::FieldAccess { .. }
            | AmirRvalue::IndexAccess { .. } => {
                self.emit_rvalue(rhs, lhs_ty);
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
            _ => self.emit_zero_fat(local),
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
    fn emit_alloc_gen_tag(&mut self, cell_ptr_local: u32) {
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
    fn emit_validate_gen_ref(&mut self, ptr_local: u32, gen_local: u32) {
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
    fn emit_invalidate_gen_cell(&mut self, ptr_local: u32) {
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

    /// Launder `value` through the exported linear-memory scratch slot at
    /// [`crate::memory::BLACK_BOX_SCRATCH_ADDR`] and read it back via an
    /// optimizer-opaque zero offset at `BLACK_BOX_SCRATCH_ADDR + 8`.
    pub(super) fn emit_black_box_value(&mut self, value: &AmirOperand, value_ty: TypeId) {
        let vector = matches!(self.interner.resolve(value_ty), ArType::Vector(_));
        let base_offset = if vector {
            crate::memory::VECTOR_BLACK_BOX_SCRATCH_ADDR
        } else {
            crate::memory::BLACK_BOX_SCRATCH_ADDR
        };
        let opaque_zero_memarg = wasm_encoder::MemArg {
            offset: base_offset + if vector { 16 } else { 8 },
            align: 2,
            memory_index: 0,
        };
        let shape = types::shape(value_ty, self.interner, self.layout_engine.data_layout);
        match shape {
            Shape::Empty => {}
            Shape::Scalar => {
                let vt = types::scalar_valtype_for(
                    value_ty,
                    self.interner,
                    self.layout_engine.data_layout,
                )
                .unwrap_or(ValType::I32);
                self.code.push(Instruction::I32Const(0));
                self.emit_operand(value, value_ty);
                match vt {
                    ValType::V128 => {
                        let m = wasm_encoder::MemArg {
                            offset: base_offset,
                            align: 4,
                            memory_index: 0,
                        };
                        self.code.push(Instruction::V128Store(m));
                        self.code.push(Instruction::I32Const(0));
                        self.code.push(Instruction::I32Load(opaque_zero_memarg));
                        self.code.push(Instruction::V128Load(m));
                    }
                    ValType::I64 => {
                        let m = wasm_encoder::MemArg {
                            offset: base_offset,
                            align: 3,
                            memory_index: 0,
                        };
                        self.code.push(Instruction::I64Store(m));
                        self.code.push(Instruction::I32Const(0));
                        self.code.push(Instruction::I32Load(opaque_zero_memarg));
                        self.code.push(Instruction::I64Load(m));
                    }
                    ValType::F32 => {
                        let m = wasm_encoder::MemArg {
                            offset: base_offset,
                            align: 2,
                            memory_index: 0,
                        };
                        self.code.push(Instruction::F32Store(m));
                        self.code.push(Instruction::I32Const(0));
                        self.code.push(Instruction::I32Load(opaque_zero_memarg));
                        self.code.push(Instruction::F32Load(m));
                    }
                    ValType::F64 => {
                        let m = wasm_encoder::MemArg {
                            offset: base_offset,
                            align: 3,
                            memory_index: 0,
                        };
                        self.code.push(Instruction::F64Store(m));
                        self.code.push(Instruction::I32Const(0));
                        self.code.push(Instruction::I32Load(opaque_zero_memarg));
                        self.code.push(Instruction::F64Load(m));
                    }
                    _ => {
                        let m = wasm_encoder::MemArg {
                            offset: base_offset,
                            align: 2,
                            memory_index: 0,
                        };
                        self.code.push(Instruction::I32Store(m));
                        self.code.push(Instruction::I32Const(0));
                        self.code.push(Instruction::I32Load(opaque_zero_memarg));
                        self.code.push(Instruction::I32Load(m));
                    }
                }
            }
            Shape::Fat => {
                let m0 = wasm_encoder::MemArg {
                    offset: base_offset,
                    align: 2,
                    memory_index: 0,
                };
                let m1 = wasm_encoder::MemArg {
                    offset: base_offset + 4,
                    align: 2,
                    memory_index: 0,
                };
                self.emit_operand(value, value_ty);
                self.code.extend([
                    Instruction::LocalSet(self.scratch_c),
                    Instruction::LocalSet(self.scratch_b),
                    Instruction::I32Const(0),
                    Instruction::LocalGet(self.scratch_b),
                    Instruction::I32Store(m0),
                    Instruction::I32Const(0),
                    Instruction::LocalGet(self.scratch_c),
                    Instruction::I32Store(m1),
                    Instruction::I32Const(0),
                    Instruction::I32Load(opaque_zero_memarg),
                    Instruction::LocalTee(self.scratch),
                    Instruction::I32Load(m0),
                    Instruction::LocalGet(self.scratch),
                    Instruction::I32Load(m1),
                ]);
            }
        }
    }

    fn is_str_reference(&self, ty: TypeId) -> bool {
        let pointee = match self.interner.resolve(ty) {
            ArType::Ref(inner) | ArType::RefMut(inner) => inner,
            _ => return false,
        };
        matches!(
            self.interner.resolve(pointee),
            ArType::Primitive(Primitive::Str)
        )
    }
}
