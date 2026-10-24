use super::super::CEmitter;
use arandu_middle::amir::{AmirConstant, AmirFunc, AmirOperand};
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
        // Box a `T` into a `T?` handle, and unbox a non-nil handle back to `T`.
        if let ArType::Nullable(inner) = expected_ar_type
            && !matches!(source_ty, ArType::Nullable(_))
            && !matches!(op, AmirOperand::Constant(AmirConstant::Nil))
        {
            let inner_ty = self.interner.resolve(*inner);
            if self.is_boxed_nullable(&inner_ty) {
                let inner_c = self.format_type(&inner_ty);
                let _ = write!(
                    &mut self.output,
                    "({{ {inner_c}* ar_box = ({inner_c}*)malloc(sizeof({inner_c})); if (!ar_box) abort(); *ar_box = ({inner_c})({op_str}); ar_box; }})"
                );
                return;
            }
        }
        if let ArType::Nullable(inner) = &source_ty
            && !matches!(expected_ar_type, ArType::Nullable(_))
        {
            let inner_ty = self.interner.resolve(*inner);
            if self.is_boxed_nullable(&inner_ty) {
                let _ = write!(&mut self.output, "(*({op_str}))");
                return;
            }
        }
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
