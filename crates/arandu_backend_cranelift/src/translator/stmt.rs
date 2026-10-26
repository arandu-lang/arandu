use arandu_middle::amir::{AmirStmt, LocalId};
use arandu_middle::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value};

use super::FunctionTranslator;
use crate::types::{ClifType, clif_type};

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    #[tracing::instrument(level = "trace", target = "arandu_backend_cranelift", skip(self))]
    pub(super) fn translate_stmt(&mut self, stmt: &AmirStmt) {
        if self.error.is_some() {
            return;
        }
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                if self.capture_integer_concat_part(*lhs, rhs) {
                    return;
                }
                let lhs_ty = self.temp_ar_ty(*lhs);
                if matches!(&lhs_ty, ArType::Primitive(Primitive::Str)) {
                    let (ptr_val, len_val) = self.translate_str_rvalue(rhs);
                    if self.error.is_some() {
                        return;
                    }
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(lhs) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else {
                    let expected_ty = self.get_temp_clif_type(*lhs);
                    let lhs_ar = self.temp_ar_ty(*lhs);
                    let expected_ar_type = Some(&lhs_ar);
                    if lhs.as_usize() == 0 {
                        self.aggregate_destination = self.indirect_return_destination;
                    }
                    let val = self.translate_rvalue(rhs, expected_ty, expected_ar_type);
                    self.aggregate_destination = None;
                    if self.error.is_some() {
                        return;
                    }
                    if let Some(&var) = self.temp_map.get(lhs) {
                        self.builder.def_var(var, val);
                    }
                }
                if let Some(var) = self.temp_map.get(lhs).copied() {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(*lhs, value);
                }
            }
            AmirStmt::Store { lhs, rhs } => {
                let place_ty = self.place_ar_ty(lhs);
                let rhs_ty = self.get_operand_ar_type(rhs);
                let is_str = matches!(&place_ty, ArType::Primitive(Primitive::Str))
                    || matches!(&rhs_ty, ArType::Primitive(Primitive::Str));
                if is_str {
                    let (ptr_val, len_val) = self.translate_str_operand(rhs);
                    if lhs.projections.is_empty() {
                        if let Some(&(var_ptr, var_len)) = self.str_local_map.get(&lhs.local) {
                            self.builder.def_var(var_ptr, ptr_val);
                            self.builder.def_var(var_len, len_val);
                        }
                        if let Some(&slot) = self.local_stack_slots.get(&lhs.local) {
                            let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                            self.builder.ins().store(
                                cranelift_codegen::ir::MemFlagsData::new(),
                                ptr_val,
                                addr,
                                0,
                            );
                            self.builder.ins().store(
                                cranelift_codegen::ir::MemFlagsData::new(),
                                len_val,
                                addr,
                                self.ptr_type.bytes() as i32,
                            );
                        }
                    } else {
                        let (base_ptr, offset) = self.translate_place_address_for_load(lhs);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            ptr_val,
                            base_ptr,
                            offset,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            len_val,
                            base_ptr,
                            offset + self.ptr_type.bytes() as i32,
                        );
                    }
                } else {
                    let expected_ty = match clif_type(&place_ty, self.ptr_type) {
                        ClifType::Concrete(ty) => Some(ty),
                        ClifType::Void => None,
                    };
                    // Route through translate_rvalue so `T?` stores box scalars
                    // (`int? = 0` must not store a null pointer handle).
                    let val = self.translate_rvalue(
                        &arandu_middle::amir::AmirRvalue::Use(*rhs),
                        expected_ty,
                        Some(&place_ty),
                    );
                    self.translate_store_place(lhs, val);
                }
            }

            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                self.translate_call(lhs, callee, args);
                if let Some(lhs) = lhs
                    && let Some(var) = self.temp_map.get(lhs).copied()
                {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(*lhs, value);
                }
            }
            AmirStmt::Free(op) => {
                if let arandu_middle::amir::AmirOperand::Copy(temp)
                | arandu_middle::amir::AmirOperand::Move(temp) = op
                    && self.integer_concat_temps[temp.as_usize()].is_some()
                {
                    return;
                }
                let op_ty = self.get_operand_ar_type(op);
                let ptr_val = if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                    self.translate_str_operand(op).0
                } else {
                    self.translate_operand(op, Some(self.ptr_type))
                };
                self.emit_free_ptr(ptr_val);
            }
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) => {}
            AmirStmt::Destroy(place) => {
                let ty = self.place_ar_ty(place);
                if matches!(ty, ArType::Primitive(Primitive::Str)) {
                    let zero = self.builder.ins().iconst(self.ptr_type, 0);
                    if place.projections.is_empty() {
                        if let Some(&slot) = self.local_stack_slots.get(&place.local) {
                            let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                            let ptr_val = self.builder.ins().load(
                                self.ptr_type,
                                MemFlagsData::new(),
                                addr,
                                0,
                            );
                            self.emit_free_ptr(ptr_val);
                            self.builder.ins().store(MemFlagsData::new(), zero, addr, 0);
                            if let Some(&(var_ptr, _)) = self.str_local_map.get(&place.local) {
                                self.builder.def_var(var_ptr, zero);
                            }
                        } else if let Some(&(var_ptr, _)) = self.str_local_map.get(&place.local) {
                            let ptr_val = self.builder.use_var(var_ptr);
                            self.emit_free_ptr(ptr_val);
                            self.builder.def_var(var_ptr, zero);
                        }
                    } else {
                        let (base_ptr, offset) = self.translate_place_address_for_load(place);
                        let ptr_val = self.builder.ins().load(
                            self.ptr_type,
                            MemFlagsData::new(),
                            base_ptr,
                            offset,
                        );
                        self.emit_free_ptr(ptr_val);
                        self.builder
                            .ins()
                            .store(MemFlagsData::new(), zero, base_ptr, offset);
                    }
                } else {
                    let ptr_val = if place.projections.is_empty() {
                        if let Some(&var) = self.local_map.get(&place.local) {
                            self.builder.use_var(var)
                        } else {
                            self.translate_place_address_for_load(place).0
                        }
                    } else {
                        let (addr, offset) = self.translate_place_address_for_load(place);
                        if offset != 0 {
                            self.builder.ins().iadd_imm_s(addr, i64::from(offset))
                        } else {
                            addr
                        }
                    };
                    self.emit_destroy_value(&ty, ptr_val, place.local);
                }
            }
            AmirStmt::Nop => {}
        }
    }

    pub(super) fn emit_destroy_value(&mut self, ty: &ArType, ptr_val: Value, local: LocalId) {
        if let ArType::Named(_, _) = ty {
            let ty_id = self.type_info.type_interner().intern(ty.clone());
            if let Some(destructor) = self.type_info.destructor_for_type(ty_id) {
                let symbol = self.symbol_table.get(destructor);
                let host_name = self.symbol_table.host_func_name(symbol);
                if let Some(id) = self.resolve_function(host_name) {
                    let function = self.module.declare_func_in_func(id, self.builder.func);
                    let arg_abi = self.classify_arg_abi(ty);
                    match arg_abi {
                        arandu_middle::layout::ArgAbi::ZeroSized => {
                            self.builder.ins().call(function, &[]);
                        }
                        arandu_middle::layout::ArgAbi::Direct(direct) => {
                            let mut args = Vec::with_capacity(direct.slots.len());
                            let layout = self.checked_layout(ty);
                            for abi_slot in &direct.slots {
                                let chunk_val = self.load_abi_slot(ptr_val, abi_slot, layout.size);
                                args.push(chunk_val);
                            }
                            self.builder.ins().call(function, &args);
                        }
                        arandu_middle::layout::ArgAbi::Indirect => {
                            self.builder.ins().call(function, &[ptr_val]);
                        }
                    }
                } else {
                    self.record_ice(
                        format!("missing @Destructor function '{}'", symbol.name),
                        self.local_span(local),
                    );
                }
            }
        }
    }
}
