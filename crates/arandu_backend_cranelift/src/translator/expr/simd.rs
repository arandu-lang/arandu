//! Native fixed-width SIMD lowering; never reinterpret a scalar struct as a vector.
use super::*;
use arandu_semantics::amir::{VectorCompareOp, VectorOp};
use arandu_semantics::types::{ArType, VectorType};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{MemFlagsData, types::I8X16};

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_vector(
        &mut self,
        rvalue: &AmirRvalue,
        expected: Option<Type>,
    ) -> Value {
        match rvalue {
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => {
                let mask = self.translate_operand(mask, Some(I8X16));
                let yes = self.translate_operand(if_true, expected);
                let ty = self.builder.func.dfg.value_type(yes);
                let no = self.translate_operand(if_false, Some(ty));
                let flags =
                    MemFlagsData::new().with_endianness(cranelift_codegen::ir::Endianness::Little);
                let yes = self.builder.ins().bitcast(I8X16, flags, yes);
                let no = self.builder.ins().bitcast(I8X16, flags, no);
                let yes = self.builder.ins().band(mask, yes);
                let no = self.builder.ins().band_not(no, mask);
                let bits = self.builder.ins().bor(yes, no);
                self.builder.ins().bitcast(ty, flags, bits)
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                let shape = self.get_operand_ar_type(left);
                let unsigned = shape == ArType::Vector(VectorType::U8x16);
                let left = self.translate_operand(left, None);
                let ty = self.builder.func.dfg.value_type(left);
                let right = self.translate_operand(right, Some(ty));
                let bits = if ty.lane_type().is_float() {
                    let cc = match op {
                        VectorCompareOp::Eq => FloatCC::Equal,
                        VectorCompareOp::Ne => FloatCC::NotEqual,
                        VectorCompareOp::Lt => FloatCC::LessThan,
                        VectorCompareOp::Le => FloatCC::LessThanOrEqual,
                        VectorCompareOp::Gt => FloatCC::GreaterThan,
                        VectorCompareOp::Ge => FloatCC::GreaterThanOrEqual,
                    };
                    self.builder.ins().fcmp(cc, left, right)
                } else {
                    let cc = match (op, unsigned) {
                        (VectorCompareOp::Eq, _) => IntCC::Equal,
                        (VectorCompareOp::Ne, _) => IntCC::NotEqual,
                        (VectorCompareOp::Lt, false) => IntCC::SignedLessThan,
                        (VectorCompareOp::Le, false) => IntCC::SignedLessThanOrEqual,
                        (VectorCompareOp::Gt, false) => IntCC::SignedGreaterThan,
                        (VectorCompareOp::Ge, false) => IntCC::SignedGreaterThanOrEqual,
                        (VectorCompareOp::Lt, true) => IntCC::UnsignedLessThan,
                        (VectorCompareOp::Le, true) => IntCC::UnsignedLessThanOrEqual,
                        (VectorCompareOp::Gt, true) => IntCC::UnsignedGreaterThan,
                        (VectorCompareOp::Ge, true) => IntCC::UnsignedGreaterThanOrEqual,
                    };
                    self.builder.ins().icmp(cc, left, right)
                };
                let flags =
                    MemFlagsData::new().with_endianness(cranelift_codegen::ir::Endianness::Little);
                self.builder.ins().bitcast(I8X16, flags, bits)
            }
            AmirRvalue::VectorSplat { value } => {
                let Some(ty) = expected.filter(|ty| ty.is_vector()) else {
                    self.record_ice("vector splat requires a vector result", self.func_span());
                    return self.poison_i32();
                };
                let value = self.translate_operand(value, Some(ty.lane_type()));
                self.builder.ins().splat(ty, value)
            }
            AmirRvalue::VectorOp { op, left, right } => {
                let left = self.translate_operand(left, expected);
                let ty = self.builder.func.dfg.value_type(left);
                let right = self.translate_operand(right, Some(ty));
                match op {
                    VectorOp::Add if ty.lane_type().is_float() => {
                        self.builder.ins().fadd(left, right)
                    }
                    VectorOp::Sub if ty.lane_type().is_float() => {
                        self.builder.ins().fsub(left, right)
                    }
                    VectorOp::Mul if ty.lane_type().is_float() => {
                        self.builder.ins().fmul(left, right)
                    }
                    VectorOp::Div => self.builder.ins().fdiv(left, right),
                    VectorOp::Add => self.builder.ins().iadd(left, right),
                    VectorOp::Sub => self.builder.ins().isub(left, right),
                    VectorOp::Mul => self.builder.ins().imul(left, right),
                    VectorOp::BitAnd => self.builder.ins().band(left, right),
                    VectorOp::BitOr => self.builder.ins().bor(left, right),
                    VectorOp::BitXor => self.builder.ins().bxor(left, right),
                }
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                let vector = self.translate_operand(vector, None);
                self.builder.ins().extractlane(vector, *lane)
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                let vector = self.translate_operand(vector, expected);
                let ty = self.builder.func.dfg.value_type(vector).lane_type();
                let value = self.translate_operand(value, Some(ty));
                self.builder.ins().insertlane(vector, value, *lane)
            }
            AmirRvalue::VectorShuffle { left, right, mask } => {
                let left = self.translate_operand(left, expected);
                let ty = self.builder.func.dfg.value_type(left);
                let right = self.translate_operand(right, Some(ty));
                let flags =
                    MemFlagsData::new().with_endianness(cranelift_codegen::ir::Endianness::Little);
                let left = self.builder.ins().bitcast(I8X16, flags, left);
                let right = self.builder.ins().bitcast(I8X16, flags, right);
                let mask = self.builder.func.dfg.immediates.push(mask.to_vec().into());
                let shuffled = self.builder.ins().shuffle(left, right, mask);
                self.builder.ins().bitcast(ty, flags, shuffled)
            }
            _ => {
                self.record_ice("expected a vector operation", self.func_span());
                self.poison_i32()
            }
        }
    }
}
