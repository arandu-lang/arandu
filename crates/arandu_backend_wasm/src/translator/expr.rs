//! Operand loading and rvalue emission into the wasm instruction buffer.

use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::{AmirOperand, AmirRvalue};
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
            AmirRvalue::Discriminant { value } => {
                // Load enum tag (i32 at offset 0 of the enum cell).
                self.emit_operand(value, result_ty);
                self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
            }
            AmirRvalue::Len(op) => {
                // For fat pointers: the len is the second slot (local+1).
                if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = op
                    && let Some(&base) = self.temp_local.get(temp)
                {
                    self.code.push(Instruction::LocalGet(base + 1));
                    return;
                }
                self.code.push(Instruction::I32Const(0));
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
            AmirRvalue::EnumPayload {
                value,
                variant: _,
                index: _,
            } => {
                // Load the payload field from an enum cell at its layout offset.
                let operand_ty = self.operand_arity_ty(value);
                let enum_ty = self.strip_ref(operand_ty).unwrap_or(operand_ty);
                let layout = self.layout_of_id(enum_ty);
                let payload_offset = layout.field_offsets.get(1).copied().unwrap_or(4);
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
                    ArType::Slice(inner) => inner,
                    ArType::Ref(inner) | ArType::RefMut(inner) => {
                        match self.interner.resolve(inner) {
                            ArType::Slice(elem) => elem,
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
                if self.is_owned_aggregate(*payload_ty) {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_c));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_c));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::I32Const(1));
                } else {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 0);
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::I32Const(1));
                }
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => {
                let (ptr_local, _gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

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
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

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
                self.code.push(Instruction::If(BlockType::Empty));
                self.alloc_cell(size);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                self.code.push(Instruction::I32Const(1));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::Else);
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
                let (ptr_local, _gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

                if self.is_owned_aggregate(*payload_ty) {
                    self.code.push(Instruction::LocalGet(ptr_local));
                } else {
                    let vt = types::scalar_valtype_for(
                        *payload_ty,
                        self.interner,
                        self.layout_engine.data_layout,
                    );
                    match vt {
                        Some(ValType::I64) => {
                            self.code.push(Instruction::LocalGet(ptr_local));
                            self.emit_load_value_at(*payload_ty, 0);
                            self.code.push(Instruction::LocalSet(self.scratch_i64));
                            self.code.push(Instruction::LocalGet(ptr_local));
                            self.code.push(Instruction::Call(self.free_func_idx));
                            self.code.push(Instruction::LocalGet(self.scratch_i64));
                        }
                        Some(ValType::I32) => {
                            self.code.push(Instruction::LocalGet(ptr_local));
                            self.emit_load_value_at(*payload_ty, 0);
                            self.code.push(Instruction::LocalSet(self.scratch_c));
                            self.code.push(Instruction::LocalGet(ptr_local));
                            self.code.push(Instruction::Call(self.free_func_idx));
                            self.code.push(Instruction::LocalGet(self.scratch_c));
                        }
                        _ => {
                            self.code.push(Instruction::LocalGet(ptr_local));
                            self.code.push(Instruction::Call(self.free_func_idx));
                            self.code.push(Instruction::LocalGet(ptr_local));
                            self.emit_load_value_at(*payload_ty, 0);
                        }
                    }
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
                self.emit_operand(value, *value_ty);
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
                    ArType::Slice(inner) => inner,
                    ArType::Ref(inner) | ArType::RefMut(inner) => {
                        match self.interner.resolve(inner) {
                            ArType::Slice(elem) => elem,
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
            AmirRvalue::Load(place) => {
                if place.projections.is_empty() {
                    let src = self.local_slot(place.local).unwrap_or(0);
                    self.code.push(Instruction::LocalGet(src));
                    self.code.push(Instruction::LocalSet(local));
                    self.code.push(Instruction::LocalGet(src + 1));
                    self.code.push(Instruction::LocalSet(local + 1));
                    return;
                }
                self.emit_zero_fat(local);
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                if place.projections.is_empty() {
                    let src = self.local_slot(place.local).unwrap_or(0);
                    self.code.push(Instruction::LocalGet(src));
                    self.code.push(Instruction::LocalSet(local));
                    self.code.push(Instruction::LocalGet(src + 1));
                    self.code.push(Instruction::LocalSet(local + 1));
                } else {
                    let place_ty = self.emit_place_address(place);
                    self.emit_load_value_at(place_ty, 0);
                    self.code.push(Instruction::LocalSet(local + 1));
                    self.code.push(Instruction::LocalSet(local));
                }
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
                if self.is_owned_aggregate(*payload_ty) {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_c));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_c));
                    self.code.push(Instruction::I32Const(size));
                    self.code.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalSet(local));
                } else {
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.emit_operand(value, *payload_ty);
                    self.emit_store_value_at(*payload_ty, 0);
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalSet(local));
                }
                self.code.push(Instruction::I32Const(1));
                self.code.push(Instruction::LocalSet(local + 1));
            }
            AmirRvalue::GenGet { gen_ref, .. } => {
                let (ptr_local, _gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

                let memarg = |o: u64| wasm_encoder::MemArg {
                    offset: o,
                    align: 2,
                    memory_index: 0,
                };
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Load(memarg(0)));
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(ptr_local));
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
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

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
                self.code.push(Instruction::If(BlockType::Empty));
                self.alloc_cell(size);
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::I32Const(1));
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::Else);
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
                let (ptr_local, _gen_local) = self.resolve_gen_ref_slots(gen_ref);
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Eqz);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::Unreachable);
                self.code.push(Instruction::End);

                let memarg = |o: u64| wasm_encoder::MemArg {
                    offset: o,
                    align: 2,
                    memory_index: 0,
                };
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Load(memarg(0)));
                self.code.push(Instruction::LocalSet(local));
                self.code.push(Instruction::LocalGet(ptr_local));
                self.code.push(Instruction::I32Load(memarg(4)));
                self.code.push(Instruction::LocalSet(local + 1));

                self.code.push(Instruction::LocalGet(ptr_local));
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
            AmirRvalue::BlackBox { value, .. } => {
                self.emit_fat_assign(lhs, lhs_ty, &AmirRvalue::Use(*value));
            }
            _ => self.emit_zero_fat(local),
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
