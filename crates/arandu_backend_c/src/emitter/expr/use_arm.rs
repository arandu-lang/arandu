use super::super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand};
use arandu_middle::types::{ArType, Primitive};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_use_rvalue(
        &mut self,
        op: &AmirOperand,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        let op_str = self.format_operand(op, func);
        let source_ty = match op {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => self.temp_ty(func, *temp),
            _ => ArType::Error,
        };
        let source_is_pointer = matches!(
            &source_ty,
            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
        ) && !source_ty.is_borrowed_slice_abi(self.interner);
        let dest_is_pointer = matches!(
            expected_ar_type,
            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
        ) && !expected_ar_type.is_borrowed_slice_abi(self.interner);
        if self.is_str_ref_to_str(&source_ty, expected_ar_type) {
            let _ = write!(&mut self.output, "(*({op_str}))");
        } else if dest_is_pointer {
            if source_is_pointer
                || matches!(source_ty, ArType::Func(_, _))
                || matches!(op, AmirOperand::FunctionRef(_))
            {
                let _ = write!(&mut self.output, "({expected_c_type})({op_str})");
            } else if matches!(source_ty, ArType::Primitive(_) | ArType::IntLiteral) {
                let _ = write!(&mut self.output, "({expected_c_type})(uintptr_t)({op_str})");
            } else {
                let _ = write!(&mut self.output, "*({expected_c_type}*)&({op_str})");
            }
        } else if source_is_pointer
            && matches!(expected_ar_type, ArType::Primitive(_) | ArType::IntLiteral)
        {
            let _ = write!(&mut self.output, "({expected_c_type})(uintptr_t)({op_str})");
        } else {
            let _ = write!(&mut self.output, "{op_str}");
        }
    }

    pub(super) fn emit_black_box(
        &mut self,
        value: &AmirOperand,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        let operand = self.format_operand(value, func);
        match expected_ar_type {
            ArType::Primitive(Primitive::Str) => {
                let _ = write!(
                    &mut self.output,
                    "({{ volatile {expected_c_type} opaque = ({operand}); opaque; }})"
                );
            }
            ArType::Primitive(Primitive::F32) => {
                let _ = write!(
                    &mut self.output,
                    "ar_bench_black_box_f32((float)({operand}))"
                );
            }
            ArType::Primitive(p) if p.is_float() => {
                let _ = write!(
                    &mut self.output,
                    "ar_bench_black_box_f64((double)({operand}))"
                );
            }
            ArType::FloatLiteral => {
                let _ = write!(
                    &mut self.output,
                    "ar_bench_black_box_f64((double)({operand}))"
                );
            }
            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) => {
                let _ = write!(
                    &mut self.output,
                    "({expected_c_type})ar_bench_black_box_ptr((void*)({operand}))"
                );
            }
            ArType::Primitive(_) | ArType::IntLiteral => {
                let _ = write!(
                    &mut self.output,
                    "({expected_c_type})ar_bench_black_box_i64((int64_t)({operand}))"
                );
            }
            _ => {
                let _ = write!(
                    &mut self.output,
                    "({{ volatile {expected_c_type} opaque = ({operand}); opaque; }})"
                );
            }
        }
    }
}
