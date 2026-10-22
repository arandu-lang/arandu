//! Native Wasm atomic instructions; the module still owns unshared memory.
use super::FuncTranslator;
use arandu_middle::amir::{AmirOperand, TempId};
use arandu_middle::intrinsics::{AtomicOp, AtomicWidth, IntrinsicKind};
use arandu_middle::types::{ArType, Primitive};
use wasm_encoder::{Instruction, MemArg};

impl FuncTranslator<'_> {
    pub(super) fn emit_atomic_intrinsic(
        &mut self,
        kind: IntrinsicKind,
        lhs: Option<TempId>,
        args: &[AmirOperand],
    ) -> bool {
        let (op, width) = match kind {
            IntrinsicKind::Atomic(op, width) => (op, width),
            IntrinsicKind::AtomicFence | IntrinsicKind::AtomicCompilerFence => {
                self.code.push(Instruction::AtomicFence);
                return true;
            }
            _ => return false,
        };
        if args.len() != op.argument_count() {
            self.code.push(Instruction::Unreachable);
            return true;
        }
        let input_ty = self.interner.intern(ArType::Primitive(width.primitive()));
        let pointer_ty = self.interner.intern(ArType::Primitive(Primitive::USize));
        self.emit_operand(&args[0], pointer_ty);
        for argument in &args[1..] {
            self.emit_operand(argument, input_ty);
        }
        let memory = MemArg {
            offset: 0,
            align: match width {
                AtomicWidth::U8 => 0,
                AtomicWidth::U64 => 3,
                AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer => 2,
            },
            memory_index: 0,
        };
        let instruction = match (op, width) {
            (AtomicOp::Load, AtomicWidth::U8) => Instruction::I32AtomicLoad8U(memory),
            (AtomicOp::Load, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicLoad(memory)
            }
            (AtomicOp::Load, AtomicWidth::U64) => Instruction::I64AtomicLoad(memory),
            (AtomicOp::Store, AtomicWidth::U8) => Instruction::I32AtomicStore8(memory),
            (AtomicOp::Store, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicStore(memory)
            }
            (AtomicOp::Store, AtomicWidth::U64) => Instruction::I64AtomicStore(memory),
            (AtomicOp::Swap, AtomicWidth::U8) => Instruction::I32AtomicRmw8XchgU(memory),
            (AtomicOp::Swap, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicRmwXchg(memory)
            }
            (AtomicOp::Swap, AtomicWidth::U64) => Instruction::I64AtomicRmwXchg(memory),
            (AtomicOp::Add, AtomicWidth::U8) => Instruction::I32AtomicRmw8AddU(memory),
            (AtomicOp::Add, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicRmwAdd(memory)
            }
            (AtomicOp::Add, AtomicWidth::U64) => Instruction::I64AtomicRmwAdd(memory),
            (AtomicOp::Sub, AtomicWidth::U8) => Instruction::I32AtomicRmw8SubU(memory),
            (AtomicOp::Sub, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicRmwSub(memory)
            }
            (AtomicOp::Sub, AtomicWidth::U64) => Instruction::I64AtomicRmwSub(memory),
            (AtomicOp::And, AtomicWidth::U8) => Instruction::I32AtomicRmw8AndU(memory),
            (AtomicOp::And, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicRmwAnd(memory)
            }
            (AtomicOp::And, AtomicWidth::U64) => Instruction::I64AtomicRmwAnd(memory),
            (AtomicOp::Or, AtomicWidth::U8) => Instruction::I32AtomicRmw8OrU(memory),
            (AtomicOp::Or, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicRmwOr(memory)
            }
            (AtomicOp::Or, AtomicWidth::U64) => Instruction::I64AtomicRmwOr(memory),
            (AtomicOp::Xor, AtomicWidth::U8) => Instruction::I32AtomicRmw8XorU(memory),
            (AtomicOp::Xor, AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer) => {
                Instruction::I32AtomicRmwXor(memory)
            }
            (AtomicOp::Xor, AtomicWidth::U64) => Instruction::I64AtomicRmwXor(memory),
            (AtomicOp::CompareExchange, AtomicWidth::U8) => {
                Instruction::I32AtomicRmw8CmpxchgU(memory)
            }
            (
                AtomicOp::CompareExchange,
                AtomicWidth::U32 | AtomicWidth::Usize | AtomicWidth::Pointer,
            ) => Instruction::I32AtomicRmwCmpxchg(memory),
            (AtomicOp::CompareExchange, AtomicWidth::U64) => {
                Instruction::I64AtomicRmwCmpxchg(memory)
            }
        };
        self.code.push(instruction);
        if op != AtomicOp::Store {
            if let Some(destination) = lhs
                && let Some(&local) = self.temp_local.get(&destination)
            {
                self.code.push(Instruction::LocalSet(local));
            } else {
                self.code.push(Instruction::Drop);
            }
        }
        true
    }
}
