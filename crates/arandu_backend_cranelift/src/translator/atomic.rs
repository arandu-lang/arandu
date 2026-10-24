//! Cranelift atomics are sequentially consistent, including weaker source orders.
use super::FunctionTranslator;
use arandu_middle::amir::{AmirOperand, TempId};
use arandu_middle::intrinsics::{AtomicOp, AtomicWidth, IntrinsicKind};
use cranelift_codegen::ir::{AtomicRmwOp, InstBuilder, MemFlagsData, types};

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_atomic_intrinsic(
        &mut self,
        kind: IntrinsicKind,
        lhs: &Option<TempId>,
        args: &[AmirOperand],
    ) -> bool {
        let (op, width) = match kind {
            IntrinsicKind::Atomic(op, width) => (op, width),
            IntrinsicKind::AtomicFence | IntrinsicKind::AtomicCompilerFence => {
                self.builder.ins().fence();
                return true;
            }
            _ => return false,
        };
        if args.len() != op.argument_count() {
            self.record_ice("invalid atomic intrinsic ABI", self.func_span());
            return true;
        }
        let ty = match width {
            AtomicWidth::U8 => types::I8,
            AtomicWidth::U32 => types::I32,
            AtomicWidth::U64 => types::I64,
            AtomicWidth::Usize | AtomicWidth::Pointer => self.ptr_type,
        };
        let pointer = self.translate_operand(&args[0], Some(self.ptr_type));
        let flags = MemFlagsData::new();
        let value = if op == AtomicOp::Load {
            self.builder.ins().atomic_load(ty, flags, pointer)
        } else {
            let input = self.translate_operand(&args[1], Some(ty));
            match op {
                AtomicOp::Store => {
                    self.builder.ins().atomic_store(flags, input, pointer);
                    return true;
                }
                AtomicOp::CompareExchange => {
                    let desired = self.translate_operand(&args[2], Some(ty));
                    self.builder
                        .ins()
                        .atomic_cas(flags, pointer, input, desired)
                }
                AtomicOp::Load
                | AtomicOp::Swap
                | AtomicOp::Add
                | AtomicOp::Sub
                | AtomicOp::And
                | AtomicOp::Or
                | AtomicOp::Xor => {
                    let operation = match op {
                        AtomicOp::Add => AtomicRmwOp::Add,
                        AtomicOp::Sub => AtomicRmwOp::Sub,
                        AtomicOp::And => AtomicRmwOp::And,
                        AtomicOp::Or => AtomicRmwOp::Or,
                        AtomicOp::Xor => AtomicRmwOp::Xor,
                        AtomicOp::Swap | AtomicOp::Load => AtomicRmwOp::Xchg,
                        AtomicOp::Store | AtomicOp::CompareExchange => {
                            self.record_ice("invalid atomic RMW operation", self.func_span());
                            return true;
                        }
                    };
                    self.builder
                        .ins()
                        .atomic_rmw(ty, flags, operation, pointer, input)
                }
            }
        };
        if let Some(destination) = lhs
            && let Some(&variable) = self.temp_map.get(destination)
        {
            self.builder.def_var(variable, value);
        }
        true
    }
}
