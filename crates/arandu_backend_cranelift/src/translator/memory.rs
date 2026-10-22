use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_module::FuncId;

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// A result construction can write straight into its caller-owned buffer.
    /// Consume the hint once: nested fields must retain independent storage.
    pub(super) fn allocate_aggregate(&mut self, ty: &arandu_semantics::types::ArType) -> Value {
        match self.aggregate_destination.take() {
            Some(destination) => destination,
            None => self.allocate_independent_aggregate(ty),
        }
    }

    /// Independent by-value storage, never the destination hint of a nested
    /// call. A stack slot belongs to one static materialization site and is
    /// reused across loop iterations; copies have separate slots.
    pub(super) fn allocate_independent_aggregate(
        &mut self,
        ty: &arandu_semantics::types::ArType,
    ) -> Value {
        let layout = self.checked_layout(ty);
        let id = self.type_info.type_interner.intern(ty.clone());
        // A bare slice descriptor is a non-owning `{data, len}` view: its own
        // bytes carry no lifetime (the pointee stays with its owner), so a
        // per-site slot replaces a per-execution malloc. Only a bare slice
        // qualifies; a view hidden inside an aggregate keeps the nested
        // rejection of `scratch_type_safe`. The promotion gate still applies:
        // `Free`/back-edge phi/extern rules that disable promotion for the
        // function must disable it for descriptors too.
        let slice_descriptor = matches!(
            ty,
            arandu_semantics::types::ArType::Slice(_)
                | arandu_semantics::types::ArType::MutSlice(_)
        );
        let eligible = self.frame_promotion_safe
            && (slice_descriptor
                || (arandu_semantics::aggregate_storage::scratch_type_safe(
                    id,
                    &self.type_info.type_interner,
                    self.type_info,
                ) && self
                    .type_info
                    .borrow_paths(id)
                    .is_ok_and(|paths| paths.is_empty())));
        // Ownership/drop elaboration already governs non-Copy payloads. This
        // allocation owns only their backing bytes: moving a value transfers
        // its resources, while each invocation retains its private backing.
        // No safe borrowed leaf may point into another invocation's scratch.
        let size = u32::try_from(layout.size.max(1)).unwrap_or(1);
        let alignment = u32::try_from(layout.align.max(1)).unwrap_or(u32::MAX);
        let used = self
            .builder
            .func
            .sized_stack_slots
            .values()
            .try_fold(0u32, |used, slot| {
                used.checked_add(slot.size)?
                    .checked_add((1u32.checked_shl(u32::from(slot.align_shift))?).saturating_sub(1))
            });
        let reserved = used.and_then(|used| {
            used.checked_add(size)?
                .checked_add(alignment.saturating_sub(1))
        });
        if eligible
            && alignment <= 16
            && alignment.is_power_of_two()
            && reserved.is_some_and(|size| size <= super::storage::FRAME_PROMOTION_BUDGET)
        {
            let slot = self
                .builder
                .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                    kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    size,
                    align_shift: u8::try_from(alignment.trailing_zeros()).unwrap_or(0),
                    key: None,
                });
            return self.builder.ins().stack_addr(self.ptr_type, slot, 0);
        }
        if eligible
            && alignment <= 16
            && self
                .current_func
                .predecessors(arandu_semantics::amir::BlockId::from_usize(0))
                .is_empty()
        {
            return self.allocate_heap_scratch(size);
        }
        self.call_malloc(size)
    }
    /// Copy object bytes into independent destination storage. Use memmove:
    /// returning a parameter can make source and destination overlap.
    pub(super) fn copy_aggregate_bytes(&mut self, destination: Value, source: Value, size: u64) {
        if size == 0 {
            return;
        }
        if i64::try_from(size).is_err() {
            self.record_ice("aggregate copy exceeds instruction range", self.func_span());
            return;
        }
        self.builder.emit_small_memory_copy(
            self.module.target_config(),
            destination,
            source,
            size,
            1,
            1,
            false,
            cranelift_codegen::ir::MemFlagsData::new(),
        );
    }
    /// A register slot may be wider than the last bytes of an aggregate. Pack
    /// only those bytes; a full-width load would read beyond the allocation.
    pub(super) fn load_abi_slot(
        &mut self,
        base: Value,
        slot: &arandu_semantics::layout::AbiSlot,
        aggregate_size: u64,
    ) -> Value {
        let ty = crate::abi::abi_scalar_to_clif(slot.scalar);
        let little = self.module.isa().endianness() == cranelift_codegen::ir::Endianness::Little;
        match crate::abi::load_aggregate_slot(&mut self.builder, base, slot, aggregate_size, little)
        {
            Ok(value) => value,
            Err(message) => {
                self.record_ice(message, self.func_span());
                self.poison_value(ty)
            }
        }
    }

    /// Inverse of `load_abi_slot`, including partial trailing slots. Never
    /// store register padding into bytes belonging to the next object.
    pub(super) fn store_abi_slot(
        &mut self,
        value: Value,
        base: Value,
        slot: &arandu_semantics::layout::AbiSlot,
        aggregate_size: u64,
    ) {
        let Some(bytes) = aggregate_size.checked_sub(slot.offset) else {
            self.record_ice(
                "ABI slot starts outside aggregate storage",
                self.func_span(),
            );
            return;
        };
        let Ok(offset) = i32::try_from(slot.offset) else {
            self.record_ice(
                "ABI slot offset exceeds instruction range",
                self.func_span(),
            );
            return;
        };
        let bytes = bytes.min(slot.scalar.size());
        if bytes == slot.scalar.size() {
            self.builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
                base,
                offset,
            );
            return;
        }
        if !self.builder.func.dfg.value_type(value).is_int() || bytes == 0 {
            self.record_ice("invalid partial ABI register slot", self.func_span());
            return;
        }
        let little = self.module.isa().endianness() == cranelift_codegen::ir::Endianness::Little;
        for byte in 0..bytes {
            let position = if little {
                byte
            } else {
                slot.scalar.size() - 1 - byte
            };
            let shifted = self.builder.ins().ushr_imm_u(value, (position * 8) as i64);
            let value = self
                .builder
                .ins()
                .ireduce(cranelift_codegen::ir::types::I8, shifted);
            self.builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
                base,
                offset + byte as i32,
            );
        }
    }

    pub(super) fn fmod_func_id(&mut self) -> Option<FuncId> {
        match self.resolve_function("fmod") {
            Some(func_id) => Some(func_id),
            None => {
                self.record_ice("fmod was not declared in the JIT module", self.func_span());
                None
            }
        }
    }

    pub(super) fn malloc_func_id(&mut self) -> Option<FuncId> {
        match self.resolve_function("malloc") {
            Some(func_id) => Some(func_id),
            None => {
                self.record_ice(
                    "malloc was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn call_malloc(&mut self, size: u32) -> Value {
        let Some(malloc_func_id) = self.malloc_func_id() else {
            return self.poison_i32();
        };
        let local_ref = self
            .module
            .declare_func_in_func(malloc_func_id, self.builder.func);
        let size_val = self.builder.ins().iconst(self.ptr_type, size.max(1) as i64);
        let call_inst = self.builder.ins().call(local_ref, &[size_val]);
        let ptr = self.builder.inst_results(call_inst)[0];
        self.trap_if_null(ptr);
        ptr
    }

    /// Fail closed when a positive-size runtime allocation cannot be satisfied.
    pub(super) fn trap_if_null(&mut self, ptr: Value) {
        let is_null =
            self.builder
                .ins()
                .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, ptr, 0);
        self.builder
            .ins()
            .trapnz(is_null, cranelift_codegen::ir::TrapCode::unwrap_user(1));
    }

    /// Preserve `malloc(0)` behavior while trapping failed positive-size allocations.
    pub(super) fn trap_if_null_for_nonzero_size(&mut self, ptr: Value, size: Value) {
        let is_null =
            self.builder
                .ins()
                .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, ptr, 0);
        let requested_bytes = self.builder.ins().icmp_imm_u(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            size,
            0,
        );
        let allocation_failed = self.builder.ins().band(is_null, requested_bytes);
        self.builder.ins().trapnz(
            allocation_failed,
            cranelift_codegen::ir::TrapCode::unwrap_user(1),
        );
    }

    pub(super) fn free_func_id(&mut self) -> Option<FuncId> {
        match self.resolve_function("free") {
            Some(func_id) => Some(func_id),
            None => {
                self.record_ice("free was not declared in the JIT module", self.func_span());
                None
            }
        }
    }

    pub(super) fn memcpy_func_id(&mut self) -> Option<FuncId> {
        match self.resolve_function("memcpy") {
            Some(func_id) => Some(func_id),
            None => {
                self.record_ice(
                    "memcpy was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn memmove_func_id(&mut self) -> Option<FuncId> {
        match self.resolve_function("memmove") {
            Some(func_id) => Some(func_id),
            None => {
                self.record_ice(
                    "memmove was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn memcmp_func_id(&mut self) -> Option<FuncId> {
        match self.resolve_function("memcmp") {
            Some(func_id) => Some(func_id),
            None => {
                self.record_ice(
                    "memcmp was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn emit_free_ptr(&mut self, ptr_val: Value) {
        let Some(free_func_id) = self.free_func_id() else {
            return;
        };
        let is_null = self.builder.ins().icmp_imm_u(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            ptr_val,
            0,
        );
        let free_block = self.builder.create_block();
        let cont_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(is_null, cont_block, &[], free_block, &[]);
        self.builder.switch_to_block(free_block);
        self.builder.seal_block(free_block);

        let local_ref = self
            .module
            .declare_func_in_func(free_func_id, self.builder.func);

        #[cfg(debug_assertions)]
        {
            let poison_val = self.builder.ins().iconst(self.ptr_type, 0xDE_i64);
            self.builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                poison_val,
                ptr_val,
                0,
            );
        }

        self.builder.ins().call(local_ref, &[ptr_val]);
        self.builder.ins().jump(cont_block, &[]);
        self.builder.switch_to_block(cont_block);
        self.builder.seal_block(cont_block);
    }
}
