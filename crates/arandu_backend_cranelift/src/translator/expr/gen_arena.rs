//! Generational arena host interop and storage layout.

use arandu_semantics::amir::AmirOperand;
use arandu_semantics::passes::type_checker::types::ArType;
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    fn gen_payload_slot(
        &mut self,
        payload_ty: arandu_semantics::types::TypeId,
    ) -> Option<(cranelift_codegen::ir::StackSlot, Type, u64, u64, bool)> {
        let ar_ty = self.type_info.resolve_type_id(payload_ty);
        let has_destructor = self
            .type_info
            .destructor_instances
            .contains_key(&payload_ty);
        if !self.type_info.is_copy(payload_ty) && !has_destructor {
            self.record_ice(
                "non-Copy GenRef payload has no explicit @Destructor contract",
                self.func_span(),
            );
            return None;
        }
        let layout = self.checked_layout(&ar_ty);
        let clif_ty = match crate::types::clif_type(&ar_ty, self.ptr_type) {
            crate::types::ClifType::Concrete(ty) => ty,
            crate::types::ClifType::Void => {
                self.record_ice("void GenRef payload", self.func_span());
                return None;
            }
        };
        let Ok(size) = u32::try_from(layout.size.max(1)) else {
            self.record_ice("GenRef payload exceeds stack-slot limit", self.func_span());
            return None;
        };
        let slot = self
            .builder
            .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                size,
                align_shift: layout.align.max(1).trailing_zeros() as u8,
                key: None,
            });
        let indirect = matches!(
            ar_ty,
            ArType::Named(_, _)
                | ArType::Array(_, _)
                | ArType::Tuple(_)
                | ArType::Option(_)
                | ArType::Result(_, _)
                | ArType::Coroutine(_)
                | ArType::Poll(_)
                | ArType::Range(_)
        );
        Some((slot, clif_ty, layout.size, layout.align, indirect))
    }

    fn gen_layout_args(
        &mut self,
        payload_ty: arandu_semantics::types::TypeId,
        size: u64,
        align: u64,
    ) -> (Value, Value, Value) {
        let drop_glue = match self.type_info.resolve_type_id(payload_ty) {
            ArType::Named(_, _) => self
                .type_info
                .destructor_instances
                .get(&payload_ty)
                .and_then(|destructor| {
                    let name =
                        crate::cgu::dependencies::drop_shim_name(self.symbol_table, *destructor)?;
                    self.resolve_function(&name)
                })
                .map(|id| {
                    let function = self.module.declare_func_in_func(id, self.builder.func);
                    self.builder.ins().func_addr(self.ptr_type, function)
                })
                .unwrap_or_else(|| self.builder.ins().iconst(self.ptr_type, 0)),
            _ => self.builder.ins().iconst(self.ptr_type, 0),
        };
        (
            self.builder.ins().iconst(self.ptr_type, size as i64),
            self.builder.ins().iconst(self.ptr_type, align as i64),
            drop_glue,
        )
    }

    pub(super) fn translate_gen_insert(
        &mut self,
        value: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
    ) -> Value {
        let Some((slot, clif_ty, size, align, indirect)) = self.gen_payload_slot(payload_ty) else {
            return self.poison_i32();
        };
        let payload = self.translate_operand(value, Some(clif_ty));
        let address = if indirect {
            payload
        } else {
            self.builder
                .ins()
                .stack_store(self.ptr_type, payload, slot, 0);
            self.builder.ins().stack_addr(self.ptr_type, slot, 0)
        };
        let (size, align, drop_glue) = self.gen_layout_args(payload_ty, size, align);
        let Some(id) = self.resolve_function("ar_gen_insert_raw") else {
            self.record_ice("missing ar_gen_insert_raw runtime import", self.func_span());
            return self.poison_i32();
        };
        let function = self.module.declare_func_in_func(id, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(function, &[address, size, align, drop_glue]);
        self.builder.inst_results(call)[0]
    }

    pub(super) fn translate_gen_read(
        &mut self,
        name: &str,
        gen_ref: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
    ) -> Value {
        use cranelift_codegen::ir::{TrapCode, types::I64};
        let Some((slot, clif_ty, size, align, indirect)) = self.gen_payload_slot(payload_ty) else {
            return self.poison_i32();
        };
        let handle = self.translate_operand(gen_ref, Some(I64));
        let address = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
        let (size, align, _) = self.gen_layout_args(payload_ty, size, align);
        let Some(id) = self.resolve_function(name) else {
            self.record_ice(format!("missing {name} runtime import"), self.func_span());
            return self.poison_i32();
        };
        let function = self.module.declare_func_in_func(id, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(function, &[handle, address, size, align]);
        let succeeded = self.builder.inst_results(call)[0];
        self.builder
            .ins()
            .trapz(succeeded, TrapCode::unwrap_user(2));
        if indirect {
            address
        } else {
            self.builder
                .ins()
                .stack_load(self.ptr_type, clif_ty, slot, 0)
        }
    }

    pub(super) fn translate_gen_write(
        &mut self,
        name: &str,
        gen_ref: &AmirOperand,
        value: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
        returns_handle: bool,
    ) -> Value {
        use cranelift_codegen::ir::{TrapCode, types::I64};
        let Some((slot, clif_ty, size, align, indirect)) = self.gen_payload_slot(payload_ty) else {
            return self.poison_i32();
        };
        let handle = self.translate_operand(gen_ref, Some(I64));
        let payload = self.translate_operand(value, Some(clif_ty));
        let address = if indirect {
            payload
        } else {
            self.builder
                .ins()
                .stack_store(self.ptr_type, payload, slot, 0);
            self.builder.ins().stack_addr(self.ptr_type, slot, 0)
        };
        let (size, align, drop_glue) = self.gen_layout_args(payload_ty, size, align);
        let Some(id) = self.resolve_function(name) else {
            self.record_ice(format!("missing {name} runtime import"), self.func_span());
            return self.poison_i32();
        };
        let function = self.module.declare_func_in_func(id, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(function, &[handle, address, size, align, drop_glue]);
        let result = self.builder.inst_results(call)[0];
        self.builder.ins().trapz(result, TrapCode::unwrap_user(2));
        if returns_handle { result } else { handle }
    }
}
