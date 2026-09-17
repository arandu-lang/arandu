use arandu_middle::amir::value::{AmirConstant, AmirOperand};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

use super::FuncTranslator;
use crate::types;

impl<'a> FuncTranslator<'a> {
    /// Concatenate `str` fat-pointer parts into a single string buffer.
    /// Pushes `(ptr, len)` onto the wasm stack.
    pub(super) fn emit_string_interp(&mut self, parts: &[AmirOperand]) {
        if parts.is_empty() {
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Const(0));
            return;
        }

        // 1. Calculate sum of lengths: push 0, then add each part's length.
        self.code.push(Instruction::I32Const(0));
        for part in parts {
            match part {
                AmirOperand::Constant(AmirConstant::Pool(lit_id)) => {
                    if let AmirLiteralEntry::Str(lexeme) = self.literal_pool.get(*lit_id) {
                        self.code.push(Instruction::I32Const(lexeme.len() as i32));
                    } else {
                        self.code.push(Instruction::I32Const(0));
                    }
                }
                AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                    let base = self.temp_local.get(t).copied().unwrap_or(0);
                    self.code.push(Instruction::LocalGet(base + 1));
                }
                _ => {
                    self.code.push(Instruction::I32Const(0));
                }
            }
            self.code.push(Instruction::I32Add);
        }
        self.code.push(Instruction::LocalSet(self.scratch_d));

        // 2. Allocate buffer: total + 1 bytes (for trailing NUL)
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Const(1));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::Call(self.alloc_func_idx));
        self.code.push(Instruction::LocalSet(self.scratch_b));

        // 3. Initialize current offset cursor = 0 in scratch_c
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::LocalSet(self.scratch_c));

        // 4. Copy each part using Instruction::MemoryCopy
        for part in parts {
            let (len_is_const, const_len) = match part {
                AmirOperand::Constant(AmirConstant::Pool(lit_id)) => {
                    if let AmirLiteralEntry::Str(lexeme) = self.literal_pool.get(*lit_id) {
                        (true, lexeme.len() as i32)
                    } else {
                        (true, 0)
                    }
                }
                _ => (false, 0),
            };

            // dst = scratch_b + scratch_c
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Add);

            // src pointer
            match part {
                AmirOperand::Constant(AmirConstant::Pool(lit_id)) => {
                    if let Some(&ro_off) = self.rodata_offsets.get(lit_id) {
                        self.code.push(Instruction::I32Const(ro_off as i32));
                    } else {
                        let str_ty = self.interner.intern(ArType::Primitive(Primitive::Str));
                        self.emit_operand(part, str_ty);
                        self.code.push(Instruction::Drop);
                    }
                }
                AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                    let base = self.temp_local.get(t).copied().unwrap_or(0);
                    self.code.push(Instruction::LocalGet(base));
                }
                _ => {
                    self.code.push(Instruction::I32Const(0));
                }
            }

            // size
            if len_is_const {
                self.code.push(Instruction::I32Const(const_len));
            } else if let AmirOperand::Copy(t) | AmirOperand::Move(t) = part {
                let base = self.temp_local.get(t).copied().unwrap_or(0);
                self.code.push(Instruction::LocalGet(base + 1));
            } else {
                self.code.push(Instruction::I32Const(0));
            }

            self.code.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });

            // Advance offset cursor: scratch_c += len
            self.code.push(Instruction::LocalGet(self.scratch_c));
            if len_is_const {
                self.code.push(Instruction::I32Const(const_len));
            } else if let AmirOperand::Copy(t) | AmirOperand::Move(t) = part {
                let base = self.temp_local.get(t).copied().unwrap_or(0);
                self.code.push(Instruction::LocalGet(base + 1));
            } else {
                self.code.push(Instruction::I32Const(0));
            }
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalSet(self.scratch_c));
        }

        // 5. Null-terminate: store 0 at scratch_b + scratch_d
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));

        // 6. Push result fat pointer (ptr, len) onto stack
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_d));
    }

    /// Format a primitive value as a `str` fat pointer (ptr, len).
    pub(super) fn emit_to_str(&mut self, value: &AmirOperand, src_ty: TypeId) {
        let ar_ty = self.interner.resolve(src_ty);
        if matches!(ar_ty, ArType::Primitive(Primitive::Str)) {
            self.emit_operand(value, src_ty);
            return;
        }

        if matches!(ar_ty, ArType::Primitive(Primitive::Bool)) {
            self.alloc_cell(8);
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::LocalSet(self.scratch_b));

            let bool_ty = self.interner.intern(ArType::Primitive(Primitive::Bool));
            self.emit_operand(value, bool_ty);
            self.code.push(Instruction::If(BlockType::Empty));
            // "true\0"
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0x6575_7274));
            self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 4,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::I32Const(4));
            self.code.push(Instruction::LocalSet(self.scratch_c));
            self.code.push(Instruction::Else);
            // "false\0"
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0x736c_6166));
            self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0x65));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 4,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 5,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::I32Const(5));
            self.code.push(Instruction::LocalSet(self.scratch_c));
            self.code.push(Instruction::End);

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            return;
        }

        if matches!(ar_ty, ArType::Primitive(Primitive::Char)) {
            self.alloc_cell(8);
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::LocalSet(self.scratch_b));

            let char_ty = self.interner.intern(ArType::Primitive(Primitive::Char));
            self.emit_operand(value, char_ty);
            self.code.push(Instruction::LocalSet(self.scratch_c));

            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(0x80));
            self.code.push(Instruction::I32LtU);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 1,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::LocalSet(self.scratch_d));
            self.code.push(Instruction::Else);
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(0x800));
            self.code.push(Instruction::I32LtU);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(6));
            self.code.push(Instruction::I32ShrU);
            self.code.push(Instruction::I32Const(0xC0));
            self.code.push(Instruction::I32Or);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(0x3F));
            self.code.push(Instruction::I32And);
            self.code.push(Instruction::I32Const(0x80));
            self.code.push(Instruction::I32Or);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 1,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 2,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::I32Const(2));
            self.code.push(Instruction::LocalSet(self.scratch_d));
            self.code.push(Instruction::Else);
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(12));
            self.code.push(Instruction::I32ShrU);
            self.code.push(Instruction::I32Const(0xE0));
            self.code.push(Instruction::I32Or);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(6));
            self.code.push(Instruction::I32ShrU);
            self.code.push(Instruction::I32Const(0x3F));
            self.code.push(Instruction::I32And);
            self.code.push(Instruction::I32Const(0x80));
            self.code.push(Instruction::I32Or);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 1,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Const(0x3F));
            self.code.push(Instruction::I32And);
            self.code.push(Instruction::I32Const(0x80));
            self.code.push(Instruction::I32Or);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 2,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 3,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::I32Const(3));
            self.code.push(Instruction::LocalSet(self.scratch_d));
            self.code.push(Instruction::End);
            self.code.push(Instruction::End);

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            return;
        }

        if types::ar_is_float(src_ty, self.interner) {
            self.alloc_cell(32);
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::LocalSet(self.scratch_b));

            self.emit_operand(value, src_ty);
            let is_f32 = matches!(
                self.interner.resolve(src_ty),
                ArType::Primitive(Primitive::F32)
            );
            if is_f32 {
                self.code.push(Instruction::F64PromoteF32);
            }
            self.code.push(Instruction::LocalSet(self.scratch_f64));

            // NaN check: val != val
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::F64Ne);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0x004E_614E));
            self.code
                .push(Instruction::I32Store(crate::memory::noffset_memarg()));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(3));
            self.code.push(Instruction::Return);
            self.code.push(Instruction::End);

            // +inf check
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::F64Const(f64::INFINITY.into()));
            self.code.push(Instruction::F64Eq);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0x0066_6E69));
            self.code
                .push(Instruction::I32Store(crate::memory::noffset_memarg()));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(3));
            self.code.push(Instruction::Return);
            self.code.push(Instruction::End);

            // -inf check
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code
                .push(Instruction::F64Const(f64::NEG_INFINITY.into()));
            self.code.push(Instruction::F64Eq);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0x666E_692D));
            self.code
                .push(Instruction::I32Store(crate::memory::noffset_memarg()));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 4,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(4));
            self.code.push(Instruction::Return);
            self.code.push(Instruction::End);

            // Sign check: val < 0.0 or 1.0 / val < 0.0 (handles -0.0)
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::F64Const(0.0.into()));
            self.code.push(Instruction::F64Lt);
            self.code.push(Instruction::F64Const(1.0.into()));
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::F64Div);
            self.code.push(Instruction::F64Const(0.0.into()));
            self.code.push(Instruction::F64Lt);
            self.code.push(Instruction::I32Or);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::LocalSet(self.scratch_c));
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::F64Neg);
            self.code.push(Instruction::LocalSet(self.scratch_f64));
            self.code.push(Instruction::Else);
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::LocalSet(self.scratch_c));
            self.code.push(Instruction::End);

            // Decompose into integer and fractional parts
            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::I64TruncSatF64U);
            self.code.push(Instruction::LocalSet(self.scratch_i64));

            self.code.push(Instruction::LocalGet(self.scratch_f64));
            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::F64ConvertI64U);
            self.code.push(Instruction::F64Sub);
            self.code.push(Instruction::F64Const(1_000_000.0.into()));
            self.code.push(Instruction::F64Mul);
            self.code.push(Instruction::F64Const(0.5.into()));
            self.code.push(Instruction::F64Add);
            self.code.push(Instruction::I64TruncSatF64U);
            self.code.push(Instruction::LocalSet(self.scratch_i64b));

            // Overflow carry
            self.code.push(Instruction::LocalGet(self.scratch_i64b));
            self.code.push(Instruction::I64Const(1_000_000));
            self.code.push(Instruction::I64GeU);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_i64b));
            self.code.push(Instruction::I64Const(1_000_000));
            self.code.push(Instruction::I64Sub);
            self.code.push(Instruction::LocalSet(self.scratch_i64b));
            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(1));
            self.code.push(Instruction::I64Add);
            self.code.push(Instruction::LocalSet(self.scratch_i64));
            self.code.push(Instruction::End);

            // Format integer part backwards starting at offset 20
            self.code.push(Instruction::I32Const(20));
            self.code.push(Instruction::LocalSet(self.scratch_d));

            self.code.push(Instruction::Loop(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch_d));

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);

            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(10));
            self.code.push(Instruction::I64RemU);
            self.code.push(Instruction::I32WrapI64);
            self.code.push(Instruction::I32Const(48));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(10));
            self.code.push(Instruction::I64DivU);
            self.code.push(Instruction::LocalSet(self.scratch_i64));

            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(0));
            self.code.push(Instruction::I64Ne);
            self.code.push(Instruction::BrIf(0));
            self.code.push(Instruction::End);

            // Move integer digits to scratch_b + is_neg
            self.code.push(Instruction::I32Const(20));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch));

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });

            // Write '-' if is_neg
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::I32Const(45));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::End);

            // cursor = is_neg + int_len
            self.code.push(Instruction::LocalGet(self.scratch_c));
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalSet(self.scratch_d));

            // Write '.' at scratch_b[cursor]
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Const(46));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            // cursor += 1
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalSet(self.scratch_d));

            // Write 6 fractional digits into scratch_b[cursor..cursor+6]
            self.code.push(Instruction::I32Const(6));
            self.code.push(Instruction::LocalSet(self.scratch));

            self.code.push(Instruction::Loop(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch));

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Add);

            self.code.push(Instruction::LocalGet(self.scratch_i64b));
            self.code.push(Instruction::I64Const(10));
            self.code.push(Instruction::I64RemU);
            self.code.push(Instruction::I32WrapI64);
            self.code.push(Instruction::I32Const(48));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            self.code.push(Instruction::LocalGet(self.scratch_i64b));
            self.code.push(Instruction::I64Const(10));
            self.code.push(Instruction::I64DivU);
            self.code.push(Instruction::LocalSet(self.scratch_i64b));

            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Ne);
            self.code.push(Instruction::BrIf(0));
            self.code.push(Instruction::End);

            // cursor += 6
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(6));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::LocalSet(self.scratch_d));

            // Trim trailing zeros, leaving at least 1 digit after '.'
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(5));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch));

            self.code.push(Instruction::Loop(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32GtU);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            self.code.push(Instruction::I32Const(48));
            self.code.push(Instruction::I32Eq);
            self.code.push(Instruction::If(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch_d));
            self.code.push(Instruction::Br(2));
            self.code.push(Instruction::End);
            self.code.push(Instruction::End);
            self.code.push(Instruction::End);

            // Null-terminate
            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            return;
        }

        // Integer formatting (64-bit or 32-bit)
        let is_64 = types::ar_is_64bit(src_ty, self.interner, self.layout_engine.data_layout);
        let is_unsigned = types::ar_is_unsigned(src_ty, self.interner);

        self.alloc_cell(24);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));

        if is_64 {
            self.emit_operand(value, src_ty);
            self.code.push(Instruction::LocalSet(self.scratch_i64));

            if !is_unsigned {
                self.code.push(Instruction::LocalGet(self.scratch_i64));
                self.code.push(Instruction::I64Const(0));
                self.code.push(Instruction::I64LtS);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::I32Const(1));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::I64Const(0));
                self.code.push(Instruction::LocalGet(self.scratch_i64));
                self.code.push(Instruction::I64Sub);
                self.code.push(Instruction::LocalSet(self.scratch_i64));
                self.code.push(Instruction::Else);
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::End);
            } else {
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::LocalSet(self.scratch_c));
            }

            self.code.push(Instruction::I32Const(23));
            self.code.push(Instruction::LocalSet(self.scratch_d));

            self.code.push(Instruction::Loop(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch_d));

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);

            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(10));
            self.code.push(Instruction::I64RemU);
            self.code.push(Instruction::I32WrapI64);
            self.code.push(Instruction::I32Const(48));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(10));
            self.code.push(Instruction::I64DivU);
            self.code.push(Instruction::LocalSet(self.scratch_i64));

            self.code.push(Instruction::LocalGet(self.scratch_i64));
            self.code.push(Instruction::I64Const(0));
            self.code.push(Instruction::I64Ne);
            self.code.push(Instruction::BrIf(0));
            self.code.push(Instruction::End);
        } else {
            self.emit_operand(value, src_ty);
            self.code.push(Instruction::LocalSet(self.scratch));

            if !is_unsigned {
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::I32LtS);
                self.code.push(Instruction::If(BlockType::Empty));
                self.code.push(Instruction::I32Const(1));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Sub);
                self.code.push(Instruction::LocalSet(self.scratch));
                self.code.push(Instruction::Else);
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::End);
            } else {
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::LocalSet(self.scratch_c));
            }

            self.code.push(Instruction::I32Const(23));
            self.code.push(Instruction::LocalSet(self.scratch_d));

            self.code.push(Instruction::Loop(BlockType::Empty));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Const(1));
            self.code.push(Instruction::I32Sub);
            self.code.push(Instruction::LocalSet(self.scratch_d));

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalGet(self.scratch_d));
            self.code.push(Instruction::I32Add);

            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Const(10));
            self.code.push(Instruction::I32RemU);
            self.code.push(Instruction::I32Const(48));
            self.code.push(Instruction::I32Add);
            self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Const(10));
            self.code.push(Instruction::I32DivU);
            self.code.push(Instruction::LocalSet(self.scratch));

            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::I32Const(0));
            self.code.push(Instruction::I32Ne);
            self.code.push(Instruction::BrIf(0));
            self.code.push(Instruction::End);
        }

        self.code.push(Instruction::LocalGet(self.scratch_c));
        self.code.push(Instruction::If(BlockType::Empty));
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Const(1));
        self.code.push(Instruction::I32Sub);
        self.code.push(Instruction::LocalSet(self.scratch_d));
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::I32Const(45));
        self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        self.code.push(Instruction::End);

        self.code.push(Instruction::I32Const(23));
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Sub);
        self.code.push(Instruction::LocalSet(self.scratch_c));

        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::LocalGet(self.scratch_c));
        self.code.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_c));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));

        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_c));
    }
}
