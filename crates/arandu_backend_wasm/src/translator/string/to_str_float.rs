use arandu_middle::amir::value::AmirOperand;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::Instruction;

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    /// The runtime's ToStr text of a float (`crate::float_text`), written by
    /// the module's float text helper into a fresh cell. An `f32` is
    /// formatted as the `f64` it widens to, as the runtime does.
    pub(super) fn emit_to_str_float(&mut self, value: &AmirOperand, src_ty: TypeId) {
        self.alloc_cell(crate::float_text::MAX_TEXT);
        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::LocalSet(self.scratch_b));

        self.emit_operand(value, src_ty);
        if matches!(
            self.interner.resolve(src_ty),
            ArType::Primitive(Primitive::F32)
        ) {
            self.code.push(Instruction::F64PromoteF32);
        }
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::Call(
            self.float_text_func_idx + crate::float_text::ENTRY,
        ));
        self.code.push(Instruction::LocalSet(self.scratch_d));

        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_d));
    }
}
