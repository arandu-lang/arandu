use super::super::FuncTranslator;
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::{AmirOperand, AmirRvalue};
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

impl<'a> FuncTranslator<'a> {
    /// Emit a fat-pointer assign (str, slice, genref).
    pub(super) fn emit_fat_assign(&mut self, lhs: TempId, lhs_ty: TypeId, rhs: &AmirRvalue) {
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
