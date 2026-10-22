//! Scalar atomics use compiler builtins, without runtime or allocator imports.
use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, TempId};
use arandu_middle::intrinsics::{AtomicOp, AtomicWidth, IntrinsicKind};
use std::fmt::Write;

impl CEmitter<'_> {
    pub(super) fn emit_atomic_intrinsic(
        &mut self,
        kind: IntrinsicKind,
        lhs: &Option<TempId>,
        args: &[AmirOperand],
        func: &AmirFunc,
    ) -> bool {
        let (op, width) = match kind {
            IntrinsicKind::Atomic(op, width) => (op, width),
            IntrinsicKind::AtomicFence | IntrinsicKind::AtomicCompilerFence => {
                let builtin = if kind == IntrinsicKind::AtomicFence {
                    "thread"
                } else {
                    "signal"
                };
                let _ = writeln!(
                    self.output,
                    "    __atomic_{builtin}_fence(__ATOMIC_SEQ_CST);"
                );
                return true;
            }
            _ => return false,
        };
        let ty = match width {
            AtomicWidth::U8 => "uint8_t",
            AtomicWidth::U32 => "uint32_t",
            AtomicWidth::U64 => "uint64_t",
            AtomicWidth::Usize => "uintptr_t",
            AtomicWidth::Pointer => "uint8_t *",
        };
        if args.len() != op.argument_count() {
            self.record_codegen_ice(func, "invalid atomic intrinsic ABI");
            return true;
        }
        let _ = writeln!(
            self.output,
            "    _Static_assert(__atomic_always_lock_free(sizeof({ty}), 0), \"atomic width requires lock-free target support\");"
        );
        let pointer = format!("({ty} *)({})", self.format_operand(&args[0], func));
        let value = args
            .get(1)
            .map(|arg| format!("({ty})({})", self.format_operand(arg, func)));
        let expression = match op {
            AtomicOp::Load => format!("__atomic_load_n({pointer}, __ATOMIC_SEQ_CST)"),
            AtomicOp::Store => format!(
                "__atomic_store_n({pointer}, {}, __ATOMIC_SEQ_CST)",
                value.as_deref().unwrap_or("0")
            ),
            AtomicOp::CompareExchange => {
                let expected = value.as_deref().unwrap_or("0");
                let desired = self.format_operand(&args[2], func);
                // Expected is an lvalue modified by the builtin on failure.
                // The statement expression returns the observation on both paths.
                format!(
                    "({{ {ty} ar_expected = {expected}; __atomic_compare_exchange_n({pointer}, &ar_expected, ({ty})({desired}), 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); ar_expected; }})"
                )
            }
            AtomicOp::Swap
            | AtomicOp::Add
            | AtomicOp::Sub
            | AtomicOp::And
            | AtomicOp::Or
            | AtomicOp::Xor => {
                let builtin = match op {
                    AtomicOp::Swap => "exchange_n",
                    AtomicOp::Add => "fetch_add",
                    AtomicOp::Sub => "fetch_sub",
                    AtomicOp::And => "fetch_and",
                    AtomicOp::Or => "fetch_or",
                    AtomicOp::Xor => "fetch_xor",
                    AtomicOp::Load => "load_n",
                    AtomicOp::Store => "store_n",
                    AtomicOp::CompareExchange => "compare_exchange_n",
                };
                format!(
                    "__atomic_{builtin}({pointer}, {}, __ATOMIC_SEQ_CST)",
                    value.as_deref().unwrap_or("0")
                )
            }
        };
        if let Some(lhs) = lhs
            && op != AtomicOp::Store
        {
            let _ = writeln!(
                self.output,
                "    t{} = {expression};",
                self.temp_slots[lhs.as_usize()]
            );
        } else {
            let _ = writeln!(self.output, "    (void)({expression});");
        }
        true
    }
}
