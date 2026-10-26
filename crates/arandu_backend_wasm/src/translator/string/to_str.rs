use arandu_middle::amir::value::AmirOperand;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_to_str_str(&mut self, value: &AmirOperand, src_ty: TypeId) {
        self.emit_operand(value, src_ty);
    }

    pub(super) fn emit_to_str_bool(&mut self, value: &AmirOperand) {
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
    }

    pub(super) fn emit_to_str_char(&mut self, value: &AmirOperand) {
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
    }
}
