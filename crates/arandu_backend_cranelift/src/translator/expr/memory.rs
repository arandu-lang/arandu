//! Memory loads, borrows, slice descriptors, length queries, and allocation.

use arandu_semantics::amir::{AmirOperand, AmirPlace};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_slice_view(&mut self, data: &AmirOperand, len: &AmirOperand) -> Value {
        let data = self.translate_operand(data, Some(self.ptr_type));
        let len = self.translate_operand(len, Some(self.ptr_type));
        self.materialize_slice_descriptor(data, len)
    }

    pub(super) fn translate_str_bytes(&mut self, source: &AmirOperand) -> Value {
        let (data, len) = self.translate_str_operand(source);
        self.materialize_slice_descriptor(data, len)
    }

    pub(super) fn translate_slice_subslice(
        &mut self,
        slice: &AmirOperand,
        start: &AmirOperand,
        len: &AmirOperand,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        let descriptor = self.translate_operand(slice, Some(self.ptr_type));
        let data = self.builder.ins().load(
            self.ptr_type,
            cranelift_codegen::ir::MemFlagsData::new(),
            descriptor,
            0,
        );
        let start = self.translate_operand(start, Some(self.ptr_type));
        let elem_size = match expected_ar_type {
            Some(ArType::Slice(inner)) => {
                let element = self.type_info.type_interner.resolve(*inner);
                self.checked_layout(&element).size
            }
            _ => match self.get_operand_ar_type(slice) {
                ArType::Slice(inner) => {
                    let element = self.type_info.type_interner.resolve(inner);
                    self.checked_layout(&element).size
                }
                _ => 1,
            },
        };
        let width = self.builder.ins().iconst(self.ptr_type, elem_size as i64);
        let offset = self.builder.ins().imul(start, width);
        let data = self.builder.ins().iadd(data, offset);
        let len = self.translate_operand(len, Some(self.ptr_type));
        self.materialize_slice_descriptor(data, len)
    }

    pub(super) fn translate_slice_data(&mut self, slice: &AmirOperand) -> Value {
        let descriptor = self.translate_operand(slice, Some(self.ptr_type));
        self.builder.ins().load(
            self.ptr_type,
            cranelift_codegen::ir::MemFlagsData::new(),
            descriptor,
            0,
        )
    }

    pub(super) fn translate_load(
        &mut self,
        place: &AmirPlace,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        if place.projections.is_empty() {
            // Address-taken scalar: load from stack home (F2.0).
            if let Some(&slot) = self.local_stack_slots.get(&place.local) {
                let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                let clif_ty = expected_ty.unwrap_or(self.ptr_type);
                self.builder.ins().load(
                    clif_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    addr,
                    0,
                )
            } else {
                match self.local_map.get(&place.local) {
                    Some(var) => self.builder.use_var(*var),
                    None => {
                        self.record_ice(
                            "use of undeclared AMIR local in codegen",
                            self.local_span(place.local),
                        );
                        self.poison_i32()
                    }
                }
            }
        } else {
            let (base_ptr, offset) = self.translate_place_address_for_load(place);
            let place_ty = expected_ar_type
                .cloned()
                .unwrap_or_else(|| self.place_ar_ty(place));
            if self.is_inline_aggregate_ty(&place_ty) {
                // Projected inline aggregates and slices are stored inline. Their value in
                // the single-slot JIT representation is the address of the sub-aggregate,
                // not the first scalar word.
                return if offset == 0 {
                    base_ptr
                } else {
                    self.builder.ins().iadd_imm_s(base_ptr, i64::from(offset))
                };
            }
            let clif_ty = expected_ty.unwrap_or(self.ptr_type);
            self.builder.ins().load(
                clif_ty,
                cranelift_codegen::ir::MemFlagsData::new(),
                base_ptr,
                offset,
            )
        }
    }

    pub(super) fn translate_borrow(&mut self, place: &AmirPlace) -> Value {
        let ty = self.local_ar_ty(place.local);
        let borrowed_ty = self.place_ar_ty(place);
        let local_is_memory = self.current_func.locals[place.local.as_usize()].is_memory;
        let has_stack_home = self.local_stack_slots.contains_key(&place.local);
        // BC.4a: Deref means base local already holds a materialised pointer
        // (heap/`ptr`/`&T`); address = use_var, never stack_addr of the slot.
        let through_ptr = place
            .projections
            .iter()
            .any(|p| matches!(p, arandu_semantics::amir::AmirProjection::Deref));
        let is_memory_backed = through_ptr
            || has_stack_home
            || local_is_memory
            || !place.projections.is_empty()
            || matches!(
                ty,
                ArType::Tuple(_)
                    | ArType::Array(_, _)
                    | ArType::Slice(_)
                    | ArType::Primitive(Primitive::Str)
                    | ArType::Ref(_)
                    | ArType::RefMut(_)
                    | ArType::Ptr(_)
            )
            || matches!(
                ty,
                ArType::Named(sym_id, _) if matches!(
                    self.symbol_table.get(sym_id).kind,
                    arandu_semantics::SymbolKind::Struct | arandu_semantics::SymbolKind::Enum
                )
            );

        if is_memory_backed {
            // Aggregates are represented by a pointer value in the JIT. Their stack
            // home stores that pointer, so `&aggregate` must pass the stored value —
            // not the address of the stack slot containing it. The latter adds an
            // unintended level of indirection and corrupts field/index projection.
            if place.projections.is_empty()
                && matches!(
                    borrowed_ty,
                    ArType::Named(_, _) | ArType::Array(_, _) | ArType::Tuple(_) | ArType::Slice(_)
                )
            {
                if let Some(&slot) = self.local_stack_slots.get(&place.local) {
                    let slot_addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                    return self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        slot_addr,
                        0,
                    );
                }
                if let Some(&var) = self.local_map.get(&place.local) {
                    return self.builder.use_var(var);
                }
            }
            let (base_ptr, offset) = self.translate_place_address_for_load(place);
            if offset == 0 {
                base_ptr
            } else {
                let offset_val = self.builder.ins().iconst(self.ptr_type, offset as i64);
                self.builder.ins().iadd(base_ptr, offset_val)
            }
        } else {
            self.record_error(
                arandu_semantics::DiagCode::U001FeatureNotSupported,
                "borrow of non-memory local without address_taken (F2.0 should mark is_memory)",
                self.local_span(place.local),
            );
            self.poison_i32()
        }
    }

    pub(super) fn translate_len(&mut self, op: &AmirOperand, expected_ty: Option<Type>) -> Value {
        let op_ty = self.get_operand_ar_type(op);
        let i64_ty = cranelift_codegen::ir::types::I64;
        let result_ty = expected_ty.unwrap_or(self.ptr_type);

        match op_ty {
            ArType::Array(len, _) => {
                let v = self.builder.ins().iconst(i64_ty, len as i64);
                self.cast_int_width(v, result_ty)
            }
            ArType::Primitive(Primitive::Str) => {
                // Dual-value Str ABI: reuse the str operand path for temps + literals.
                let (_, len_val) = self.translate_str_operand(op);
                self.cast_int_width(len_val, result_ty)
            }
            _ if op_ty
                .slice_abi_element(&self.type_info.type_interner)
                .is_some() =>
            {
                // Slice fat pointer in memory: {ptr @0, len @pointer_width}.
                let base = self.translate_operand(op, Some(self.ptr_type));
                let len_off = self.ptr_type.bytes() as i32;
                let len_val = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    base,
                    len_off,
                );
                self.cast_int_width(len_val, result_ty)
            }
            _ => {
                self.record_ice(
                    format!("Len not supported for type {op_ty:?}"),
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }

    /// Byte-count heap allocation via `malloc` (RC-RVALUE-GAPS).
    pub(super) fn translate_alloc(&mut self, op: &AmirOperand) -> Value {
        let size_val = self.translate_operand(op, Some(self.ptr_type));
        let Some(malloc_id) = self.malloc_func_id() else {
            return self.poison_i32();
        };
        let malloc_ref = self
            .module
            .declare_func_in_func(malloc_id, self.builder.func);
        let call = self.builder.ins().call(malloc_ref, &[size_val]);
        let ptr = self.builder.inst_results(call)[0];
        self.trap_if_null_for_nonzero_size(ptr, size_val);
        ptr
    }
}
