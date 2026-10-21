use arandu_middle::amir::value::AmirOperand;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_to_str_float(&mut self, value: &AmirOperand, src_ty: TypeId) {
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
    }
}
