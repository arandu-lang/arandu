use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_module::FuncId;

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn fmod_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("fmod") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice("fmod was not declared in the JIT module", self.func_span());
                None
            }
        }
    }

    pub(super) fn malloc_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("malloc") {
            Some(func_id) => Some(*func_id),
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
        self.builder.inst_results(call_inst)[0]
    }

    pub(super) fn free_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("free") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice("free was not declared in the JIT module", self.func_span());
                None
            }
        }
    }

    pub(super) fn memcpy_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("memcpy") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice(
                    "memcpy was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn memcmp_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("memcmp") {
            Some(func_id) => Some(*func_id),
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
