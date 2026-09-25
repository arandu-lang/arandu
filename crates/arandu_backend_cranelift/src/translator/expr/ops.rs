//! Arithmetic, logical, bitwise, and casting operations.

use arandu_semantics::amir::AmirOperand;
use arandu_semantics::ops::UnaryOp;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_binary(
        &mut self,
        op: arandu_semantics::ops::BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
        expected_ty: Option<Type>,
    ) -> Value {
        // `str` equality uses fat pointers + memcmp (not scalar icmp).
        let left_is_str = matches!(
            self.get_operand_ar_type(left),
            ArType::Primitive(Primitive::Str)
        );
        let right_is_str = matches!(
            self.get_operand_ar_type(right),
            ArType::Primitive(Primitive::Str)
        );
        if left_is_str || right_is_str {
            match op {
                arandu_semantics::ops::BinaryOp::Equal
                | arandu_semantics::ops::BinaryOp::NotEqual => {
                    return self.translate_str_eq(left, right, op);
                }
                _ => {
                    self.record_ice("unsupported binary op on str in codegen", self.func_span());
                    return self.poison_i32();
                }
            }
        }
        let opt_ty = match op {
            arandu_semantics::ops::BinaryOp::Add
            | arandu_semantics::ops::BinaryOp::Sub
            | arandu_semantics::ops::BinaryOp::Mul
            | arandu_semantics::ops::BinaryOp::Div
            | arandu_semantics::ops::BinaryOp::Mod
            | arandu_semantics::ops::BinaryOp::BitOr
            | arandu_semantics::ops::BinaryOp::BitXor
            | arandu_semantics::ops::BinaryOp::BitAnd
            | arandu_semantics::ops::BinaryOp::ShiftLeft
            | arandu_semantics::ops::BinaryOp::ShiftRight => expected_ty,
            // Comparisons (incl. `x == nil` / `x != nil`): prefer the
            // non-constant side's ABI type so Nil is a zero of matching width.
            arandu_semantics::ops::BinaryOp::Equal
            | arandu_semantics::ops::BinaryOp::NotEqual
            | arandu_semantics::ops::BinaryOp::Lt
            | arandu_semantics::ops::BinaryOp::LtEqual
            | arandu_semantics::ops::BinaryOp::Gt
            | arandu_semantics::ops::BinaryOp::GtEqual => {
                let left_ty = self.get_operand_clif_type(left);
                let right_ty = self.get_operand_clif_type(right);
                left_ty.or(right_ty).or(expected_ty)
            }
            _ => None,
        };
        let lhs = self.translate_operand(left, opt_ty);
        let rhs = self.translate_operand(right, opt_ty);
        self.translate_binary_op(op, lhs, rhs, Some(left), Some(right))
    }

    pub(super) fn translate_unary(
        &mut self,
        op: UnaryOp,
        operand: &AmirOperand,
        expected_ty: Option<Type>,
    ) -> Value {
        if matches!(op, UnaryOp::Deref) {
            let operand_ty = self.get_operand_ar_type(operand);
            if operand_ty.is_borrowed_slice_abi(&self.type_info.type_interner) {
                // A reference to a dynamically-sized slice is already the
                // `{ data, len }` view. Dereferencing changes the static
                // permission, not the runtime representation.
                return self.translate_operand(operand, Some(self.ptr_type));
            }
            let ptr = self.translate_operand(operand, Some(self.ptr_type));
            let load_ty = expected_ty.unwrap_or(self.ptr_type);
            return self.builder.ins().load(
                load_ty,
                cranelift_codegen::ir::MemFlagsData::new(),
                ptr,
                0,
            );
        }
        // A3.6: await = block_on(poll) until Ready. State layout:
        //   +0 disc (u32, 0=Ready), +8 payload.
        if matches!(op, UnaryOp::Await) {
            return self.translate_await_block_on(operand, expected_ty);
        }
        let val = self.translate_operand(operand, expected_ty);
        self.translate_unary_op(op, val)
    }

    pub(crate) fn cast_int_width(&mut self, val: Value, target: Type) -> Value {
        self.cast_int_width_signed(val, target, false)
    }

    pub(crate) fn cast_int_width_signed(
        &mut self,
        val: Value,
        target: Type,
        is_signed: bool,
    ) -> Value {
        let src = self.builder.func.dfg.value_type(val);
        if src == target {
            return val;
        }
        if src.bits() < target.bits() {
            if is_signed {
                self.builder.ins().sextend(target, val)
            } else {
                self.builder.ins().uextend(target, val)
            }
        } else if src.bits() > target.bits() {
            self.builder.ins().ireduce(target, val)
        } else {
            val
        }
    }

    pub(super) fn translate_unary_op(&mut self, op: UnaryOp, val: Value) -> Value {
        let ty = self.builder.func.dfg.value_type(val);
        let is_float = ty.is_float();

        match op {
            UnaryOp::Neg => {
                if is_float {
                    self.builder.ins().fneg(val)
                } else {
                    self.builder.ins().ineg(val)
                }
            }
            UnaryOp::Not => {
                let zero = self.builder.ins().iconst(ty, 0);
                self.builder
                    .ins()
                    .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, val, zero)
            }
            UnaryOp::BitNot => self.builder.ins().bnot(val),
            UnaryOp::Await => {
                // Handled in translate_rvalue (needs expected_ty for load width).
                self.record_ice(
                    "Unary Await must be lowered via rvalue path with expected_ty",
                    self.func_span(),
                );
                self.poison_i32()
            }
            // F2.0: Ref/RefMut are lowered as Borrow rvalues, not Unary.
            // Deref of a pointer-valued SSA: load pointee as expected return type.
            UnaryOp::Ref | UnaryOp::RefMut => {
                self.record_ice(
                    "Unary Ref/RefMut should lower as Borrow, not Unary",
                    self.func_span(),
                );
                self.poison_i32()
            }
            UnaryOp::Deref => {
                // `val` is a pointer; load a machine word (int-sized) by default.
                let load_ty = if ty.is_int() || ty.is_float() {
                    ty
                } else {
                    self.ptr_type
                };
                // When the value is already a pointer type, load through it.
                let ptr = val;
                self.builder
                    .ins()
                    .load(load_ty, cranelift_codegen::ir::MemFlagsData::new(), ptr, 0)
            }
            // `UnaryOp` is `#[non_exhaustive]` across crate boundaries.
            _ => {
                self.record_error(
                    arandu_semantics::DiagCode::U001FeatureNotSupported,
                    "unsupported unary operator in Cranelift backend",
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }
}
