use super::super::FuncTranslator;
use arandu_middle::amir::value::AmirRvalue;
use arandu_middle::types::{ArType, TypeId};
use wasm_encoder::Instruction;

impl<'a> FuncTranslator<'a> {
    pub(super) fn emit_vector_rvalue(&mut self, rvalue: &AmirRvalue, result_ty: TypeId) {
        match rvalue {
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
            _ => unreachable!(),
        }
    }
}
