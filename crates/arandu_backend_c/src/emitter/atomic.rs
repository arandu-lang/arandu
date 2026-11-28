//! Scalar atomics use compiler builtins, without runtime or allocator imports.
use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, TempId};
use arandu_middle::intrinsics::{AtomicOp, AtomicOrdering, AtomicWidth, IntrinsicKind};
use std::fmt::Write;

impl CEmitter<'_> {
    pub(super) fn emit_atomic_intrinsic(
        &mut self,
        kind: IntrinsicKind,
        lhs: &Option<TempId>,
        args: &[AmirOperand],
        func: &AmirFunc,
    ) -> bool {
        let (op, width, ordering) = match kind {
            IntrinsicKind::Atomic(op, width, ordering) => (op, width, ordering),
            IntrinsicKind::AtomicFence(ordering) | IntrinsicKind::AtomicCompilerFence(ordering) => {
                let builtin = if matches!(kind, IntrinsicKind::AtomicFence(..)) {
                    "thread"
                } else {
                    "signal"
                };
                let c_order = match ordering {
                    AtomicOrdering::Relaxed => "__ATOMIC_RELAXED",
                    AtomicOrdering::Acquire => "__ATOMIC_ACQUIRE",
                    AtomicOrdering::Release => "__ATOMIC_RELEASE",
                    AtomicOrdering::AcqRel => "__ATOMIC_ACQ_REL",
                    AtomicOrdering::SeqCst => "__ATOMIC_SEQ_CST",
                };
                let _ = writeln!(self.output, "    __atomic_{builtin}_fence({c_order});");
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
        let load_order = match ordering {
            AtomicOrdering::Relaxed => "__ATOMIC_RELAXED",
            AtomicOrdering::Acquire | AtomicOrdering::AcqRel => "__ATOMIC_ACQUIRE",
            _ => "__ATOMIC_SEQ_CST",
        };
        let store_order = match ordering {
            AtomicOrdering::Relaxed => "__ATOMIC_RELAXED",
            AtomicOrdering::Release | AtomicOrdering::AcqRel => "__ATOMIC_RELEASE",
            _ => "__ATOMIC_SEQ_CST",
        };
        let rmw_order = match ordering {
            AtomicOrdering::Relaxed => "__ATOMIC_RELAXED",
            AtomicOrdering::Acquire => "__ATOMIC_ACQUIRE",
            AtomicOrdering::Release => "__ATOMIC_RELEASE",
            AtomicOrdering::AcqRel => "__ATOMIC_ACQ_REL",
            AtomicOrdering::SeqCst => "__ATOMIC_SEQ_CST",
        };
        let expression = match op {
            AtomicOp::Load => format!("__atomic_load_n({pointer}, {load_order})"),
            AtomicOp::Store => format!(
                "__atomic_store_n({pointer}, {}, {store_order})",
                value.as_deref().unwrap_or("0")
            ),
            AtomicOp::CompareExchange => {
                let expected = value.as_deref().unwrap_or("0");
                let desired = self.format_operand(&args[2], func);
                let (cas_succ, cas_fail) = match ordering {
                    AtomicOrdering::Relaxed => ("__ATOMIC_RELAXED", "__ATOMIC_RELAXED"),
                    AtomicOrdering::Acquire => ("__ATOMIC_ACQUIRE", "__ATOMIC_ACQUIRE"),
                    AtomicOrdering::Release => ("__ATOMIC_RELEASE", "__ATOMIC_RELAXED"),
                    AtomicOrdering::AcqRel => ("__ATOMIC_ACQ_REL", "__ATOMIC_ACQUIRE"),
                    AtomicOrdering::SeqCst => ("__ATOMIC_SEQ_CST", "__ATOMIC_SEQ_CST"),
                };
                // Expected is an lvalue modified by the builtin on failure.
                // The statement expression returns the observation on both paths.
                format!(
                    "({{ {ty} ar_expected = {expected}; __atomic_compare_exchange_n({pointer}, &ar_expected, ({ty})({desired}), 0, {cas_succ}, {cas_fail}); ar_expected; }})"
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
                    "__atomic_{builtin}({pointer}, {}, {rmw_order})",
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
