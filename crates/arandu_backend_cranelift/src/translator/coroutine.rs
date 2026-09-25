use arandu_semantics::amir::AmirOperand;
use cranelift_codegen::ir::{InstBuilder, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// A3.0/A3.3/A3.6: state blob with disc@0 and payload@8.
    /// `stack == true` → stack slot (zero-heap); else `malloc` (escaping / returned).
    pub(super) fn translate_coroutine_ready(
        &mut self,
        value: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
        stack: bool,
    ) -> Value {
        use cranelift_codegen::ir::types::I32;
        let payload_ar = self.type_info.resolve_type_id(payload_ty);
        let layout = self.checked_layout(&payload_ar);
        // Header 8 bytes (disc u32 + pad) + payload.
        let size = (8 + layout.size).max(16);
        let align = layout.align.max(8);
        let align_shift = align.trailing_zeros() as u8;

        let ptr_val = if stack {
            // A3.3 stack-first: task state lives in the creator's frame.
            let slot = self
                .builder
                .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                    kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    size: size as u32,
                    align_shift,
                    key: None,
                });
            self.builder.ins().stack_addr(self.ptr_type, slot, 0)
        } else {
            let Some(malloc_id) = self.malloc_func_id() else {
                return self.poison_i32();
            };
            let malloc_ref = self
                .module
                .declare_func_in_func(malloc_id, self.builder.func);
            let size_val = self.builder.ins().iconst(self.ptr_type, size as i64);
            let call = self.builder.ins().call(malloc_ref, &[size_val]);
            let ptr = self.builder.inst_results(call)[0];
            self.trap_if_null(ptr);
            ptr
        };

        // disc = 0 (Ready)
        let zero = self.builder.ins().iconst(I32, 0);
        self.builder
            .ins()
            .store(cranelift_codegen::ir::MemFlagsData::new(), zero, ptr_val, 0);

        // magic = CO_MAGIC (0x4152434f) at offset 4
        let magic = self.builder.ins().iconst(I32, 0x4152434f);
        self.builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            magic,
            ptr_val,
            4,
        );

        let clif_ty = match crate::types::clif_type(&payload_ar, self.ptr_type) {
            crate::types::ClifType::Concrete(t) => t,
            crate::types::ClifType::Void => {
                return ptr_val;
            }
        };
        let payload_val = self.translate_operand(value, Some(clif_ty));
        // payload at +8
        self.builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            payload_val,
            ptr_val,
            8,
        );
        ptr_val
    }

    /// A3.6: `await co` → drive until Ready, return payload.
    ///
    /// Fast path: disc==0 → load payload@+8.
    /// Slow path: `ar_co_block_on_i64` (handles PendingOnce and future disc values).
    pub(super) fn translate_await_block_on(
        &mut self,
        operand: &AmirOperand,
        expected_ty: Option<cranelift_codegen::ir::Type>,
    ) -> Value {
        use cranelift_codegen::ir::types::{I32, I64};
        let ptr = self.translate_operand(operand, Some(self.ptr_type));
        let load_ty = expected_ty.unwrap_or(I64);

        // Fast path for Ready (disc == 0) without host call.
        let disc = self
            .builder
            .ins()
            .load(I32, cranelift_codegen::ir::MemFlagsData::new(), ptr, 0);
        let zero = self.builder.ins().iconst(I32, 0);
        let is_ready =
            self.builder
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, disc, zero);

        let ready_bb = self.builder.create_block();
        let slow_bb = self.builder.create_block();
        let join_bb = self.builder.create_block();
        self.builder.append_block_param(join_bb, load_ty);

        self.builder
            .ins()
            .brif(is_ready, ready_bb, &[], slow_bb, &[]);

        self.builder.switch_to_block(ready_bb);
        self.builder.seal_block(ready_bb);
        let ready_val =
            self.builder
                .ins()
                .load(load_ty, cranelift_codegen::ir::MemFlagsData::new(), ptr, 8);
        self.builder.ins().jump(
            join_bb,
            &[cranelift_codegen::ir::BlockArg::Value(ready_val)],
        );

        self.builder.switch_to_block(slow_bb);
        self.builder.seal_block(slow_bb);
        // Host block_on always returns i64; cast/truncate to load_ty if needed.
        let slow_i64 = self.call_co_block_on(ptr);
        let slow_val = if load_ty == I64 {
            slow_i64
        } else if load_ty.bits() < 64 {
            self.builder.ins().ireduce(load_ty, slow_i64)
        } else {
            slow_i64
        };
        self.builder
            .ins()
            .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(slow_val)]);

        self.builder.switch_to_block(join_bb);
        self.builder.seal_block(join_bb);
        self.builder.block_params(join_bb)[0]
    }

    pub(super) fn call_co_block_on(&mut self, state_ptr: Value) -> Value {
        use cranelift_codegen::ir::types::I64;
        let Some(&func_id) = self.func_ids.get("ar_co_block_on_i64") else {
            self.record_ice("ar_co_block_on_i64 not declared", self.func_span());
            return self.builder.ins().iconst(I64, 0);
        };
        let fref = self.module.declare_func_in_func(func_id, self.builder.func);
        let call = self.builder.ins().call(fref, &[state_ptr]);
        self.builder.inst_results(call)[0]
    }
}
