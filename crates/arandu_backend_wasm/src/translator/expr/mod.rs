//! Operand loading and rvalue emission into the wasm instruction buffer.

use super::FuncTranslator;
use crate::types::{self, Shape};
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::{AmirOperand, AmirRvalue};
use arandu_middle::types::{ArType, TypeId};
use wasm_encoder::{Instruction, ValType};

mod aggregate;
mod fat;
mod gen_ops;
mod memory;
mod vector;

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
            AmirRvalue::VectorCompare { .. }
            | AmirRvalue::VectorSelect { .. }
            | AmirRvalue::VectorOp { .. }
            | AmirRvalue::VectorSplat { .. }
            | AmirRvalue::VectorShuffle { .. }
            | AmirRvalue::VectorExtractLane { .. }
            | AmirRvalue::VectorInsertLane { .. } => self.emit_vector_rvalue(rvalue, result_ty),
            AmirRvalue::Discriminant { .. }
            | AmirRvalue::Len(_)
            | AmirRvalue::SliceData(_)
            | AmirRvalue::SliceView { .. }
            | AmirRvalue::Borrow(_)
            | AmirRvalue::BorrowMut(_)
            | AmirRvalue::Load(_)
            | AmirRvalue::Alloc(_)
            | AmirRvalue::StrView { .. }
            | AmirRvalue::StrBytes { .. }
            | AmirRvalue::SliceSubslice { .. }
            | AmirRvalue::RelativeBorrow { .. } => self.emit_memory_rvalue(rvalue, result_ty),
            AmirRvalue::StructLiteral { .. }
            | AmirRvalue::Tuple { .. }
            | AmirRvalue::Array { .. }
            | AmirRvalue::FieldAccess { .. }
            | AmirRvalue::IndexAccess { .. }
            | AmirRvalue::EnumConstruct { .. }
            | AmirRvalue::EnumPayload { .. } => self.emit_aggregate_rvalue(rvalue, result_ty),
            AmirRvalue::GenInsert { .. }
            | AmirRvalue::GenGet { .. }
            | AmirRvalue::GenSet { .. }
            | AmirRvalue::GenUpsert { .. }
            | AmirRvalue::GenRemove { .. } => self.emit_gen_rvalue(rvalue, result_ty),
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
}
