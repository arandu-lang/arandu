use super::super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand};
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{ArType, Primitive};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_binary_rvalue(
        &mut self,
        op: &BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        if matches!(op, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive) {
            let left_str = self.format_operand(left, func);
            let right_str = self.format_operand(right, func);
            let _ = write!(
                &mut self.output,
                "ar_make_range((intptr_t)({}), (intptr_t)({}))",
                left_str, right_str
            );
        } else {
            let left_str = self.format_operand(left, func);
            let right_str = self.format_operand(right, func);
            if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
                && let (Some(left_depth), Some(right_depth)) = (
                    self.operand_ty(func, left)
                        .str_reference_depth(self.interner),
                    self.operand_ty(func, right)
                        .str_reference_depth(self.interner),
                )
            {
                let descriptor = |mut expression: String, depth| {
                    for _ in 0..depth {
                        expression = format!("*({expression})");
                    }
                    expression
                };
                let left = descriptor(left_str, left_depth);
                let right = descriptor(right_str, right_depth);
                let negate = if *op == BinaryOp::NotEqual { "!" } else { "" };
                let _ = write!(&mut self.output, "{negate}ar_str_eq({left}, {right})");
                return;
            }
            let integer_result = match expected_ar_type {
                ArType::IntLiteral => true,
                ArType::Primitive(primitive) => primitive.is_integer(),
                _ => false,
            };
            if matches!(op, BinaryOp::ShiftLeft | BinaryOp::ShiftRight) && integer_result {
                let bits = self.checked_layout(expected_ar_type).size * 8;
                let execution_bits = bits.max(32);
                let signed =
                    expected_ar_type.is_signed() || matches!(expected_ar_type, ArType::IntLiteral);
                let _ = write!(
                    self.output,
                    "({{ {expected_c_type} ar_lhs = ({left_str}); unsigned ar_count = (unsigned)((uint64_t)({right_str}) & {}); uint{bits}_t ar_bits = ",
                    execution_bits - 1
                );
                if *op == BinaryOp::ShiftLeft {
                    let _ = write!(self.output, "(uint{execution_bits}_t)ar_lhs << ar_count; ");
                } else if signed {
                    // Complementing before and after an unsigned right
                    // shift implements sign extension without relying
                    // on C's implementation-defined negative >>.
                    let _ = write!(
                        self.output,
                        "ar_lhs < 0 ? ~((uint{execution_bits}_t)(~ar_lhs) >> ar_count) : (uint{execution_bits}_t)ar_lhs >> ar_count; "
                    );
                } else {
                    let _ = write!(self.output, "(uint{execution_bits}_t)ar_lhs >> ar_count; ");
                }
                // Copying the result bits also avoids an out-of-range
                // unsigned-to-signed cast for left shifts.
                let _ = write!(
                    self.output,
                    "{expected_c_type} ar_result; memcpy(&ar_result, &ar_bits, sizeof(ar_result)); ar_result; }})"
                );
                return;
            }
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && integer_result {
                // C division is undefined for zero and signed MIN / -1.
                // Signed remainder must return zero for MIN % -1, as
                // Cranelift/Wasm do, without evaluating C's undefined %.
                let signed =
                    expected_ar_type.is_signed() || matches!(expected_ar_type, ArType::IntLiteral);
                let _ = write!(
                    self.output,
                    "({{ {expected_c_type} ar_lhs = ({left_str}); {expected_c_type} ar_rhs = ({right_str}); if (ar_rhs == 0) {{ AR_ABORT(); }} "
                );
                let operator = if *op == BinaryOp::Div { "/" } else { "%" };
                if signed {
                    if *op == BinaryOp::Div {
                        let bits = self.checked_layout(expected_ar_type).size * 8;
                        let _ = write!(
                            self.output,
                            "if (ar_lhs == INT{bits}_MIN && ar_rhs == -1) {{ AR_ABORT(); }} "
                        );
                    }
                    if *op == BinaryOp::Mod {
                        let _ = write!(
                            self.output,
                            "({expected_c_type})(ar_rhs == -1 ? 0 : ar_lhs % ar_rhs); }})"
                        );
                        return;
                    }
                }
                let _ = write!(
                    self.output,
                    "({expected_c_type})(ar_lhs {operator} ar_rhs); }})"
                );
                return;
            }
            let op_str = match op {
                BinaryOp::Add => "+",
                BinaryOp::Sub => "-",
                BinaryOp::Mul => "*",
                BinaryOp::Div => "/",
                BinaryOp::Mod => "%",
                BinaryOp::Equal => "==",
                BinaryOp::NotEqual => "!=",
                BinaryOp::Lt => "<",
                BinaryOp::LtEqual => "<=",
                BinaryOp::Gt => ">",
                BinaryOp::GtEqual => ">=",
                BinaryOp::And => "&&",
                BinaryOp::Or => "||",
                BinaryOp::BitAnd => "&",
                BinaryOp::BitOr => "|",
                BinaryOp::BitXor => "^",
                BinaryOp::ShiftLeft => "<<",
                BinaryOp::ShiftRight => ">>",
                BinaryOp::NullCoalesce => {
                    self.record_codegen_ice(
                        func,
                        "NullCoalesce reached C codegen before CFG lowering",
                    );
                    "/* invalid NullCoalesce */"
                }
                BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => unreachable!(),
                _ => unreachable!(),
            };
            let _ = write!(&mut self.output, "{} {} {}", left_str, op_str, right_str);
        }
    }

    pub(super) fn emit_unary_rvalue(
        &mut self,
        op: &UnaryOp,
        operand: &AmirOperand,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        let op_val = self.format_operand(operand, func);
        match op {
            UnaryOp::Neg => {
                let _ = write!(&mut self.output, "-{}", op_val);
            }
            UnaryOp::Not => {
                let _ = write!(&mut self.output, "!{}", op_val);
            }
            UnaryOp::BitNot => {
                let _ = write!(&mut self.output, "~{}", op_val);
            }
            // A3.6: await = poll until Ready; disc@0, payload@8.
            // Load/store the **expected payload C type** (not i64 cast). Casting
            // block_on_i64 → float/ptr was the root of nonsensical C for non-int.
            UnaryOp::Await => {
                let is_float = matches!(
                    expected_ar_type,
                    ArType::Primitive(Primitive::Float) | ArType::FloatLiteral
                );
                let is_ptr = matches!(
                    expected_ar_type,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                );
                let helper = if is_float {
                    "ar_co_await_f64"
                } else if is_ptr {
                    "ar_co_await_ptr"
                } else {
                    "ar_co_await_i64"
                };
                let _ = write!(
                    &mut self.output,
                    "({expected_c_type}){helper}((uint8_t*)({op_val}))"
                );
            }
            UnaryOp::Deref => {
                let operand_ty = match operand {
                    AmirOperand::Copy(temp) | AmirOperand::Move(temp) => self.temp_ty(func, *temp),
                    _ => ArType::Error,
                };
                if operand_ty.is_borrowed_slice_abi(self.interner) {
                    let _ = write!(&mut self.output, "{}", op_val);
                } else {
                    let _ = write!(&mut self.output, "*{}", op_val);
                }
            }
            UnaryOp::Ref | UnaryOp::RefMut => {
                self.record_codegen_ice(
                    func,
                    "Unary Ref/RefMut reached C codegen before Borrow lowering",
                );
                let _ = write!(&mut self.output, "/* invalid reference rvalue */ 0");
            }
            _ => {
                let _ = write!(&mut self.output, "{}", op_val);
            }
        }
    }
}
