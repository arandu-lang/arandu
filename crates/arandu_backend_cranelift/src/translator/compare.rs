use arandu_semantics::amir::AmirOperand;
use arandu_semantics::ops::BinaryOp;
use cranelift_codegen::ir::{InstBuilder, Value};

use super::FunctionTranslator;
use crate::types::ar_type_is_unsigned_comparison;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn operand_is_unsigned_integer(&self, operand: &AmirOperand) -> Option<bool> {
        let temp_id = match operand {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => *temp_id,
            _ => return None,
        };
        self.current_func
            .temps
            .get(temp_id.as_usize())
            .map(|temp| ar_type_is_unsigned_comparison(&self.resolve_ty(temp.ty)))
    }

    pub(super) fn operands_are_unsigned(
        &self,
        left: Option<&AmirOperand>,
        right: Option<&AmirOperand>,
    ) -> bool {
        left.and_then(|op| self.operand_is_unsigned_integer(op))
            .or_else(|| right.and_then(|op| self.operand_is_unsigned_integer(op)))
            .unwrap_or(false)
    }

    pub(super) fn translate_binary_op(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
        left_operand: Option<&AmirOperand>,
        right_operand: Option<&AmirOperand>,
    ) -> Value {
        let is_unsigned = self.operands_are_unsigned(left_operand, right_operand);
        let mut lhs = lhs;
        let mut rhs = rhs;
        let lhs_ty = self.builder.func.dfg.value_type(lhs);
        let rhs_ty = self.builder.func.dfg.value_type(rhs);
        let is_float = lhs_ty.is_float() || rhs_ty.is_float();

        if !is_float && lhs_ty != rhs_ty {
            if lhs_ty.bits() < rhs_ty.bits() {
                if is_unsigned {
                    lhs = self.builder.ins().uextend(rhs_ty, lhs);
                } else {
                    lhs = self.builder.ins().sextend(rhs_ty, lhs);
                }
            } else {
                if is_unsigned {
                    rhs = self.builder.ins().uextend(lhs_ty, rhs);
                } else {
                    rhs = self.builder.ins().sextend(lhs_ty, rhs);
                }
            }
        }

        match op {
            BinaryOp::Add => {
                if is_float {
                    self.builder.ins().fadd(lhs, rhs)
                } else {
                    self.builder.ins().iadd(lhs, rhs)
                }
            }
            BinaryOp::Sub => {
                if is_float {
                    self.builder.ins().fsub(lhs, rhs)
                } else {
                    self.builder.ins().isub(lhs, rhs)
                }
            }
            BinaryOp::Mul => {
                if is_float {
                    self.builder.ins().fmul(lhs, rhs)
                } else {
                    self.builder.ins().imul(lhs, rhs)
                }
            }
            BinaryOp::Div => {
                if is_float {
                    self.builder.ins().fdiv(lhs, rhs)
                } else if is_unsigned {
                    self.builder.ins().udiv(lhs, rhs)
                } else {
                    self.builder.ins().sdiv(lhs, rhs)
                }
            }
            BinaryOp::Mod => {
                if is_float {
                    let Some(fmod_id) = self.fmod_func_id() else {
                        return self.poison_i32();
                    };
                    let local_ref = self.module.declare_func_in_func(fmod_id, self.builder.func);
                    let call_inst = self.builder.ins().call(local_ref, &[lhs, rhs]);
                    self.builder.inst_results(call_inst)[0]
                } else if is_unsigned {
                    self.builder.ins().urem(lhs, rhs)
                } else {
                    self.builder.ins().srem(lhs, rhs)
                }
            }
            BinaryOp::BitOr => self.builder.ins().bor(lhs, rhs),
            BinaryOp::BitAnd => self.builder.ins().band(lhs, rhs),
            BinaryOp::BitXor => self.builder.ins().bxor(lhs, rhs),
            BinaryOp::ShiftLeft => self.builder.ins().ishl(lhs, rhs),
            BinaryOp::ShiftRight => {
                if is_unsigned {
                    self.builder.ins().ushr(lhs, rhs)
                } else {
                    self.builder.ins().sshr(lhs, rhs)
                }
            }
            BinaryOp::Equal => {
                if is_float {
                    self.builder.ins().fcmp(
                        cranelift_codegen::ir::condcodes::FloatCC::Equal,
                        lhs,
                        rhs,
                    )
                } else {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        lhs,
                        rhs,
                    )
                }
            }
            BinaryOp::NotEqual => {
                if is_float {
                    self.builder.ins().fcmp(
                        cranelift_codegen::ir::condcodes::FloatCC::NotEqual,
                        lhs,
                        rhs,
                    )
                } else {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                        lhs,
                        rhs,
                    )
                }
            }
            BinaryOp::Lt => {
                if is_float {
                    self.builder.ins().fcmp(
                        cranelift_codegen::ir::condcodes::FloatCC::LessThan,
                        lhs,
                        rhs,
                    )
                } else if is_unsigned {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                        lhs,
                        rhs,
                    )
                } else {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                        lhs,
                        rhs,
                    )
                }
            }
            BinaryOp::Gt => {
                if is_float {
                    self.builder.ins().fcmp(
                        cranelift_codegen::ir::condcodes::FloatCC::GreaterThan,
                        lhs,
                        rhs,
                    )
                } else if is_unsigned {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
                        lhs,
                        rhs,
                    )
                } else {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan,
                        lhs,
                        rhs,
                    )
                }
            }
            BinaryOp::LtEqual => {
                if is_float {
                    self.builder.ins().fcmp(
                        cranelift_codegen::ir::condcodes::FloatCC::LessThanOrEqual,
                        lhs,
                        rhs,
                    )
                } else if is_unsigned {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThanOrEqual,
                        lhs,
                        rhs,
                    )
                } else {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::SignedLessThanOrEqual,
                        lhs,
                        rhs,
                    )
                }
            }
            BinaryOp::GtEqual => {
                if is_float {
                    self.builder.ins().fcmp(
                        cranelift_codegen::ir::condcodes::FloatCC::GreaterThanOrEqual,
                        lhs,
                        rhs,
                    )
                } else if is_unsigned {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                        lhs,
                        rhs,
                    )
                } else {
                    self.builder.ins().icmp(
                        cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThanOrEqual,
                        lhs,
                        rhs,
                    )
                }
            }
            BinaryOp::Or => self.builder.ins().bor(lhs, rhs),
            BinaryOp::And => self.builder.ins().band(lhs, rhs),
            BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => {
                let Some(malloc_func_id) = self.malloc_func_id() else {
                    return self.poison_i32();
                };
                let local_ref = self
                    .module
                    .declare_func_in_func(malloc_func_id, self.builder.func);
                let pointer_width = self.ptr_type.bytes() as i64;
                let size_val = self.builder.ins().iconst(self.ptr_type, pointer_width * 2);
                let call_inst = self.builder.ins().call(local_ref, &[size_val]);
                let ptr_val = self.builder.inst_results(call_inst)[0];
                self.trap_if_null(ptr_val);

                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    lhs,
                    ptr_val,
                    0,
                );
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    rhs,
                    ptr_val,
                    pointer_width as i32,
                );
                ptr_val
            }
            BinaryOp::NullCoalesce => {
                // Root honesty: `??` is CFG-lowered in AMIR (`HirExprKind::NullCoalesce` →
                // branch on `!= nil`). A BinaryOp::NullCoalesce that reaches codegen is a
                // pipeline bug, not a missing feature.
                self.record_ice(
                    "internal: NullCoalesce must be CFG-lowered before codegen (not a BinaryOp)",
                    self.func_span(),
                );
                self.poison_i32()
            }
            _ => {
                self.record_ice(
                    format!(
                        "Binary operator {:?} not implemented in Cranelift JIT yet",
                        op
                    ),
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }
}
