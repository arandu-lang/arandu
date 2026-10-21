use arandu_middle::amir::value::AmirOperand;
use arandu_middle::types::TypeId;
use wasm_encoder::{BlockType, Instruction};

use super::super::FuncTranslator;
use crate::types;

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_to_str_int(&mut self, value: &AmirOperand, src_ty: TypeId) {
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
