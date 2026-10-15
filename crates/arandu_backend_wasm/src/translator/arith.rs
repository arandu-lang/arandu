use arandu_middle::amir::value::AmirOperand;
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction, ValType};

use super::FuncTranslator;
use crate::types;

impl<'a> FuncTranslator<'a> {
    /// Emit a binary operation.
    pub(super) fn emit_binary(
        &mut self,
        op: BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
        result_ty: TypeId,
    ) {
        let left_ty = self.operand_arity_ty(left);
        let right_ty = self.operand_arity_ty(right);
        let is_str =
            |ty: TypeId| matches!(self.interner.resolve(ty), ArType::Primitive(Primitive::Str));
        if (is_str(left_ty) || is_str(right_ty))
            && matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
        {
            let operand_ty = if is_str(left_ty) { left_ty } else { right_ty };
            self.emit_string_equality(op, left, right, operand_ty);
            return;
        }

        let is_empty = |ty: TypeId| {
            types::shape(ty, self.interner, self.layout_engine.data_layout) == types::Shape::Empty
        };

        // Comparisons return `bool`, but the instruction and operand stack
        // type comes from their operands. Selecting an opcode from `result_ty`
        // would emit an i32 comparison for f64/i64 values and produce an
        // invalid Wasm module. For Nil / empty operands, select the sibling's
        // type so nil compares as a zero of matching width, or fall back to
        // i32 if both are empty (e.g. nil == nil).
        let operation_ty = if matches!(
            op,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::LtEqual
                | BinaryOp::GtEqual
        ) {
            if !is_empty(left_ty) {
                left_ty
            } else if !is_empty(right_ty) {
                right_ty
            } else {
                self.interner.intern(ArType::Primitive(Primitive::Int))
            }
        } else {
            result_ty
        };
        let is_unsigned = types::ar_is_unsigned(operation_ty, self.interner);
        let is_float = types::ar_is_float(operation_ty, self.interner);
        let is_64 = types::ar_is_64bit(operation_ty, self.interner, self.layout_engine.data_layout);

        self.emit_operand(left, operation_ty);
        self.emit_operand(right, operation_ty);

        if is_float {
            // The AMIR operand type determines the stack format. A bool
            // result does not change an f32 comparison into an f64 operation.
            let is_f32 = matches!(
                types::slot_valtype(
                    operation_ty,
                    0,
                    self.interner,
                    self.layout_engine.data_layout
                ),
                Some(ValType::F32)
            );
            match (is_f32, op) {
                (true, BinaryOp::Add) => self.code.push(Instruction::F32Add),
                (true, BinaryOp::Sub) => self.code.push(Instruction::F32Sub),
                (true, BinaryOp::Mul) => self.code.push(Instruction::F32Mul),
                (true, BinaryOp::Div) => self.code.push(Instruction::F32Div),
                (true, BinaryOp::Equal) => self.code.push(Instruction::F32Eq),
                (true, BinaryOp::NotEqual) => self.code.push(Instruction::F32Ne),
                (true, BinaryOp::Lt) => self.code.push(Instruction::F32Lt),
                (true, BinaryOp::Gt) => self.code.push(Instruction::F32Gt),
                (true, BinaryOp::LtEqual) => self.code.push(Instruction::F32Le),
                (true, BinaryOp::GtEqual) => self.code.push(Instruction::F32Ge),
                (false, BinaryOp::Add) => self.code.push(Instruction::F64Add),
                (false, BinaryOp::Sub) => self.code.push(Instruction::F64Sub),
                (false, BinaryOp::Mul) => self.code.push(Instruction::F64Mul),
                (false, BinaryOp::Div) => self.code.push(Instruction::F64Div),
                (false, BinaryOp::Equal) => self.code.push(Instruction::F64Eq),
                (false, BinaryOp::NotEqual) => self.code.push(Instruction::F64Ne),
                (false, BinaryOp::Lt) => self.code.push(Instruction::F64Lt),
                (false, BinaryOp::Gt) => self.code.push(Instruction::F64Gt),
                (false, BinaryOp::LtEqual) => self.code.push(Instruction::F64Le),
                (false, BinaryOp::GtEqual) => self.code.push(Instruction::F64Ge),
                _ => self.emit_zero(result_ty),
            }
            return;
        }

        if is_64 {
            match op {
                BinaryOp::Add => self.code.push(Instruction::I64Add),
                BinaryOp::Sub => self.code.push(Instruction::I64Sub),
                BinaryOp::Mul => self.code.push(Instruction::I64Mul),
                BinaryOp::Div if is_unsigned => self.code.push(Instruction::I64DivU),
                BinaryOp::Div => self.code.push(Instruction::I64DivS),
                BinaryOp::Mod if is_unsigned => self.code.push(Instruction::I64RemU),
                BinaryOp::Mod => self.code.push(Instruction::I64RemS),
                BinaryOp::Equal => self.code.push(Instruction::I64Eq),
                BinaryOp::NotEqual => self.code.push(Instruction::I64Ne),
                BinaryOp::Lt if is_unsigned => self.code.push(Instruction::I64LtU),
                BinaryOp::Lt => self.code.push(Instruction::I64LtS),
                BinaryOp::Gt if is_unsigned => self.code.push(Instruction::I64GtU),
                BinaryOp::Gt => self.code.push(Instruction::I64GtS),
                BinaryOp::LtEqual if is_unsigned => self.code.push(Instruction::I64LeU),
                BinaryOp::LtEqual => self.code.push(Instruction::I64LeS),
                BinaryOp::GtEqual if is_unsigned => self.code.push(Instruction::I64GeU),
                BinaryOp::GtEqual => self.code.push(Instruction::I64GeS),
                BinaryOp::BitOr => self.code.push(Instruction::I64Or),
                BinaryOp::BitXor => self.code.push(Instruction::I64Xor),
                BinaryOp::BitAnd => self.code.push(Instruction::I64And),
                BinaryOp::ShiftLeft => self.code.push(Instruction::I64Shl),
                BinaryOp::ShiftRight if is_unsigned => self.code.push(Instruction::I64ShrU),
                BinaryOp::ShiftRight => self.code.push(Instruction::I64ShrS),
                BinaryOp::Or => {
                    // Logical OR: (left != 0) | (right != 0). The comparison
                    // results are i32, so widen them back to i64 before OR.
                    self.code.push(Instruction::LocalSet(self.scratch_i64));
                    self.code.push(Instruction::LocalSet(self.scratch_i64b));
                    self.code.push(Instruction::LocalGet(self.scratch_i64b));
                    self.code.push(Instruction::I64Const(0));
                    self.code.push(Instruction::I64Ne);
                    self.code.push(Instruction::I64ExtendI32U);
                    self.code.push(Instruction::LocalGet(self.scratch_i64));
                    self.code.push(Instruction::I64Const(0));
                    self.code.push(Instruction::I64Ne);
                    self.code.push(Instruction::I64ExtendI32U);
                    self.code.push(Instruction::I64Or);
                }
                BinaryOp::And => {
                    // Logical AND: (left != 0) & (right != 0).
                    self.code.push(Instruction::LocalSet(self.scratch_i64));
                    self.code.push(Instruction::LocalSet(self.scratch_i64b));
                    self.code.push(Instruction::LocalGet(self.scratch_i64b));
                    self.code.push(Instruction::I64Const(0));
                    self.code.push(Instruction::I64Ne);
                    self.code.push(Instruction::I64ExtendI32U);
                    self.code.push(Instruction::LocalGet(self.scratch_i64));
                    self.code.push(Instruction::I64Const(0));
                    self.code.push(Instruction::I64Ne);
                    self.code.push(Instruction::I64ExtendI32U);
                    self.code.push(Instruction::I64And);
                }
                _ => self.emit_zero(result_ty),
            }
            return;
        }

        // 32-bit integer operations.
        match op {
            BinaryOp::Add => self.code.push(Instruction::I32Add),
            BinaryOp::Sub => self.code.push(Instruction::I32Sub),
            BinaryOp::Mul => self.code.push(Instruction::I32Mul),
            BinaryOp::Div if is_unsigned => self.code.push(Instruction::I32DivU),
            BinaryOp::Div => self.code.push(Instruction::I32DivS),
            BinaryOp::Mod if is_unsigned => self.code.push(Instruction::I32RemU),
            BinaryOp::Mod => self.code.push(Instruction::I32RemS),
            BinaryOp::Equal => self.code.push(Instruction::I32Eq),
            BinaryOp::NotEqual => self.code.push(Instruction::I32Ne),
            BinaryOp::Lt if is_unsigned => self.code.push(Instruction::I32LtU),
            BinaryOp::Lt => self.code.push(Instruction::I32LtS),
            BinaryOp::Gt if is_unsigned => self.code.push(Instruction::I32GtU),
            BinaryOp::Gt => self.code.push(Instruction::I32GtS),
            BinaryOp::LtEqual if is_unsigned => self.code.push(Instruction::I32LeU),
            BinaryOp::LtEqual => self.code.push(Instruction::I32LeS),
            BinaryOp::GtEqual if is_unsigned => self.code.push(Instruction::I32GeU),
            BinaryOp::GtEqual => self.code.push(Instruction::I32GeS),
            BinaryOp::BitOr => self.code.push(Instruction::I32Or),
            BinaryOp::BitXor => self.code.push(Instruction::I32Xor),
            BinaryOp::BitAnd => self.code.push(Instruction::I32And),
            BinaryOp::ShiftLeft => self.code.push(Instruction::I32Shl),
            BinaryOp::ShiftRight if is_unsigned => self.code.push(Instruction::I32ShrU),
            BinaryOp::ShiftRight => self.code.push(Instruction::I32ShrS),
            BinaryOp::Or => {
                // Logical OR: (left != 0) | (right != 0).
                // Stack currently has [left, right].
                self.code.push(Instruction::LocalSet(self.scratch)); // pops right
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::I32Ne); // left != 0
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::I32Ne); // right != 0
                self.code.push(Instruction::I32Or);
            }
            BinaryOp::And => {
                // Logical AND: (left != 0) & (right != 0).
                // Stack currently has [left, right].
                self.code.push(Instruction::LocalSet(self.scratch)); // pops right
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::I32Ne); // left != 0
                self.code.push(Instruction::LocalGet(self.scratch));
                self.code.push(Instruction::I32Const(0));
                self.code.push(Instruction::I32Ne); // right != 0
                self.code.push(Instruction::I32And);
            }
            _ => {
                self.emit_zero(result_ty);
            }
        }
    }

    /// Compare string contents by byte length and then byte value.
    ///
    /// The Wasm ABI lowers `str` to `(ptr, len)`. Comparing only one I32 slot
    /// leaves values on the operand stack and can also treat distinct strings
    /// with equal contents as unequal. Pointer locals advance together, while
    /// the left length is used as a bounded loop counter.
    fn emit_string_equality(
        &mut self,
        op: BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
        operand_ty: TypeId,
    ) {
        self.emit_operand(left, operand_ty);
        self.emit_operand(right, operand_ty);
        // Stack: left_ptr, left_len, right_ptr, right_len.
        self.code.push(Instruction::LocalSet(self.scratch_d));
        self.code.push(Instruction::LocalSet(self.scratch_c));
        self.code.push(Instruction::LocalSet(self.scratch_b));
        self.code.push(Instruction::LocalSet(self.scratch));

        self.code
            .push(Instruction::Block(BlockType::Result(ValType::I32)));
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::LocalGet(self.scratch_d));
        self.code.push(Instruction::I32Ne);
        self.code.push(Instruction::If(BlockType::Empty));
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::Br(1));
        self.code.push(Instruction::End);

        self.code.push(Instruction::Loop(BlockType::Empty));
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::I32Eqz);
        self.code.push(Instruction::If(BlockType::Empty));
        self.code.push(Instruction::I32Const(1));
        self.code.push(Instruction::Br(2));
        self.code.push(Instruction::End);

        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::I32Load8U(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        self.code.push(Instruction::LocalGet(self.scratch_c));
        self.code.push(Instruction::I32Load8U(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        self.code.push(Instruction::I32Ne);
        self.code.push(Instruction::If(BlockType::Empty));
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::Br(2));
        self.code.push(Instruction::End);

        self.code.push(Instruction::LocalGet(self.scratch));
        self.code.push(Instruction::I32Const(1));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::LocalSet(self.scratch));
        self.code.push(Instruction::LocalGet(self.scratch_c));
        self.code.push(Instruction::I32Const(1));
        self.code.push(Instruction::I32Add);
        self.code.push(Instruction::LocalSet(self.scratch_c));
        self.code.push(Instruction::LocalGet(self.scratch_b));
        self.code.push(Instruction::I32Const(1));
        self.code.push(Instruction::I32Sub);
        self.code.push(Instruction::LocalSet(self.scratch_b));
        self.code.push(Instruction::Br(0));
        self.code.push(Instruction::End);
        // The structured validator permits a loop to fall through even though
        // this body always branches back or exits the outer block. Keep a
        // fallback value for that structurally reachable path.
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::End);

        if op == BinaryOp::NotEqual {
            self.code.push(Instruction::I32Eqz);
        }
    }

    /// Emit a unary operation.
    pub(super) fn emit_unary(&mut self, op: UnaryOp, operand: &AmirOperand, result_ty: TypeId) {
        match op {
            UnaryOp::Neg => {
                if types::ar_is_float(result_ty, self.interner) {
                    self.emit_operand(operand, result_ty);
                    let instruction = match types::slot_valtype(
                        result_ty,
                        0,
                        self.interner,
                        self.layout_engine.data_layout,
                    ) {
                        Some(ValType::F32) => Instruction::F32Neg,
                        _ => Instruction::F64Neg,
                    };
                    self.code.push(instruction);
                } else if types::ar_is_64bit(
                    result_ty,
                    self.interner,
                    self.layout_engine.data_layout,
                ) {
                    // Emit 0 first, then the operand: `i64.sub` computes
                    // `operand - 0` in that order, so `0 - x` negates.
                    self.code.push(Instruction::I64Const(0));
                    self.emit_operand(operand, result_ty);
                    self.code.push(Instruction::I64Sub);
                } else {
                    // `i32.sub` computes `operand - 0` if the zero is pushed
                    // after the operand; push the zero first for `0 - x`.
                    self.code.push(Instruction::I32Const(0));
                    self.emit_operand(operand, result_ty);
                    self.code.push(Instruction::I32Sub);
                }
            }
            UnaryOp::Not => {
                self.emit_operand(operand, result_ty);
                self.code.push(Instruction::I32Eqz);
            }
            UnaryOp::BitNot => {
                self.emit_operand(operand, result_ty);
                if types::ar_is_64bit(result_ty, self.interner, self.layout_engine.data_layout) {
                    self.code.push(Instruction::I64Const(-1));
                    self.code.push(Instruction::I64Xor);
                } else {
                    self.code.push(Instruction::I32Const(-1));
                    self.code.push(Instruction::I32Xor);
                }
            }
            UnaryOp::Await => {
                let ptr_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                self.emit_operand(operand, ptr_ty);
                self.emit_load_value_at(result_ty, 8);
            }
            UnaryOp::Deref => {
                let operand_ty = self.operand_arity_ty(operand);
                if self.interner.slice_abi_element(operand_ty).is_some() {
                    self.emit_operand(operand, result_ty);
                } else {
                    let ptr_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    self.emit_operand(operand, ptr_ty);
                    self.emit_load_value_at(result_ty, 0);
                }
            }
            _ => {
                // Ref, RefMut: identity.
                self.emit_operand(operand, result_ty);
            }
        }
    }
}
