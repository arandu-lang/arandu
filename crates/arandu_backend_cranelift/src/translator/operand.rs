use arandu_semantics::amir::{AmirConstant, AmirOperand};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type, Value};
use cranelift_module::Module;

use super::FunctionTranslator;

/// Whether an AMIR type is carried as a fat pointer `{ data, len }`, either
/// directly (str / slice) or behind a reference/pointer to such a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FatOperandKind {
    None,
    Str,
    Slice,
}

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// Classifies an AMIR type as a fat-pointer ABI operand (str / slice).
    pub(super) fn fat_operand_kind(&self, ty: &ArType) -> FatOperandKind {
        match ty {
            ArType::Primitive(Primitive::Str) => FatOperandKind::Str,
            _ if ty
                .slice_abi_element(&self.type_info.type_interner)
                .is_some() =>
            {
                FatOperandKind::Slice
            }
            _ => FatOperandKind::None,
        }
    }

    /// Loads the `{ data, len }` pair from behind a reference/pointer to a
    /// str or slice descriptor. The String object layout (`data` at 0, `len`
    /// at pointer width) matches the fat descriptor layout, so a `ref str`
    /// view can be passed directly where a `str` (or `[]T`) is expected.
    pub(super) fn translate_fat_ref_operand(&mut self, operand: &AmirOperand) -> (Value, Value) {
        if self.error.is_some() {
            return (self.poison_i32(), self.poison_i32());
        }
        let base = self.translate_operand(operand, Some(self.ptr_type));
        let flags = cranelift_codegen::ir::MemFlagsData::new();
        let data = self.builder.ins().load(self.ptr_type, flags, base, 0);
        let len = self
            .builder
            .ins()
            .load(self.ptr_type, flags, base, self.ptr_type.bytes() as i32);
        (data, len)
    }

    pub(super) fn translate_slice_operand(&mut self, operand: &AmirOperand) -> (Value, Value) {
        let op_ty = self.get_operand_ar_type(operand);
        if let ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) = &op_ty
            && matches!(
                self.resolve_ty(*inner),
                ArType::Primitive(Primitive::Str) | ArType::Slice(_)
            )
        {
            return self.translate_fat_ref_operand(operand);
        }
        let descriptor = self.translate_operand(operand, Some(self.ptr_type));
        let flags = cranelift_codegen::ir::MemFlagsData::new();
        let data = self.builder.ins().load(self.ptr_type, flags, descriptor, 0);
        let len = self.builder.ins().load(
            self.ptr_type,
            flags,
            descriptor,
            self.ptr_type.bytes() as i32,
        );
        (data, len)
    }

    pub(super) fn translate_str_operand(&mut self, operand: &AmirOperand) -> (Value, Value) {
        if self.error.is_some() {
            return (self.poison_i32(), self.poison_i32());
        }

        match operand {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(temp_id) {
                    let ptr_val = self.builder.use_var(var_ptr);
                    let len_val = self.builder.use_var(var_len);
                    (ptr_val, len_val)
                } else if let Some(&var) = self.temp_map.get(temp_id) {
                    let op_ty = self.get_operand_ar_type(operand);
                    if let ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) = &op_ty
                        && matches!(
                            self.resolve_ty(*inner),
                            ArType::Primitive(Primitive::Str) | ArType::Slice(_)
                        )
                    {
                        // `ref str` view: the var is a pointer to the
                        // `{ data, len }` descriptor (e.g. a String object).
                        let ptr_val = self.builder.use_var(var);
                        let flags = cranelift_codegen::ir::MemFlagsData::new();
                        let data = self.builder.ins().load(self.ptr_type, flags, ptr_val, 0);
                        let len = self.builder.ins().load(
                            self.ptr_type,
                            flags,
                            ptr_val,
                            self.ptr_type.bytes() as i32,
                        );
                        return (data, len);
                    }
                    let ptr_val = self.builder.use_var(var);
                    let len_val = self.builder.ins().iconst(self.ptr_type, 0);
                    (ptr_val, len_val)
                } else {
                    self.record_ice(
                        "use of undeclared AMIR temp in codegen",
                        self.temp_span(*temp_id),
                    );
                    (self.poison_i32(), self.poison_i32())
                }
            }
            AmirOperand::Constant(AmirConstant::Nil) => {
                // Empty string / null fat pointer (used when zeroing the Ok binding
                // on a Result.Err path of `let ok, err = …`).
                let ptr_val = self.builder.ins().iconst(self.ptr_type, 0);
                let len_val = self.builder.ins().iconst(self.ptr_type, 0);
                (ptr_val, len_val)
            }
            AmirOperand::Constant(AmirConstant::Pool(lit_id)) => {
                let entry = self.literal_pool.get(*lit_id);
                if let arandu_semantics::literal_pool::AmirLiteralEntry::Str(s) = entry {
                    let str_bytes = s.as_bytes();
                    let data_id = match self.module.declare_data(
                        &format!("str_lit_{}", lit_id.0),
                        cranelift_module::Linkage::Local,
                        false,
                        false,
                    ) {
                        Ok(data_id) => data_id,
                        Err(err) => {
                            self.record_ice(
                                format!("failed to declare string literal in JIT module: {err:?}"),
                                self.func_span(),
                            );
                            return (self.poison_i32(), self.poison_i32());
                        }
                    };
                    let mut data_ctx = cranelift_module::DataDescription::new();
                    data_ctx.define(str_bytes.to_vec().into_boxed_slice());
                    let _ = self.module.define_data(data_id, &data_ctx);
                    let local_data_ref =
                        self.module.declare_data_in_func(data_id, self.builder.func);
                    let ptr_val = self
                        .builder
                        .ins()
                        .symbol_value(self.ptr_type, local_data_ref);
                    let len_val = self.builder.ins().iconst(self.ptr_type, s.len() as i64);
                    (ptr_val, len_val)
                } else {
                    self.record_ice("expected string literal in pool", self.func_span());
                    (self.poison_i32(), self.poison_i32())
                }
            }
            _ => {
                self.record_ice(
                    "unsupported operand for translate_str_operand",
                    self.func_span(),
                );
                (self.poison_i32(), self.poison_i32())
            }
        }
    }

    pub(super) fn translate_operand(
        &mut self,
        operand: &AmirOperand,
        expected_ty: Option<Type>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        let mut val = match operand {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                match self.temp_map.get(temp_id) {
                    Some(var) => self.builder.use_var(*var),
                    None => {
                        // ZST temps (void / typeck error) have no Cranelift vars.
                        // `Err` is pointer-sized and must be declared like other scalars.
                        let ty = self.temp_ar_ty(*temp_id);
                        if matches!(
                            ty,
                            arandu_semantics::types::ArType::Void
                                | arandu_semantics::types::ArType::Error
                        ) {
                            return self.poison_i32();
                        }
                        self.record_ice(
                            "use of undeclared AMIR temp in codegen",
                            self.temp_span(*temp_id),
                        );
                        return self.poison_i32();
                    }
                }
            }
            AmirOperand::Constant(c) => match c {
                AmirConstant::Bool(b) => {
                    let imm = if *b { 1 } else { 0 };
                    self.builder
                        .ins()
                        .iconst(cranelift_codegen::ir::types::I8, imm)
                }
                AmirConstant::Nil => {
                    // Prefer the expected ABI type so `int?`/`Point?`/`Err?` compares
                    // against a zero of the same width as the left-hand side.
                    let ty = expected_ty.unwrap_or(cranelift_codegen::ir::types::I32);
                    self.builder.ins().iconst(ty, 0)
                }
                AmirConstant::Pool(lit_id) => {
                    let entry = self.literal_pool.get(*lit_id);
                    match entry {
                        arandu_semantics::literal_pool::AmirLiteralEntry::Int(s) => {
                            let val = match arandu_semantics::literal_pool::parse_int_literal(s) {
                                Some(v) => v as i64,
                                None => {
                                    self.record_ice(
                                        format!(
                                            "invalid integer literal in AMIR literal pool: '{s}'"
                                        ),
                                        self.func_span(),
                                    );
                                    return self.poison_i32();
                                }
                            };
                            let ty = match expected_ty {
                                Some(t) if t.is_int() => t,
                                _ => cranelift_codegen::ir::types::I32,
                            };
                            self.builder.ins().iconst(ty, val)
                        }
                        arandu_semantics::literal_pool::AmirLiteralEntry::Float(s) => {
                            let val = match arandu_semantics::literal_pool::parse_float_literal(s) {
                                Some(v) => v,
                                None => {
                                    self.record_ice(
                                        format!(
                                            "invalid float literal in AMIR literal pool: '{s}'"
                                        ),
                                        self.func_span(),
                                    );
                                    return self.poison_i32();
                                }
                            };
                            let ty = match expected_ty {
                                Some(t) if t.is_float() => t,
                                _ => cranelift_codegen::ir::types::F64,
                            };
                            if ty == cranelift_codegen::ir::types::F32 {
                                self.builder.ins().f32const(val as f32)
                            } else {
                                self.builder.ins().f64const(val)
                            }
                        }
                        arandu_semantics::literal_pool::AmirLiteralEntry::Str(s) => {
                            let str_bytes = s.as_bytes();
                            let data_id = match self.module.declare_data(
                                &format!("str_lit_{}", lit_id.0),
                                cranelift_module::Linkage::Local,
                                false,
                                false,
                            ) {
                                Ok(data_id) => data_id,
                                Err(err) => {
                                    self.record_ice(
                                        format!(
                                            "failed to declare string literal in JIT module: {err:?}"
                                        ),
                                        self.func_span(),
                                    );
                                    return self.poison_i32();
                                }
                            };
                            let mut data_ctx = cranelift_module::DataDescription::new();
                            data_ctx.define(str_bytes.to_vec().into_boxed_slice());
                            let _ = self.module.define_data(data_id, &data_ctx);
                            let local_data_ref =
                                self.module.declare_data_in_func(data_id, self.builder.func);
                            self.builder
                                .ins()
                                .symbol_value(self.ptr_type, local_data_ref)
                        }
                        arandu_semantics::literal_pool::AmirLiteralEntry::Char(s) => {
                            let val = s.chars().next().unwrap_or('\0') as i64;
                            self.builder
                                .ins()
                                .iconst(cranelift_codegen::ir::types::I32, val)
                        }
                    }
                }
            },
            AmirOperand::FunctionRef(sym_id) => {
                let sym = self.symbol_table.get(*sym_id);
                let host_name = self.symbol_table.host_func_name(sym);
                let func_id = match self.func_ids.get(host_name) {
                    Some(func_id) => *func_id,
                    None => {
                        self.record_ice(
                            format!("function '{}' was not declared in the JIT module", sym.name),
                            sym.span,
                        );
                        return self.poison_i32();
                    }
                };
                let local_ref = self.module.declare_func_in_func(func_id, self.builder.func);
                self.builder.ins().func_addr(self.ptr_type, local_ref)
            }
            AmirOperand::GlobalRef(symbol) => {
                let symbol = self.symbol_table.get(*symbol);
                self.record_ice(
                    format!(
                        "global reference '{}' reached Cranelift value translation; global operands are unsupported",
                        symbol.name
                    ),
                    symbol.span,
                );
                self.poison_i32()
            }
        };

        if let Some(target_ty) = expected_ty {
            let val_ty = self.builder.func.dfg.value_type(val);
            if val_ty != target_ty {
                if val_ty.is_int() && target_ty.is_int() {
                    if val_ty.bits() < target_ty.bits() {
                        let is_unsigned = match operand {
                            AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                                let ar_ty = self.temp_ar_ty(*t);
                                crate::types::ar_type_is_unsigned_integer(&ar_ty)
                            }
                            _ => false,
                        };
                        if is_unsigned {
                            val = self.builder.ins().uextend(target_ty, val);
                        } else {
                            val = self.builder.ins().sextend(target_ty, val);
                        }
                    } else if val_ty.bits() > target_ty.bits() {
                        val = self.builder.ins().ireduce(target_ty, val);
                    }
                } else if val_ty.is_int() && target_ty.is_float() {
                    let is_unsigned = match operand {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                            let ar_ty = self.temp_ar_ty(*t);
                            crate::types::ar_type_is_unsigned_integer(&ar_ty)
                        }
                        _ => false,
                    };
                    if is_unsigned {
                        val = self.builder.ins().fcvt_from_uint(target_ty, val);
                    } else {
                        val = self.builder.ins().fcvt_from_sint(target_ty, val);
                    }
                } else if val_ty.is_float() && target_ty.is_int() {
                    let is_unsigned = match operand {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                            let ar_ty = self.temp_ar_ty(*t);
                            crate::types::ar_type_is_unsigned_integer(&ar_ty)
                        }
                        _ => false,
                    };
                    if is_unsigned {
                        val = self.builder.ins().fcvt_to_uint(target_ty, val);
                    } else {
                        val = self.builder.ins().fcvt_to_sint(target_ty, val);
                    }
                } else if val_ty.is_float() && target_ty.is_float() {
                    if val_ty.bits() < target_ty.bits() {
                        val = self.builder.ins().fpromote(target_ty, val);
                    } else if val_ty.bits() > target_ty.bits() {
                        val = self.builder.ins().fdemote(target_ty, val);
                    }
                }
            }
        }

        val
    }
}
