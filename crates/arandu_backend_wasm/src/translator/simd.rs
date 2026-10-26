//! Fixed-width 128-bit SIMD lowers directly to the WebAssembly SIMD proposal.

use arandu_middle::amir::{AmirOperand, VectorCompareOp, VectorOp};
use arandu_middle::types::{ArType, TypeId, VectorType};
use wasm_encoder::Instruction;

use super::FuncTranslator;

impl FuncTranslator<'_> {
    pub(super) fn emit_vector_compare(
        &mut self,
        op: VectorCompareOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) {
        let ty = self.operand_arity_ty(left);
        let ArType::Vector(vector) = self.interner.resolve(ty) else {
            self.code.push(Instruction::Unreachable);
            return;
        };
        let instruction = match (vector, op) {
            (VectorType::F32x4, VectorCompareOp::Eq) => Instruction::F32x4Eq,
            (VectorType::F32x4, VectorCompareOp::Ne) => Instruction::F32x4Ne,
            (VectorType::F32x4, VectorCompareOp::Lt) => Instruction::F32x4Lt,
            (VectorType::F32x4, VectorCompareOp::Le) => Instruction::F32x4Le,
            (VectorType::F32x4, VectorCompareOp::Gt) => Instruction::F32x4Gt,
            (VectorType::F32x4, VectorCompareOp::Ge) => Instruction::F32x4Ge,
            (VectorType::F64x2, VectorCompareOp::Eq) => Instruction::F64x2Eq,
            (VectorType::F64x2, VectorCompareOp::Ne) => Instruction::F64x2Ne,
            (VectorType::F64x2, VectorCompareOp::Lt) => Instruction::F64x2Lt,
            (VectorType::F64x2, VectorCompareOp::Le) => Instruction::F64x2Le,
            (VectorType::F64x2, VectorCompareOp::Gt) => Instruction::F64x2Gt,
            (VectorType::F64x2, VectorCompareOp::Ge) => Instruction::F64x2Ge,
            (VectorType::I32x4, VectorCompareOp::Eq) => Instruction::I32x4Eq,
            (VectorType::I32x4, VectorCompareOp::Ne) => Instruction::I32x4Ne,
            (VectorType::I32x4, VectorCompareOp::Lt) => Instruction::I32x4LtS,
            (VectorType::I32x4, VectorCompareOp::Le) => Instruction::I32x4LeS,
            (VectorType::I32x4, VectorCompareOp::Gt) => Instruction::I32x4GtS,
            (VectorType::I32x4, VectorCompareOp::Ge) => Instruction::I32x4GeS,
            (VectorType::I16x8, VectorCompareOp::Eq) => Instruction::I16x8Eq,
            (VectorType::I16x8, VectorCompareOp::Ne) => Instruction::I16x8Ne,
            (VectorType::I16x8, VectorCompareOp::Lt) => Instruction::I16x8LtS,
            (VectorType::I16x8, VectorCompareOp::Le) => Instruction::I16x8LeS,
            (VectorType::I16x8, VectorCompareOp::Gt) => Instruction::I16x8GtS,
            (VectorType::I16x8, VectorCompareOp::Ge) => Instruction::I16x8GeS,
            (VectorType::U8x16, VectorCompareOp::Eq) => Instruction::I8x16Eq,
            (VectorType::U8x16, VectorCompareOp::Ne) => Instruction::I8x16Ne,
            (VectorType::U8x16, VectorCompareOp::Lt) => Instruction::I8x16LtU,
            (VectorType::U8x16, VectorCompareOp::Le) => Instruction::I8x16LeU,
            (VectorType::U8x16, VectorCompareOp::Gt) => Instruction::I8x16GtU,
            (VectorType::U8x16, VectorCompareOp::Ge) => Instruction::I8x16GeU,
            (VectorType::I64x2, VectorCompareOp::Eq) => Instruction::I64x2Eq,
            (VectorType::I64x2, VectorCompareOp::Ne) => Instruction::I64x2Ne,
            (VectorType::I64x2, VectorCompareOp::Lt) => Instruction::I64x2LtS,
            (VectorType::I64x2, VectorCompareOp::Le) => Instruction::I64x2LeS,
            (VectorType::I64x2, VectorCompareOp::Gt) => Instruction::I64x2GtS,
            (VectorType::I64x2, VectorCompareOp::Ge) => Instruction::I64x2GeS,
        };
        self.emit_operand(left, ty);
        self.emit_operand(right, ty);
        self.code.push(instruction);
    }

    pub(super) fn emit_vector_op(
        &mut self,
        op: VectorOp,
        left: &AmirOperand,
        right: &AmirOperand,
        ty: TypeId,
    ) {
        let ArType::Vector(vector) = self.interner.resolve(ty) else {
            self.code.push(Instruction::Unreachable);
            return;
        };
        // The shared AMIR validator rejects unsupported shape/operator pairs.
        let instruction = match (op, vector) {
            (VectorOp::BitAnd, _) => Instruction::V128And,
            (VectorOp::BitOr, _) => Instruction::V128Or,
            (VectorOp::BitXor, _) => Instruction::V128Xor,
            (VectorOp::Add, VectorType::F32x4) => Instruction::F32x4Add,
            (VectorOp::Sub, VectorType::F32x4) => Instruction::F32x4Sub,
            (VectorOp::Mul, VectorType::F32x4) => Instruction::F32x4Mul,
            (VectorOp::Div, VectorType::F32x4) => Instruction::F32x4Div,
            (VectorOp::Add, VectorType::F64x2) => Instruction::F64x2Add,
            (VectorOp::Sub, VectorType::F64x2) => Instruction::F64x2Sub,
            (VectorOp::Mul, VectorType::F64x2) => Instruction::F64x2Mul,
            (VectorOp::Div, VectorType::F64x2) => Instruction::F64x2Div,
            (VectorOp::Add, VectorType::I32x4) => Instruction::I32x4Add,
            (VectorOp::Sub, VectorType::I32x4) => Instruction::I32x4Sub,
            (VectorOp::Mul, VectorType::I32x4) => Instruction::I32x4Mul,
            (VectorOp::Add, VectorType::I16x8) => Instruction::I16x8Add,
            (VectorOp::Sub, VectorType::I16x8) => Instruction::I16x8Sub,
            (VectorOp::Mul, VectorType::I16x8) => Instruction::I16x8Mul,
            (VectorOp::Add, VectorType::U8x16) => Instruction::I8x16Add,
            (VectorOp::Sub, VectorType::U8x16) => Instruction::I8x16Sub,
            (VectorOp::Add, VectorType::I64x2) => Instruction::I64x2Add,
            (VectorOp::Sub, VectorType::I64x2) => Instruction::I64x2Sub,
            (VectorOp::Mul, VectorType::I64x2) => Instruction::I64x2Mul,
            (VectorOp::Mul, VectorType::U8x16)
            | (
                VectorOp::Div,
                VectorType::I32x4 | VectorType::I16x8 | VectorType::U8x16 | VectorType::I64x2,
            ) => {
                self.code.push(Instruction::Unreachable);
                return;
            }
        };
        self.emit_operand(left, ty);
        self.emit_operand(right, ty);
        self.code.push(instruction);
    }

    pub(super) fn emit_vector_splat(&mut self, value: &AmirOperand, ty: TypeId) {
        let ArType::Vector(vector) = self.interner.resolve(ty) else {
            self.code.push(Instruction::Unreachable);
            return;
        };
        let lane_ty = self.interner.intern(ArType::Primitive(vector.lane()));
        self.emit_operand(value, lane_ty);
        self.code.push(match vector {
            VectorType::F32x4 => Instruction::F32x4Splat,
            VectorType::F64x2 => Instruction::F64x2Splat,
            VectorType::I32x4 => Instruction::I32x4Splat,
            VectorType::I16x8 => Instruction::I16x8Splat,
            VectorType::U8x16 => Instruction::I8x16Splat,
            VectorType::I64x2 => Instruction::I64x2Splat,
        });
    }

    pub(super) fn emit_vector_extract(&mut self, value: &AmirOperand, lane: u8) {
        let ty = self.operand_arity_ty(value);
        let ArType::Vector(vector) = self.interner.resolve(ty) else {
            self.code.push(Instruction::Unreachable);
            return;
        };
        self.emit_operand(value, ty);
        self.code.push(match vector {
            VectorType::F32x4 => Instruction::F32x4ExtractLane(lane),
            VectorType::F64x2 => Instruction::F64x2ExtractLane(lane),
            VectorType::I32x4 => Instruction::I32x4ExtractLane(lane),
            VectorType::I16x8 => Instruction::I16x8ExtractLaneS(lane),
            VectorType::U8x16 => Instruction::I8x16ExtractLaneU(lane),
            VectorType::I64x2 => Instruction::I64x2ExtractLane(lane),
        });
    }

    pub(super) fn emit_vector_insert(
        &mut self,
        vector: &AmirOperand,
        lane: u8,
        value: &AmirOperand,
        ty: TypeId,
    ) {
        let ArType::Vector(shape) = self.interner.resolve(ty) else {
            self.code.push(Instruction::Unreachable);
            return;
        };
        let lane_ty = self.interner.intern(ArType::Primitive(shape.lane()));
        self.emit_operand(vector, ty);
        self.emit_operand(value, lane_ty);
        self.code.push(match shape {
            VectorType::F32x4 => Instruction::F32x4ReplaceLane(lane),
            VectorType::F64x2 => Instruction::F64x2ReplaceLane(lane),
            VectorType::I32x4 => Instruction::I32x4ReplaceLane(lane),
            VectorType::I16x8 => Instruction::I16x8ReplaceLane(lane),
            VectorType::U8x16 => Instruction::I8x16ReplaceLane(lane),
            VectorType::I64x2 => Instruction::I64x2ReplaceLane(lane),
        });
    }
}
