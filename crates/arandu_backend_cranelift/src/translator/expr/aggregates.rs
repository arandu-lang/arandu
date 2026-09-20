//! Aggregate construction and member/index access for structs, tuples, and arrays.

use arandu_semantics::amir::AmirOperand;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive, is_vec_type};
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_struct_literal(
        &mut self,
        struct_symbol: &arandu_semantics::SymbolId,
        fields: &[(arandu_semantics::SmolStr, AmirOperand)],
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        let pointer_width = self.ptr_type.bytes() as u64;
        let struct_ty = expected_ar_type.cloned().unwrap_or_else(|| {
            arandu_semantics::types::ArType::named(
                *struct_symbol,
                &[],
                &self.type_info.type_interner,
            )
        });
        let layout = self.checked_layout(&struct_ty);

        let ptr_val = self.call_malloc(layout.size as u32);

        for (i, (name, op)) in fields.iter().enumerate() {
            let field_idx = self
                .type_info
                .struct_fields
                .get(struct_symbol)
                .and_then(|m| m.get(name.as_str()))
                .map(|f| f.index)
                .unwrap_or(i);
            let offset = layout.field_offsets.get(field_idx).copied().unwrap_or(0) as i32;
            let op_ty = self.get_operand_ar_type(op);
            if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                let (elem_ptr, elem_len) = self.translate_str_operand(op);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_ptr,
                    ptr_val,
                    offset,
                );
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_len,
                    ptr_val,
                    offset + pointer_width as i32,
                );
            } else if matches!(op_ty, ArType::Slice(_)) {
                let (elem_ptr, elem_len) = self.translate_slice_operand(op);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_ptr,
                    ptr_val,
                    offset,
                );
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_len,
                    ptr_val,
                    offset + pointer_width as i32,
                );
            } else {
                let field_ty = arandu_semantics::layout::instantiated_field_type(
                    &struct_ty,
                    name.as_str(),
                    &self.type_info.type_interner,
                    self.type_info,
                )
                .map(|id| self.type_info.type_interner.resolve(id))
                .unwrap_or_else(|| {
                    let field_defs = self.type_info.struct_fields.get(struct_symbol);
                    field_defs
                        .and_then(|m| m.get(name.as_str()))
                        .map(|f| self.type_info.type_interner.resolve(f.ty))
                        .unwrap_or_else(|| self.get_operand_ar_type(op))
                });
                let field_layout = self.checked_layout(&field_ty);
                if field_layout.size == 0 {
                    continue;
                }
                let is_aggregate = matches!(
                    field_ty,
                    ArType::Named(..)
                        | ArType::Array(..)
                        | ArType::ConstArray(..)
                        | ArType::Tuple(..)
                );
                if is_aggregate && let Some(memcpy_id) = self.memcpy_func_id() {
                    let val = self.translate_operand(op, Some(self.ptr_type));
                    let memcpy_ref = self
                        .module
                        .declare_func_in_func(memcpy_id, self.builder.func);
                    let size_val = self
                        .builder
                        .ins()
                        .iconst(self.ptr_type, field_layout.size as i64);
                    let field_dest = if offset != 0 {
                        self.builder.ins().iadd_imm_s(ptr_val, i64::from(offset))
                    } else {
                        ptr_val
                    };
                    self.builder
                        .ins()
                        .call(memcpy_ref, &[field_dest, val, size_val]);
                    continue;
                }
                let expected_field_ty = match crate::types::clif_type(&field_ty, self.ptr_type) {
                    crate::types::ClifType::Concrete(ty) => Some(ty),
                    crate::types::ClifType::Void => None,
                };
                let val = self.translate_operand(op, expected_field_ty);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    val,
                    ptr_val,
                    offset,
                );
            }
        }
        ptr_val
    }

    pub(super) fn translate_tuple(
        &mut self,
        items: &[AmirOperand],
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        let pointer_width = self.ptr_type.bytes() as u64;
        let tuple_ty = expected_ar_type.cloned().unwrap_or(ArType::Error);
        let layout = self.checked_layout(&tuple_ty);

        let ptr_val = self.call_malloc(layout.size as u32);

        for (i, op) in items.iter().enumerate() {
            let offset = layout.field_offsets.get(i).copied().unwrap_or(0) as i32;
            let op_ty = self.get_operand_ar_type(op);
            if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                let (elem_ptr, elem_len) = self.translate_str_operand(op);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_ptr,
                    ptr_val,
                    offset,
                );
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_len,
                    ptr_val,
                    offset + pointer_width as i32,
                );
            } else {
                let elem_ty = match &tuple_ty {
                    ArType::Tuple(tids) => self
                        .type_info
                        .type_interner
                        .type_args(*tids)
                        .get(i)
                        .map(|&tid| self.type_info.type_interner.resolve(tid))
                        .unwrap_or(ArType::Error),
                    _ => ArType::Error,
                };
                let is_aggregate = matches!(
                    elem_ty,
                    ArType::Named(..)
                        | ArType::Array(..)
                        | ArType::ConstArray(..)
                        | ArType::Tuple(..)
                );
                let elem_layout = self.checked_layout(&elem_ty);
                if is_aggregate
                    && elem_layout.size > 0
                    && let Some(memcpy_id) = self.memcpy_func_id()
                {
                    let val = self.translate_operand(op, Some(self.ptr_type));
                    let memcpy_ref = self
                        .module
                        .declare_func_in_func(memcpy_id, self.builder.func);
                    let size_val = self
                        .builder
                        .ins()
                        .iconst(self.ptr_type, elem_layout.size as i64);
                    let elem_dest = if offset != 0 {
                        self.builder.ins().iadd_imm_s(ptr_val, i64::from(offset))
                    } else {
                        ptr_val
                    };
                    self.builder
                        .ins()
                        .call(memcpy_ref, &[elem_dest, val, size_val]);
                    continue;
                }
                let expected_elem_ty = match crate::types::clif_type(&elem_ty, self.ptr_type) {
                    crate::types::ClifType::Concrete(ty) => Some(ty),
                    crate::types::ClifType::Void => None,
                };
                let val = self.translate_operand(op, expected_elem_ty);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    val,
                    ptr_val,
                    offset,
                );
            }
        }
        ptr_val
    }

    pub(super) fn translate_array(
        &mut self,
        items: &[AmirOperand],
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        let pointer_width = self.ptr_type.bytes() as u64;
        let array_ty = expected_ar_type.cloned().unwrap_or(ArType::Error);
        let _ = self.checked_layout(&array_ty);

        let item_ar_ty = match &array_ty {
            ArType::Array(_, inner) => self.type_info.resolve_type_id(*inner),
            _ => ArType::Error,
        };
        let item_layout = self.checked_layout(&item_ar_ty);
        let item_size = item_layout.size as i32;

        let total_bytes = items.len() * item_size as usize;
        let ptr_val = self.call_malloc(total_bytes as u32);

        for (i, op) in items.iter().enumerate() {
            let offset = i as i32 * item_size;
            let op_ty = self.get_operand_ar_type(op);
            if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                let (elem_ptr, elem_len) = self.translate_str_operand(op);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_ptr,
                    ptr_val,
                    offset,
                );
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    elem_len,
                    ptr_val,
                    offset + pointer_width as i32,
                );
            } else {
                let is_aggregate = matches!(
                    item_ar_ty,
                    ArType::Named(..)
                        | ArType::Array(..)
                        | ArType::ConstArray(..)
                        | ArType::Tuple(..)
                );
                if is_aggregate
                    && item_layout.size > 0
                    && let Some(memcpy_id) = self.memcpy_func_id()
                {
                    let val = self.translate_operand(op, Some(self.ptr_type));
                    let memcpy_ref = self
                        .module
                        .declare_func_in_func(memcpy_id, self.builder.func);
                    let size_val = self
                        .builder
                        .ins()
                        .iconst(self.ptr_type, item_layout.size as i64);
                    let elem_dest = if offset != 0 {
                        self.builder.ins().iadd_imm_s(ptr_val, i64::from(offset))
                    } else {
                        ptr_val
                    };
                    self.builder
                        .ins()
                        .call(memcpy_ref, &[elem_dest, val, size_val]);
                    continue;
                }
                let expected_item_ty = match crate::types::clif_type(&item_ar_ty, self.ptr_type) {
                    crate::types::ClifType::Concrete(ty) => Some(ty),
                    crate::types::ClifType::Void => None,
                };
                let val = self.translate_operand(op, expected_item_ty);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    val,
                    ptr_val,
                    offset,
                );
            }
        }
        ptr_val
    }

    pub(super) fn translate_field_access(
        &mut self,
        base: &AmirOperand,
        field: usize,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        let ptr_val = self.translate_operand(base, Some(self.ptr_type));
        let base_ty = match base {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => self.temp_ar_ty(*temp_id),
            _ => arandu_semantics::types::ArType::Error,
        };
        // Unwrap ptr / ref / nullable so layout sees the struct/tuple payload.
        // `shared`/`mut self` formals are `&T`/`&mut T` (pointer-sized SSA).
        let struct_ty = match base_ty {
            arandu_semantics::types::ArType::Ptr(inner)
            | arandu_semantics::types::ArType::Ref(inner)
            | arandu_semantics::types::ArType::RefMut(inner)
            | arandu_semantics::types::ArType::Nullable(inner) => {
                self.type_info.resolve_type_id(inner)
            }
            other => other,
        };
        let layout = self.checked_layout(&struct_ty);
        if let Some(exp_ar) = expected_ar_type {
            let exp_layout = self.checked_layout(exp_ar);
            if exp_layout.size == 0 {
                return self.builder.ins().iconst(self.ptr_type, 0);
            }
        }
        let Some(&off) = layout.field_offsets.get(field) else {
            // Dead `p?.field` access branch with nil/ZST base, or incomplete layout.
            return self.poison_i32();
        };
        let is_inline_aggregate = expected_ar_type
            .map(|exp| self.is_inline_aggregate_ty(exp))
            .unwrap_or_else(|| match &struct_ty {
                ArType::Tuple(elems) => self
                    .type_info
                    .type_interner
                    .type_args(*elems)
                    .get(field)
                    .map(|tid| self.is_inline_aggregate_ty(&self.type_info.resolve_type_id(*tid)))
                    .unwrap_or(false),
                ArType::Named(sym_id, _) => self
                    .type_info
                    .struct_fields
                    .get(sym_id)
                    .and_then(|fields| fields.fields.iter().find(|f| f.index == field))
                    .map(|f| self.is_inline_aggregate_ty(&self.type_info.resolve_type_id(f.ty)))
                    .unwrap_or(false),
                _ => false,
            });
        if is_inline_aggregate {
            let Ok(off_imm) = i64::try_from(off) else {
                self.record_ice(
                    "aggregate field offset does not fit the Cranelift immediate",
                    self.func_span(),
                );
                return self.poison_i32();
            };
            return if off == 0 {
                ptr_val
            } else {
                self.builder.ins().iadd_imm_u(ptr_val, off_imm)
            };
        }
        let offset = off as i32;

        let clif_ty = expected_ty.unwrap_or(self.ptr_type);
        self.builder.ins().load(
            clif_ty,
            cranelift_codegen::ir::MemFlagsData::new(),
            ptr_val,
            offset,
        )
    }

    pub(super) fn translate_index_access(
        &mut self,
        base: &AmirOperand,
        index: &AmirOperand,
        expected_ty: Option<Type>,
    ) -> Value {
        let mut ptr_val = self.translate_operand(base, Some(self.ptr_type));
        let mut idx_val = self.translate_operand(index, Some(self.ptr_type));
        let idx_ty = self.builder.func.dfg.value_type(idx_val);
        if idx_ty != self.ptr_type {
            let index_ty = self.get_operand_ar_type(index);
            let is_signed = !crate::types::ar_type_is_unsigned_integer(&index_ty);
            idx_val = self.cast_int_width_signed(idx_val, self.ptr_type, is_signed);
        }

        let base_ty = self.get_operand_ar_type(base);
        let deref_ty = match &base_ty {
            ArType::Ptr(inner) => self.type_info.resolve_type_id(*inner),
            other => other.clone(),
        };
        let elem_ty = match &deref_ty {
            ArType::Array(len, elem) => {
                let len_val = self.builder.ins().iconst(self.ptr_type, *len as i64);
                let is_oob = self.builder.ins().icmp(
                    cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                    idx_val,
                    len_val,
                );
                self.builder
                    .ins()
                    .trapnz(is_oob, cranelift_codegen::ir::TrapCode::unwrap_user(1));
                self.type_info.resolve_type_id(*elem)
            }
            ArType::ConstArray(_, elem) => {
                let arr_layout = self.checked_layout(&deref_ty);
                let elem_ty = self.type_info.resolve_type_id(*elem);
                let elem_layout = self.checked_layout(&elem_ty);
                let len = arr_layout.size.checked_div(elem_layout.size).unwrap_or(0);
                let len_val = self.builder.ins().iconst(self.ptr_type, len as i64);
                let is_oob = self.builder.ins().icmp(
                    cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                    idx_val,
                    len_val,
                );
                self.builder
                    .ins()
                    .trapnz(is_oob, cranelift_codegen::ir::TrapCode::unwrap_user(1));
                elem_ty
            }
            ArType::Slice(elem) => {
                let len_val = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    self.ptr_type.bytes() as i32,
                );
                let is_oob = self.builder.ins().icmp(
                    cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                    idx_val,
                    len_val,
                );
                self.builder
                    .ins()
                    .trapnz(is_oob, cranelift_codegen::ir::TrapCode::unwrap_user(1));
                ptr_val = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                );
                self.type_info.resolve_type_id(*elem)
            }
            ArType::Named(_, args) if is_vec_type(&deref_ty, self.symbol_table) => {
                ptr_val = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                );
                self.type_info
                    .type_interner
                    .type_args(*args)
                    .first()
                    .copied()
                    .map(|tid| self.type_info.resolve_type_id(tid))
                    .unwrap_or(ArType::Error)
            }
            _ => ArType::Error,
        };

        let layout = self.checked_layout(&elem_ty);

        let elem_size = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
        let offset_val = self.builder.ins().imul(idx_val, elem_size);
        let target_ptr = self.builder.ins().iadd(ptr_val, offset_val);

        if self.is_inline_aggregate_ty(&elem_ty) {
            return target_ptr;
        }

        let clif_ty = expected_ty.unwrap_or(self.ptr_type);
        self.builder.ins().load(
            clif_ty,
            cranelift_codegen::ir::MemFlagsData::new(),
            target_ptr,
            0,
        )
    }
}
