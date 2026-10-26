use super::super::{FuncEmitter, TypedValue, ice};
use arandu_middle::Diagnostic;
use arandu_middle::amir::AmirOperand;
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::ArType;

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_binary(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        if matches!(
            op,
            BinaryOp::NullCoalesce | BinaryOp::RangeExclusive | BinaryOp::RangeInclusive
        ) {
            return Err(ice(format!(
                "LLVM backend does not yet lower binary operator {op:?}"
            )));
        }

        let comparison = matches!(
            op,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::LtEqual
                | BinaryOp::GtEqual
        );
        let operand_ty = if comparison {
            self.module.operand_type(left, self.func)
        } else {
            lhs_ty.clone()
        };
        if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
            && self.module.is_fat_pointer(&operand_ty)
        {
            return self.emit_fat_pointer_eq(
                result,
                left,
                right,
                &operand_ty,
                matches!(op, BinaryOp::NotEqual),
            );
        }
        let left = self.operand(left, &operand_ty)?;
        let right = self.operand(right, &operand_ty)?;
        let signed = self.module.is_signed(&operand_ty);
        let float = self.module.is_float(&operand_ty);

        let expression = match op {
            BinaryOp::Equal => {
                if float {
                    format!("fcmp oeq {}, {}", left.render(), right.value)
                } else {
                    format!("icmp eq {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::NotEqual => {
                if float {
                    format!("fcmp une {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ne {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Lt => {
                if float {
                    format!("fcmp olt {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp slt {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ult {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Gt => {
                if float {
                    format!("fcmp ogt {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sgt {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ugt {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::LtEqual => {
                if float {
                    format!("fcmp ole {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sle {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ule {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::GtEqual => {
                if float {
                    format!("fcmp oge {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sge {}, {}", left.render(), right.value)
                } else {
                    format!("icmp uge {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Add => self.arithmetic("add", "fadd", &left, &right, float),
            BinaryOp::Sub => self.arithmetic("sub", "fsub", &left, &right, float),
            BinaryOp::Mul => self.arithmetic("mul", "fmul", &left, &right, float),
            BinaryOp::Div | BinaryOp::Mod if float => {
                let instruction = if op == BinaryOp::Div { "fdiv" } else { "frem" };
                format!("{instruction} {}, {}", left.render(), right.value)
            }
            // LLVM leaves a zero divisor and signed `MIN / -1` undefined.
            // Trap on both like C, Cranelift and Wasm, and give `MIN % -1`
            // its defined result 0 by dividing by 1 instead.
            BinaryOp::Div | BinaryOp::Mod => {
                let ty = left.ty.clone();
                let zero = self.new_value();
                self.writer
                    .line(format!("{zero} = icmp eq {ty} {}, 0", right.value));
                self.trap_if(&zero);
                let mut divisor = right.value.clone();
                if signed {
                    let Some(width) = self.module.integer_width(&operand_ty) else {
                        return Err(ice(format!(
                            "integer division requires an integer, got {operand_ty:?}"
                        )));
                    };
                    let minus_one = self.new_value();
                    self.writer
                        .line(format!("{minus_one} = icmp eq {ty} {}, -1", right.value));
                    if op == BinaryOp::Div {
                        let min = -(1i128 << (width - 1));
                        let is_min = self.new_value();
                        self.writer
                            .line(format!("{is_min} = icmp eq {ty} {}, {min}", left.value));
                        let overflow = self.new_value();
                        self.writer
                            .line(format!("{overflow} = and i1 {is_min}, {minus_one}"));
                        self.trap_if(&overflow);
                    } else {
                        let safe = self.new_value();
                        self.writer.line(format!(
                            "{safe} = select i1 {minus_one}, {ty} 1, {ty} {}",
                            right.value
                        ));
                        divisor = safe;
                    }
                }
                let instruction = match (op, signed) {
                    (BinaryOp::Div, true) => "sdiv",
                    (BinaryOp::Div, false) => "udiv",
                    (_, true) => "srem",
                    (_, false) => "urem",
                };
                format!("{instruction} {ty} {}, {divisor}", left.value)
            }
            BinaryOp::BitOr => self.arithmetic("or", "or", &left, &right, false),
            BinaryOp::BitXor => self.arithmetic("xor", "xor", &left, &right, false),
            BinaryOp::BitAnd => self.arithmetic("and", "and", &left, &right, false),
            BinaryOp::Or | BinaryOp::And => {
                let instruction = if op == BinaryOp::Or { "or" } else { "and" };
                format!("{instruction} {}, {}", left.render(), right.value)
            }
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                let Some(width) = self.module.integer_width(&operand_ty) else {
                    return Err(ice(format!(
                        "shift requires an integer, got {operand_ty:?}"
                    )));
                };
                let instruction = match op {
                    BinaryOp::ShiftLeft => "shl",
                    BinaryOp::ShiftRight if signed => "ashr",
                    BinaryOp::ShiftRight => "lshr",
                    _ => unreachable!(),
                };
                let narrow_ty = left.ty.clone();
                // Like C, Cranelift and Wasm, 8- and 16-bit values shift in a
                // 32-bit slot with the count masked by 31, then truncate.
                if width < 32 {
                    let extend = if signed { "sext" } else { "zext" };
                    let wide = self.new_value();
                    self.writer.line(format!(
                        "{wide} = {extend} {narrow_ty} {} to i32",
                        left.value
                    ));
                    let count = self.new_value();
                    self.writer
                        .line(format!("{count} = zext {narrow_ty} {} to i32", right.value));
                    let mask = self.new_value();
                    self.writer.line(format!("{mask} = and i32 {count}, 31"));
                    let shifted = self.new_value();
                    self.writer
                        .line(format!("{shifted} = {instruction} i32 {wide}, {mask}"));
                    format!("trunc i32 {shifted} to {narrow_ty}")
                } else {
                    let mask = self.new_value();
                    self.writer.line(format!(
                        "{mask} = and {narrow_ty} {}, {}",
                        right.value,
                        width - 1
                    ));
                    format!("{instruction} {narrow_ty} {}, {mask}", left.value)
                }
            }
            _ => {
                return Err(ice(format!(
                    "LLVM backend does not yet lower binary operator {op:?}"
                )));
            }
        };
        self.writer.line(format!("{} = {expression}", result));
        Ok(())
    }

    pub(super) fn arithmetic(
        &self,
        integer: &str,
        float: &str,
        left: &TypedValue,
        right: &TypedValue,
        is_float: bool,
    ) -> String {
        let instruction = if is_float { float } else { integer };
        format!("{instruction} {}, {}", left.render(), right.value)
    }

    pub(super) fn emit_unary(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: UnaryOp,
        operand: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        match op {
            UnaryOp::Neg => {
                // The result type is the operand type; a bare literal is only
                // typed as an untyped literal (defaulting to i32), so take the
                // width from the destination: `let x: i64 = -5`.
                let operand_ty = lhs_ty.clone();
                let value = self.operand(operand, &operand_ty)?;
                if self.module.is_float(&operand_ty) {
                    self.writer
                        .line(format!("{} = fneg {}", result, value.render()));
                } else {
                    self.writer
                        .line(format!("{} = sub {} 0, {}", result, value.ty, value.value));
                }
                Ok(())
            }
            UnaryOp::Not | UnaryOp::BitNot => {
                let operand_ty = lhs_ty.clone();
                let value = self.operand(operand, &operand_ty)?;
                let all_ones = if value.ty == "i1" {
                    "true".to_owned()
                } else {
                    "-1".to_owned()
                };
                self.writer.line(format!(
                    "{} = xor {} {}, {all_ones}",
                    result, value.ty, value.value
                ));
                Ok(())
            }
            UnaryOp::Await => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = self.operand(operand, &operand_ty)?;
                let payload_ty = match &operand_ty {
                    ArType::Coroutine(inner) => self.module.interner.resolve(*inner),
                    _ => lhs_ty.clone(),
                };
                let address = self.byte_gep(&pointer.value, 8);
                let payload_text = self.module.llvm_ty(&payload_ty)?;
                let align = self.module.align_of(&payload_ty);
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {payload_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty)
            }
            UnaryOp::Deref => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = self.operand(operand, &operand_ty)?;
                let pointee = match &operand_ty {
                    ArType::Ptr(inner)
                    | ArType::Ref(inner)
                    | ArType::RefMut(inner)
                    | ArType::Nullable(inner) => self.module.interner.resolve(*inner),
                    other => {
                        return Err(ice(format!("LLVM deref requires a pointer, got {other:?}")));
                    }
                };
                let pointee_text = self.module.llvm_ty(&pointee)?;
                let align = self.module.align_of(&pointee);
                self.writer.line(format!(
                    "{} = load {pointee_text}, ptr {}, align {align}",
                    result, pointer.value
                ));
                let _ = lhs_ty;
                Ok(())
            }
            other => Err(ice(format!(
                "LLVM backend does not yet lower unary operator {other:?}"
            ))),
        }
    }
}
