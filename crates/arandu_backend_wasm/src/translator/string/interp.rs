use arandu_middle::amir::value::{AmirConstant, AmirOperand};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};
use wasm_encoder::Instruction;

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    /// Concatenate `str` fat-pointer parts into a single string buffer.
    /// Pushes `(ptr, len)` onto the wasm stack.
    pub(in crate::translator) fn emit_string_interp(&mut self, parts: &[AmirOperand]) {
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
}
