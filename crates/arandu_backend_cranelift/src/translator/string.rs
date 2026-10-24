use arandu_middle::amir::{AmirOperand, AmirRvalue};
use arandu_middle::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// Capture at the original ToStr position; only its proven-private buffer
    /// is removed. Formatting into the result happens at the concat position.
    pub(super) fn capture_integer_concat_part(
        &mut self,
        temp: arandu_middle::amir::TempId,
        rvalue: &AmirRvalue,
    ) -> bool {
        use arandu_codegen::string_interp::IntegerStringKind;
        let Some(kind) = self.integer_concat_temps[temp.as_usize()] else {
            return false;
        };
        let AmirRvalue::ToStr { value, .. } = rvalue else {
            return false;
        };
        let i64_type = cranelift_codegen::ir::types::I64;
        let value = self.translate_operand(value, Some(i64_type));
        let value_type = self.builder.func.dfg.value_type(value);
        let value = if value_type.bits() < 64 {
            match kind {
                IntegerStringKind::Signed => self.builder.ins().sextend(i64_type, value),
                IntegerStringKind::Unsigned => self.builder.ins().uextend(i64_type, value),
            }
        } else if value_type.bits() > 64 {
            self.builder.ins().ireduce(i64_type, value)
        } else {
            value
        };
        let helper = match kind {
            IntegerStringKind::Signed => "ar_rt_i64_write_digits",
            IntegerStringKind::Unsigned => "ar_rt_u64_write_digits",
        };
        let null = self.builder.ins().iconst(self.ptr_type, 0);
        let len = self.call_integer_writer(helper, value, null);
        let len = if self.ptr_type.bits() < 64 {
            self.builder.ins().ireduce(self.ptr_type, len)
        } else {
            len
        };
        self.integer_concat_values
            .insert(temp, (value, len, helper));
        true
    }

    fn call_integer_writer(&mut self, helper: &str, value: Value, destination: Value) -> Value {
        let Some(id) = self.resolve_function(helper) else {
            self.record_ice(
                format!("missing integer interpolation helper {helper}"),
                self.func_span(),
            );
            return self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, 0);
        };
        let reference = self.module.declare_func_in_func(id, self.builder.func);
        let call = self.builder.ins().call(reference, &[value, destination]);
        self.builder.inst_results(call)[0]
    }

    /// Add string byte lengths without allowing a wrapped allocation size.
    fn checked_string_size_add(&mut self, lhs: Value, rhs: Value) -> Value {
        let sum = self.builder.ins().iadd(lhs, rhs);
        let overflow = self.builder.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
            sum,
            lhs,
        );
        self.builder
            .ins()
            .trapnz(overflow, cranelift_codegen::ir::TrapCode::unwrap_user(1));
        sum
    }

    pub(super) fn translate_str_rvalue(&mut self, rvalue: &AmirRvalue) -> (Value, Value) {
        if self.error.is_some() {
            return (self.poison_i32(), self.poison_i32());
        }

        match rvalue {
            AmirRvalue::Use(op) => self.translate_str_operand(op),
            AmirRvalue::Unary {
                op: arandu_middle::ops::UnaryOp::Deref,
                operand,
            } => {
                let address = self.translate_operand(operand, Some(self.ptr_type));
                let flags = cranelift_codegen::ir::MemFlagsData::new();
                let ptr = self.builder.ins().load(self.ptr_type, flags, address, 0);
                let len = self.builder.ins().load(
                    self.ptr_type,
                    flags,
                    address,
                    self.ptr_type.bytes() as i32,
                );
                (ptr, len)
            }
            AmirRvalue::Load(place) => {
                if place.projections.is_empty() {
                    if let Some(&slot) = self.local_stack_slots.get(&place.local) {
                        let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                        let loaded_ptr = self.builder.ins().load(
                            self.ptr_type,
                            cranelift_codegen::ir::MemFlagsData::new(),
                            addr,
                            0,
                        );
                        let loaded_len = self.builder.ins().load(
                            self.ptr_type,
                            cranelift_codegen::ir::MemFlagsData::new(),
                            addr,
                            self.ptr_type.bytes() as i32,
                        );
                        (loaded_ptr, loaded_len)
                    } else if let Some(&(var_ptr, var_len)) = self.str_local_map.get(&place.local) {
                        (self.builder.use_var(var_ptr), self.builder.use_var(var_len))
                    } else {
                        self.record_ice(
                            "use of undeclared AMIR local str in codegen",
                            self.local_span(place.local),
                        );
                        (self.poison_i32(), self.poison_i32())
                    }
                } else {
                    let (ptr_val, offset) = self.translate_place_address_for_load(place);
                    let loaded_ptr = self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr_val,
                        offset,
                    );
                    let loaded_len = self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr_val,
                        offset + self.ptr_type.bytes() as i32,
                    );
                    (loaded_ptr, loaded_len)
                }
            }
            AmirRvalue::IndexAccess { base, index } => {
                let (address, _) = self.translate_index_address(base, index);
                let flags = cranelift_codegen::ir::MemFlagsData::new();
                let ptr = self.builder.ins().load(self.ptr_type, flags, address, 0);
                let len = self.builder.ins().load(
                    self.ptr_type,
                    flags,
                    address,
                    self.ptr_type.bytes() as i32,
                );
                (ptr, len)
            }
            AmirRvalue::FieldAccess { base, field } => {
                let ptr_val = self.translate_operand(base, Some(self.ptr_type));
                let base_ty = match base {
                    AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                        self.temp_ar_ty(*temp_id)
                    }
                    _ => arandu_middle::types::ArType::Error,
                };
                let struct_ty = match base_ty {
                    arandu_middle::types::ArType::Ptr(inner)
                    | arandu_middle::types::ArType::Ref(inner)
                    | arandu_middle::types::ArType::RefMut(inner)
                    | arandu_middle::types::ArType::Nullable(inner) => {
                        self.type_info.resolve_type_id(inner)
                    }
                    other => other,
                };
                let pointer_width = self.ptr_type.bytes() as u64;
                let layout = self.checked_layout(&struct_ty);
                let offset = layout.field_offsets.get(*field).copied().unwrap_or(0) as i32;

                let loaded_ptr = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    offset,
                );
                let loaded_len = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    offset + pointer_width as i32,
                );
                (loaded_ptr, loaded_len)
            }
            AmirRvalue::EnumPayload {
                value,
                variant,
                index,
                tuple_ty,
                ..
            } => {
                let ptr_val = self.translate_operand(value, Some(self.ptr_type));
                let pointer_width = self.ptr_type.bytes() as u64;

                let base_ty = match value {
                    AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                        self.temp_ar_ty(*temp_id)
                    }
                    _ => arandu_middle::types::ArType::Error,
                };
                let enum_ty = match base_ty {
                    arandu_middle::types::ArType::Ptr(inner) => {
                        self.type_info.resolve_type_id(inner)
                    }
                    other => other,
                };
                let tag = self.type_info.enum_variant_tag(*variant).unwrap_or(0);

                let mut payload_offset = 0;
                if let Some(payload_ty_id) = (*tuple_ty).or_else(|| {
                    arandu_middle::layout::instantiated_enum_variant_payload_type(
                        &enum_ty,
                        tag,
                        self.type_info.type_interner(),
                        self.type_info,
                    )
                }) {
                    let payload_ty = self.type_info.resolve_type_id(payload_ty_id);
                    let payload_layout = self.checked_layout(&payload_ty);
                    if *index < payload_layout.field_offsets.len() {
                        payload_offset = payload_layout.field_offsets[*index] as i32;
                    }
                }

                let enum_layout = self.checked_layout(&enum_ty);
                let base_offset = enum_layout
                    .field_offsets
                    .get(1)
                    .copied()
                    .unwrap_or(pointer_width) as i32;
                let total_offset = base_offset + payload_offset;
                let loaded_ptr = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    total_offset,
                );
                let loaded_len = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    total_offset + self.ptr_type.bytes() as i32,
                );
                (loaded_ptr, loaded_len)
            }
            AmirRvalue::StringInterp { parts } => self.translate_string_interp(parts),
            AmirRvalue::ToStr { value, src_ty } => self.translate_to_str(value, *src_ty),
            AmirRvalue::BlackBox { value, .. } => {
                let (ptr, len) = self.translate_str_operand(value);
                let Some(ptr_id) = self.resolve_function("ar_bench_black_box_ptr") else {
                    self.record_ice("missing pointer black-box runtime import", self.func_span());
                    return (self.poison_i32(), self.poison_i32());
                };
                let Some(len_id) = self.resolve_function("ar_bench_black_box_i64") else {
                    self.record_ice("missing integer black-box runtime import", self.func_span());
                    return (self.poison_i32(), self.poison_i32());
                };
                let ptr_ref = self.module.declare_func_in_func(ptr_id, self.builder.func);
                let len_ref = self.module.declare_func_in_func(len_id, self.builder.func);
                let ptr_call = self.builder.ins().call(ptr_ref, &[ptr]);
                let len_arg = if self.ptr_type.bits() < 64 {
                    self.builder
                        .ins()
                        .uextend(cranelift_codegen::ir::types::I64, len)
                } else {
                    len
                };
                let len_call = self.builder.ins().call(len_ref, &[len_arg]);
                let ptr_result = self.builder.inst_results(ptr_call)[0];
                let mut len_result = self.builder.inst_results(len_call)[0];
                if self.ptr_type.bits() < 64 {
                    len_result = self.builder.ins().ireduce(self.ptr_type, len_result);
                }
                (ptr_result, len_result)
            }
            _ => {
                self.record_ice(
                    "unsupported rvalue kind returning str in codegen",
                    self.func_span(),
                );
                (self.poison_i32(), self.poison_i32())
            }
        }
    }

    /// Format a primitive via host ToStr helpers → `(ptr, len)`.
    fn translate_to_str(
        &mut self,
        value: &AmirOperand,
        src_ty: arandu_middle::types::TypeId,
    ) -> (Value, Value) {
        let ar_ty = self.type_info.type_interner().resolve(src_ty);
        if matches!(ar_ty, ArType::Primitive(Primitive::Str)) {
            return self.translate_str_operand(value);
        }

        let i64_ty = cranelift_codegen::ir::types::I64;
        let helper_name = match &ar_ty {
            ArType::Primitive(Primitive::Bool) => "ar_jit_bool_to_str",
            ArType::Primitive(Primitive::Char) => "ar_jit_char_to_str",
            ArType::FloatLiteral => "ar_jit_f64_to_str",
            ArType::Primitive(p) if p.is_float() => "ar_jit_f64_to_str",
            ArType::IntLiteral => "ar_jit_i64_to_str",
            ArType::Primitive(p) if p.is_integer() && p.is_signed() => "ar_jit_i64_to_str",
            ArType::Primitive(p) if p.is_integer() => "ar_jit_u64_to_str",
            ArType::Err => "ar_jit_err_to_str",
            _ => {
                self.record_ice(
                    format!("ToStr v0.1 unsupported type in Cranelift: {ar_ty:?}"),
                    self.func_span(),
                );
                return (self.poison_i32(), self.poison_i32());
            }
        };

        // Stack slot for out_len: i64
        let len_slot = self
            .builder
            .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                size: 8,
                align_shift: 3,
                key: None,
            });
        let len_ptr = self.builder.ins().stack_addr(self.ptr_type, len_slot, 0);

        let arg = match helper_name {
            "ar_jit_bool_to_str" => {
                let v = self.translate_operand(value, Some(cranelift_codegen::ir::types::I8));
                // Ensure i8 width
                if self.builder.func.dfg.value_type(v) != cranelift_codegen::ir::types::I8 {
                    self.builder
                        .ins()
                        .ireduce(cranelift_codegen::ir::types::I8, v)
                } else {
                    v
                }
            }
            "ar_jit_char_to_str" => {
                let v = self.translate_operand(value, Some(cranelift_codegen::ir::types::I32));
                let vt = self.builder.func.dfg.value_type(v);
                if vt == cranelift_codegen::ir::types::I32 {
                    v
                } else if vt.bits() < 32 {
                    self.builder
                        .ins()
                        .uextend(cranelift_codegen::ir::types::I32, v)
                } else {
                    self.builder
                        .ins()
                        .ireduce(cranelift_codegen::ir::types::I32, v)
                }
            }
            "ar_jit_f64_to_str" => {
                let v = self.translate_operand(value, Some(cranelift_codegen::ir::types::F64));
                let vt = self.builder.func.dfg.value_type(v);
                if vt == cranelift_codegen::ir::types::F64 {
                    v
                } else if vt == cranelift_codegen::ir::types::F32 {
                    self.builder
                        .ins()
                        .fpromote(cranelift_codegen::ir::types::F64, v)
                } else {
                    v
                }
            }
            "ar_jit_i64_to_str" | "ar_jit_u64_to_str" => {
                let v = self.translate_operand(value, Some(i64_ty));
                let vt = self.builder.func.dfg.value_type(v);
                if vt == i64_ty {
                    v
                } else if vt.bits() < 64 {
                    if helper_name == "ar_jit_u64_to_str" {
                        self.builder.ins().uextend(i64_ty, v)
                    } else {
                        self.builder.ins().sextend(i64_ty, v)
                    }
                } else {
                    self.builder.ins().ireduce(i64_ty, v)
                }
            }
            "ar_jit_err_to_str" => self.translate_operand(value, Some(self.ptr_type)),
            _ => unreachable!(),
        };

        let Some(func_id) = self.resolve_function(helper_name) else {
            self.record_ice(
                format!("{helper_name} was not declared in the JIT module"),
                self.func_span(),
            );
            return (self.poison_i32(), self.poison_i32());
        };
        let local_ref = self.module.declare_func_in_func(func_id, self.builder.func);
        let call = self.builder.ins().call(local_ref, &[arg, len_ptr]);
        let ptr = self.builder.inst_results(call)[0];
        let len = self
            .builder
            .ins()
            .stack_load(self.ptr_type, i64_ty, len_slot, 0);
        let len = if self.ptr_type.bits() < 64 {
            self.builder.ins().ireduce(self.ptr_type, len)
        } else {
            len
        };
        (ptr, len)
    }

    /// Compare two `str` fat pointers for equality (`==` / `!=`).
    /// Equal when lengths match and either both are empty or `memcmp` reports zero.
    pub(super) fn translate_str_eq(
        &mut self,
        left: &AmirOperand,
        right: &AmirOperand,
        op: arandu_middle::ops::BinaryOp,
    ) -> Value {
        let (l_ptr, l_len) = self.translate_str_operand(left);
        let (r_ptr, r_len) = self.translate_str_operand(right);
        let Some(memcmp_id) = self.memcmp_func_id() else {
            return self.poison_i32();
        };

        let len_eq =
            self.builder
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, l_len, r_len);
        let bool_ty = self.builder.func.dfg.value_type(len_eq);
        let different_lengths = self.builder.create_block();
        let same_length = self.builder.create_block();
        let empty_strings = self.builder.create_block();
        let compare_bytes = self.builder.create_block();
        let join = self.builder.create_block();
        self.builder.append_block_param(join, bool_ty);
        self.builder
            .ins()
            .brif(len_eq, same_length, &[], different_lengths, &[]);

        self.builder.switch_to_block(different_lengths);
        self.builder.seal_block(different_lengths);
        self.builder
            .ins()
            .jump(join, &[cranelift_codegen::ir::BlockArg::Value(len_eq)]);

        self.builder.switch_to_block(same_length);
        self.builder.seal_block(same_length);
        let zero_len = self.builder.ins().iconst(self.ptr_type, 0);
        let is_empty = self.builder.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            l_len,
            zero_len,
        );
        self.builder
            .ins()
            .brif(is_empty, empty_strings, &[], compare_bytes, &[]);

        self.builder.switch_to_block(empty_strings);
        self.builder.seal_block(empty_strings);
        self.builder
            .ins()
            .jump(join, &[cranelift_codegen::ir::BlockArg::Value(len_eq)]);

        self.builder.switch_to_block(compare_bytes);
        self.builder.seal_block(compare_bytes);
        let memcmp_ref = self
            .module
            .declare_func_in_func(memcmp_id, self.builder.func);
        // memcmp size is size_t (= pointer width).
        let size = l_len;

        let call = self.builder.ins().call(memcmp_ref, &[l_ptr, r_ptr, size]);
        let cmp = self.builder.inst_results(call)[0];
        let zero_i32 = self
            .builder
            .ins()
            .iconst(cranelift_codegen::ir::types::I32, 0);
        let bytes_eq = self.builder.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            cmp,
            zero_i32,
        );
        self.builder
            .ins()
            .jump(join, &[cranelift_codegen::ir::BlockArg::Value(bytes_eq)]);

        self.builder.switch_to_block(join);
        self.builder.seal_block(join);
        let eq = self.builder.block_params(join)[0];

        match op {
            arandu_middle::ops::BinaryOp::Equal => eq,
            arandu_middle::ops::BinaryOp::NotEqual => self.builder.ins().bxor_imm_u(eq, 1),
            _ => self.poison_i32(),
        }
    }

    /// Concatenate `str` fat-pointer parts via `malloc` + `memcpy`.
    /// Zero-length parts skip the copy so a null data pointer never reaches libc.
    /// Returns caller-owned `(ptr, len)`, paired with the existing string drop.
    fn translate_string_interp(&mut self, parts: &[AmirOperand]) -> (Value, Value) {
        if parts.is_empty() {
            let empty_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            let empty_len = self.builder.ins().iconst(self.ptr_type, 0);
            return (empty_ptr, empty_len);
        }

        // Materialize each part as (ptr, len).
        let mut part_vals = Vec::with_capacity(parts.len());
        for part in parts {
            let integer = match part {
                AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                    self.integer_concat_values.get(temp).copied()
                }
                _ => None,
            };
            if let Some((value, len, helper)) = integer {
                part_vals.push((value, len, Some(helper)));
            } else {
                let (pointer, len) = self.translate_str_operand(part);
                part_vals.push((pointer, len, None));
            }
        }

        // total = sum of lengths (ptr_type), trapping instead of wrapping to
        // an allocation smaller than the bytes copied below.
        let mut total = self.builder.ins().iconst(self.ptr_type, 0);
        for &(_, len, _) in &part_vals {
            total = self.checked_string_size_add(total, len);
        }

        // malloc(total + 1) for trailing NUL safety
        let one = self.builder.ins().iconst(self.ptr_type, 1);
        let alloc_size = self.checked_string_size_add(total, one);

        let Some(malloc_id) = self.malloc_func_id() else {
            return (self.poison_i32(), self.poison_i32());
        };
        let malloc_ref = self
            .module
            .declare_func_in_func(malloc_id, self.builder.func);
        let call = self.builder.ins().call(malloc_ref, &[alloc_size]);
        let buf = self.builder.inst_results(call)[0];
        self.trap_if_null(buf);

        let Some(memcpy_id) = self.memcpy_func_id() else {
            return (self.poison_i32(), self.poison_i32());
        };
        let memcpy_ref = self
            .module
            .declare_func_in_func(memcpy_id, self.builder.func);

        // Copy each part into the buffer.
        let mut offset_ptr = self.builder.ins().iconst(self.ptr_type, 0);
        for &(src_ptr, src_len, integer_helper) in &part_vals {
            if let Some(helper) = integer_helper {
                let destination = self.builder.ins().iadd(buf, offset_ptr);
                self.call_integer_writer(helper, src_ptr, destination);
                offset_ptr = self.builder.ins().iadd(offset_ptr, src_len);
                continue;
            }
            let zero_len = self.builder.ins().iconst(self.ptr_type, 0);
            let is_empty = self.builder.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                src_len,
                zero_len,
            );
            let copy_part = self.builder.create_block();
            let next_part = self.builder.create_block();
            self.builder
                .ins()
                .brif(is_empty, next_part, &[], copy_part, &[]);

            self.builder.switch_to_block(copy_part);
            self.builder.seal_block(copy_part);
            let dest = self.builder.ins().iadd(buf, offset_ptr);
            self.builder
                .ins()
                .call(memcpy_ref, &[dest, src_ptr, src_len]);
            self.builder.ins().jump(next_part, &[]);

            self.builder.switch_to_block(next_part);
            self.builder.seal_block(next_part);
            offset_ptr = self.builder.ins().iadd(offset_ptr, src_len);
        }

        // Write trailing NUL at buf + total
        let zero_byte = self
            .builder
            .ins()
            .iconst(cranelift_codegen::ir::types::I8, 0);
        let end_ptr = self.builder.ins().iadd(buf, total);
        self.builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            zero_byte,
            end_ptr,
            0,
        );

        (buf, total)
    }
}
