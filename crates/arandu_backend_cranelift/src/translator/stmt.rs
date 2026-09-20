use arandu_semantics::amir::{AmirStmt, LocalId};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Value};

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
                if let Some(lhs) = lhs
                    && let Some(var) = self.temp_map.get(lhs).copied()
                {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(*lhs, value);
                }
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

    fn emit_destroy_value(&mut self, ty: &ArType, ptr_val: Value, local: LocalId) {
        match ty {
            ArType::Named(_, _) => {
                let ty_id = self.type_info.type_interner.intern(ty.clone());
                if let Some(destructor) = self.type_info.destructor_instances.get(&ty_id) {
                    let symbol = self.symbol_table.get(*destructor);
                    let host_name = self.symbol_table.host_func_name(symbol);
                    if let Some(&id) = self.func_ids.get(host_name) {
                        let function = self.module.declare_func_in_func(id, self.builder.func);
                        let arg_abi = self.classify_arg_abi(ty);
                        match arg_abi {
                            arandu_semantics::layout::ArgAbi::ZeroSized => {
                                self.builder.ins().call(function, &[]);
                            }
                            arandu_semantics::layout::ArgAbi::Direct(direct) => {
                                let mut args = Vec::with_capacity(direct.slots.len());
                                for abi_slot in &direct.slots {
                                    let chunk_ty = crate::abi::abi_scalar_to_clif(abi_slot.scalar);
                                    let chunk_val = self.builder.ins().load(
                                        chunk_ty,
                                        cranelift_codegen::ir::MemFlagsData::new(),
                                        ptr_val,
                                        abi_slot.offset as i32,
                                    );
                                    args.push(chunk_val);
                                }
                                self.builder.ins().call(function, &args);
                            }
                            arandu_semantics::layout::ArgAbi::Indirect => {
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
            ArType::Array(_, elem) | ArType::ConstArray(_, elem) => {
                let elem_ty = self.type_info.resolve_type_id(*elem);
                let elem_layout = self.checked_layout(&elem_ty);
                let elem_size = elem_layout.size as i64;
                let count = match ty {
                    ArType::Array(len, _) => *len,
                    _ => 0,
                };
                for i in (0..count).rev() {
                    let offset = (i as i64) * elem_size;
                    let elem_ptr = if offset != 0 {
                        self.builder.ins().iadd_imm_s(ptr_val, offset)
                    } else {
                        ptr_val
                    };
                    self.emit_destroy_value(&elem_ty, elem_ptr, local);
                }
            }
            ArType::Tuple(args) => {
                let arg_ids = self.type_info.type_interner.type_args(*args).to_vec();
                let layout = self.checked_layout(ty);
                for (i, &arg_id) in arg_ids.iter().enumerate().rev() {
                    let arg_ty = self.type_info.resolve_type_id(arg_id);
                    let offset = layout.field_offsets.get(i).copied().unwrap_or(0) as i32;
                    let elem_ptr = if offset != 0 {
                        self.builder.ins().iadd_imm_s(ptr_val, i64::from(offset))
                    } else {
                        ptr_val
                    };
                    self.emit_destroy_value(&arg_ty, elem_ptr, local);
                }
            }
            ArType::Option(elem) => {
                let elem_ty = self.type_info.resolve_type_id(*elem);
                let layout = self.checked_layout(ty);
                let tag = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                );
                let is_some = self.builder.ins().icmp_imm_u(
                    cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                    tag,
                    0,
                );
                let payload_offset = layout
                    .field_offsets
                    .get(1)
                    .copied()
                    .unwrap_or(self.ptr_type.bytes() as u64)
                    as i32;
                let payload_ptr = if payload_offset != 0 {
                    self.builder
                        .ins()
                        .iadd_imm_s(ptr_val, i64::from(payload_offset))
                } else {
                    ptr_val
                };
                let then_block = self.builder.create_block();
                let merge_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(is_some, then_block, &[], merge_block, &[]);
                self.builder.switch_to_block(then_block);
                self.builder.seal_block(then_block);
                self.emit_destroy_value(&elem_ty, payload_ptr, local);
                self.builder.ins().jump(merge_block, &[]);
                self.builder.switch_to_block(merge_block);
                self.builder.seal_block(merge_block);
            }
            ArType::Result(ok, err) => {
                let ok_ty = self.type_info.resolve_type_id(*ok);
                let err_ty = self.type_info.resolve_type_id(*err);
                let layout = self.checked_layout(ty);
                let tag = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                );
                let is_ok = self.builder.ins().icmp_imm_u(
                    cranelift_codegen::ir::condcodes::IntCC::Equal,
                    tag,
                    0,
                );
                let payload_offset = layout
                    .field_offsets
                    .get(1)
                    .copied()
                    .unwrap_or(self.ptr_type.bytes() as u64)
                    as i32;
                let payload_ptr = if payload_offset != 0 {
                    self.builder
                        .ins()
                        .iadd_imm_s(ptr_val, i64::from(payload_offset))
                } else {
                    ptr_val
                };
                let ok_block = self.builder.create_block();
                let err_block = self.builder.create_block();
                let merge_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(is_ok, ok_block, &[], err_block, &[]);
                self.builder.switch_to_block(ok_block);
                self.builder.seal_block(ok_block);
                self.emit_destroy_value(&ok_ty, payload_ptr, local);
                self.builder.ins().jump(merge_block, &[]);
                self.builder.switch_to_block(err_block);
                self.builder.seal_block(err_block);
                self.emit_destroy_value(&err_ty, payload_ptr, local);
                self.builder.ins().jump(merge_block, &[]);
                self.builder.switch_to_block(merge_block);
                self.builder.seal_block(merge_block);
            }
            _ => {}
        }
    }
}
