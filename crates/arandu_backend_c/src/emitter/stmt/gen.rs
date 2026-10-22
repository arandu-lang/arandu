use super::super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, TempId};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_gen_write_assign(
        &mut self,
        lhs: TempId,
        handle: Option<&AmirOperand>,
        value: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
        func: &AmirFunc,
        upsert: bool,
    ) {
        let payload_ar = self.interner.resolve(payload_ty);
        let drop_glue = self
            .gen_drop_glue(payload_ty, &payload_ar)
            .map(|(name, _)| name);
        if !self
            .provider
            .is_copy_type(payload_ty)
            .unwrap_or_else(|| payload_ar.is_copy_v01())
            && drop_glue.is_none()
        {
            self.record_codegen_ice(
                func,
                "non-Copy GenRef payload has no explicit @Destructor contract",
            );
            let _ = writeln!(
                &mut self.output,
                "    t{} = 0;",
                self.temp_slots[lhs.as_usize()]
            );
            return;
        }
        let payload_c = self.format_type(&payload_ar);
        let layout = self.checked_layout(&payload_ar);
        let value = self.format_operand(value, func);
        let glue = drop_glue.as_deref().unwrap_or("NULL");
        let slot = self.next_co_stack_slot();
        let _ = writeln!(
            &mut self.output,
            "    {payload_c} __ar_gen_payload_{slot} = ({payload_c})({value});"
        );
        match handle {
            None => {
                let _ = writeln!(
                    &mut self.output,
                    "    t{} = (int64_t)ar_gen_insert_raw(&__ar_gen_payload_{slot}, {}, {}, {glue});",
                    self.temp_slots[lhs.as_usize()],
                    layout.size,
                    layout.align
                );
            }
            Some(handle) if upsert => {
                let handle = self.format_operand(handle, func);
                let _ = writeln!(
                    &mut self.output,
                    "    t{} = (int64_t)ar_gen_upsert_raw((uint64_t)({handle}), &__ar_gen_payload_{slot}, {}, {}, {glue});",
                    self.temp_slots[lhs.as_usize()],
                    layout.size,
                    layout.align
                );
            }
            Some(handle) => {
                let handle = self.format_operand(handle, func);
                let _ = writeln!(
                    &mut self.output,
                    "    if (!ar_gen_set_raw((uint64_t)({handle}), &__ar_gen_payload_{slot}, {}, {}, {glue})) abort();",
                    layout.size, layout.align
                );
                let _ = writeln!(
                    &mut self.output,
                    "    t{} = (int64_t)({handle});",
                    self.temp_slots[lhs.as_usize()]
                );
            }
        }
        let _ = writeln!(
            &mut self.output,
            "    if (t{} == 0) abort();",
            self.temp_slots[lhs.as_usize()]
        );
    }

    pub(super) fn emit_gen_read_assign(
        &mut self,
        lhs: TempId,
        name: &str,
        handle: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
        func: &AmirFunc,
    ) {
        let payload_ar = self.interner.resolve(payload_ty);
        if !self
            .provider
            .is_copy_type(payload_ty)
            .unwrap_or_else(|| payload_ar.is_copy_v01())
            && self.gen_drop_glue(payload_ty, &payload_ar).is_none()
        {
            self.record_codegen_ice(
                func,
                "non-Copy GenRef payload has no explicit @Destructor contract",
            );
            return;
        }
        let layout = self.checked_layout(&payload_ar);
        let handle = self.format_operand(handle, func);
        let _ = writeln!(
            &mut self.output,
            "    if (!{name}((uint64_t)({handle}), &t{}, {}, {})) abort();",
            self.temp_slots[lhs.as_usize()],
            layout.size,
            layout.align
        );
    }
}
