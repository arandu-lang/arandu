use arandu_semantics::amir::AmirStmt;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::InstBuilder;

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
                let lhs_ty = self.temp_ar_ty(*lhs);
                if matches!(&lhs_ty, ArType::Primitive(Primitive::Str)) {
                    let (ptr_val, len_val) = self.translate_str_rvalue(rhs);
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(lhs) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else {
                    let expected_ty = self.get_temp_clif_type(*lhs);
                    let lhs_ar = self.temp_ar_ty(*lhs);
                    let expected_ar_type = Some(&lhs_ar);
                    let val = self.translate_rvalue(rhs, expected_ty, expected_ar_type);
                    if let Some(&var) = self.temp_map.get(lhs) {
                        self.builder.def_var(var, val);
                    }
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
                        &arandu_semantics::amir::AmirRvalue::Use(*rhs),
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
            }
            AmirStmt::Free(op) => {
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
                    if let Some(&(var_ptr, _)) = self.str_local_map.get(&place.local) {
                        let ptr_val = self.builder.use_var(var_ptr);
                        self.emit_free_ptr(ptr_val);
                    }
                } else {
                    let ty_id = self.type_info.type_interner.intern(ty.clone());
                    if let ArType::Named(_, _) = ty
                        && let Some(destructor) = self.type_info.destructor_instances.get(&ty_id)
                    {
                        let symbol = self.symbol_table.get(*destructor);
                        let host_name = self.symbol_table.host_func_name(symbol);
                        if let Some(&id) = self.func_ids.get(host_name) {
                            let ptr_val = if place.projections.is_empty() {
                                if let Some(&var) = self.local_map.get(&place.local) {
                                    self.builder.use_var(var)
                                } else {
                                    self.translate_place_address_for_load(place).0
                                }
                            } else {
                                let (addr, offset) = self.translate_place_address_for_load(place);
                                self.builder.ins().load(
                                    self.ptr_type,
                                    cranelift_codegen::ir::MemFlagsData::new(),
                                    addr,
                                    offset,
                                )
                            };
                            let function = self.module.declare_func_in_func(id, self.builder.func);
                            self.builder.ins().call(function, &[ptr_val]);
                        } else {
                            self.record_ice(
                                format!("missing @Destructor function '{}'", symbol.name),
                                self.local_span(place.local),
                            );
                        }
                    }
                }
            }
            AmirStmt::Nop => {}
        }
    }
}
