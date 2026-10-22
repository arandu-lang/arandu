use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, AmirRvalue};
use arandu_middle::types::{ArType, Primitive};

use super::super::{FuncEmitter, TypedValue, ice};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(in crate::codegen) fn enum_storage(
        &mut self,
        value: &AmirOperand,
    ) -> Result<(String, ArType), Diagnostic> {
        let base_ty = self.module.operand_type(value, self.func);
        match &base_ty {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                let owner = self.module.interner.resolve(*inner);
                let pointer = self.operand(value, &base_ty)?;
                Ok((pointer.value, owner))
            }
            _ => {
                let address = self.aggregate_address(value, &base_ty)?;
                Ok((address, base_ty))
            }
        }
    }

    pub(in crate::codegen) fn emit_coroutine_ready(
        &mut self,
        result: &str,
        value: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
    ) -> Result<(), Diagnostic> {
        let payload_ar = self.module.interner.resolve(payload_ty);
        let payload_layout = self
            .module
            .layout
            .layout_of_type(&payload_ar, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {payload_ar:?}: {error}")))?;
        // Header is 8 bytes; host poll/block_on helpers read an i64 payload, so
        // keep at least 16 bytes (matches Cranelift).
        let size = (8 + payload_layout.size.max(1)).max(16);
        self.module.require_raw_malloc();
        let pointer = self.new_value();
        self.writer.line(format!(
            "{pointer} = call ptr @ar_rt_raw_malloc({} {size})",
            self.module.ptr_int_ty()
        ));
        let null = self.new_value();
        self.writer
            .line(format!("{null} = icmp eq ptr {pointer}, null"));
        let ok = self.new_value();
        self.writer.line(format!("{ok} = xor i1 {null}, true"));
        let trap = self.new_block();
        let continue_block = self.new_block();
        self.writer.line(format!(
            "br i1 {ok}, label %{continue_block}, label %{trap}"
        ));
        self.enter_llvm_label(&trap);
        self.module.require_llvm_trap();
        self.writer.line("call void @llvm.trap()");
        self.writer.line("unreachable");
        self.enter_llvm_label(&continue_block);
        self.writer
            .line(format!("store i32 0, ptr {pointer}, align 4"));
        let magic = self.byte_gep(&pointer, 4);
        // CO_MAGIC = 0x4152434F ("ARCO"); must match `poll_runtime::CO_MAGIC`.
        self.writer.line(format!(
            "store i32 {}, ptr {magic}, align 4",
            0x4152_434fu32
        ));
        if !matches!(payload_ar, ArType::Void) {
            let payload = self.operand(value, &payload_ar)?;
            let payload_address = self.byte_gep(&pointer, 8);
            // Widen integer payloads to i64 so `ar_co_block_on_i64` can read them.
            let stored = if self.module.integer_width(&payload_ar).is_some() {
                self.coerce(payload, &ArType::Primitive(Primitive::I64))?
            } else {
                payload
            };
            let stored_ty = if self.module.integer_width(&payload_ar).is_some() {
                ArType::Primitive(Primitive::I64)
            } else {
                payload_ar.clone()
            };
            self.store_value(&payload_address, &stored, &stored_ty)?;
        }
        self.assign_coerced(
            result,
            TypedValue::new("ptr".to_owned(), pointer),
            &ArType::Ptr(self.module.interner.intern(ArType::Void)),
        )
    }

    pub(in crate::codegen) fn emit_gen_operation(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
    ) -> Result<(), Diagnostic> {
        match rvalue {
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let value = self.operand(value, &payload_ar)?;
                self.store_value(&slot, &value, &payload_ar)?;
                self.module.require_gen_helper("ar_gen_insert_raw")?;
                let inserted = self.new_value();
                self.writer.line(format!(
                    "{inserted} = call i64 @ar_gen_insert_raw(ptr {slot}, i64 {}, i64 {}, ptr null)",
                    layout.size, layout.align
                ));
                self.assign_coerced(result, TypedValue::new("i64".to_owned(), inserted), lhs_ty)
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            }
            | AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let handle = self.operand(gen_ref, &ArType::Primitive(Primitive::I64))?;
                let name = if matches!(rvalue, AmirRvalue::GenGet { .. }) {
                    "ar_gen_get_raw"
                } else {
                    "ar_gen_remove_raw"
                };
                self.module.require_gen_helper(name)?;
                let ok = self.new_value();
                self.writer.line(format!(
                    "{ok} = call i1 @{name}(i64 {}, ptr {slot}, i64 {}, i64 {})",
                    handle.value, layout.size, layout.align
                ));
                let fail = self.new_block();
                let cont = self.new_block();
                self.writer
                    .line(format!("br i1 {ok}, label %{cont}, label %{fail}"));
                self.enter_llvm_label(&fail);
                self.module.require_llvm_trap();
                self.writer.line("call void @llvm.trap()");
                self.writer.line("unreachable");
                self.enter_llvm_label(&cont);
                let payload_text = self.module.llvm_ty(&payload_ar)?;
                let align = self.module.align_of(&payload_ar);
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {payload_text}, ptr {slot}, align {align}"
                ));
                self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty)
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            }
            | AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                let upsert = matches!(rvalue, AmirRvalue::GenUpsert { .. });
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let value = self.operand(value, &payload_ar)?;
                self.store_value(&slot, &value, &payload_ar)?;
                let handle = self.operand(gen_ref, &ArType::Primitive(Primitive::I64))?;
                if upsert {
                    self.module.require_gen_helper("ar_gen_upsert_raw")?;
                    let updated = self.new_value();
                    self.writer.line(format!(
                        "{updated} = call i64 @ar_gen_upsert_raw(i64 {}, ptr {slot}, i64 {}, i64 {}, ptr null)",
                        handle.value, layout.size, layout.align
                    ));
                    self.assign_coerced(result, TypedValue::new("i64".to_owned(), updated), lhs_ty)
                } else {
                    self.module.require_gen_helper("ar_gen_set_raw")?;
                    let ok = self.new_value();
                    self.writer.line(format!(
                        "{ok} = call i1 @ar_gen_set_raw(i64 {}, ptr {slot}, i64 {}, i64 {}, ptr null)",
                        handle.value, layout.size, layout.align
                    ));
                    let fail = self.new_block();
                    let cont = self.new_block();
                    self.writer
                        .line(format!("br i1 {ok}, label %{cont}, label %{fail}"));
                    self.enter_llvm_label(&fail);
                    self.module.require_llvm_trap();
                    self.writer.line("call void @llvm.trap()");
                    self.writer.line("unreachable");
                    self.enter_llvm_label(&cont);
                    self.assign_coerced(result, handle, lhs_ty)
                }
            }
            _ => Err(ice("unexpected GenRef operation")),
        }
    }

    pub(in crate::codegen) fn construct_aggregate(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        fields: &[(u64, ArType, &AmirOperand)],
    ) -> Result<(), Diagnostic> {
        let slot = self.scratch_alloca(lhs_ty)?;
        let aggregate = self.module.llvm_ty(lhs_ty)?;
        let align = self.module.align_of(lhs_ty);
        self.writer.line(format!(
            "store {aggregate} zeroinitializer, ptr {slot}, align {align}"
        ));
        for (offset, field_ty, operand) in fields {
            let address = self.byte_gep(&slot, *offset);
            let value = self.operand(operand, field_ty)?;
            self.store_value(&address, &value, field_ty)?;
        }
        self.writer.line(format!(
            "{result} = load {aggregate}, ptr {slot}, align {align}"
        ));
        Ok(())
    }

    pub(in crate::codegen) fn new_value(&mut self) -> String {
        let value = format!("%v{}", self.next_value);
        self.next_value += 1;
        value
    }

    /// Fresh basic-block label without a leading `%`.
    ///
    /// Callers must write `label %{name}` / `{name}:`. Using [`Self::new_value`]
    /// here would emit `%%vN` because that helper already includes `%`.
    pub(in crate::codegen) fn new_block(&mut self) -> String {
        let name = format!("loc{}", self.next_value);
        self.next_value += 1;
        name
    }
}
