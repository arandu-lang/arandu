//! AMIR rvalue translation to Cranelift IR.

pub(super) mod aggregates;
pub(super) mod enums;
pub(super) mod gen_arena;
pub(super) mod memory;
pub(super) mod ops;

use arandu_semantics::amir::{AmirConstant, AmirOperand, AmirRvalue};
use arandu_semantics::passes::type_checker::types::ArType;
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// Box a scalar into a heap cell for `T?` (null-or-pointer ABI).
    fn box_nullable_scalar(&mut self, val: Value, inner: &ArType) -> Value {
        let Some(malloc_id) = self.malloc_func_id() else {
            return self.poison_i32();
        };
        let malloc_ref = self
            .module
            .declare_func_in_func(malloc_id, self.builder.func);
        let layout = self.checked_layout(inner);
        let size = self
            .builder
            .ins()
            .iconst(self.ptr_type, layout.size.max(1) as i64);
        let call = self.builder.ins().call(malloc_ref, &[size]);
        let ptr = self.builder.inst_results(call)[0];
        self.builder
            .ins()
            .store(cranelift_codegen::ir::MemFlagsData::new(), val, ptr, 0);
        ptr
    }

    /// Load a boxed scalar from a non-null `T?` handle.
    fn unbox_nullable_scalar(&mut self, handle: Value, inner: &ArType) -> Value {
        let clif = match crate::types::clif_type(inner, self.ptr_type) {
            crate::types::ClifType::Concrete(t) => t,
            crate::types::ClifType::Void => return self.poison_i32(),
        };
        self.builder
            .ins()
            .load(clif, cranelift_codegen::ir::MemFlagsData::new(), handle, 0)
    }

    pub(super) fn translate_rvalue(
        &mut self,
        rvalue: &AmirRvalue,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        // ── Nullable handle ABI ──────────────────────────────────────────
        // `T?` is always a pointer: null = nil; non-null = object ptr or
        // boxed scalar. Box/unbox keeps `int? = 0` distinct from `nil`.
        if let Some(ArType::Nullable(inner_id)) = expected_ar_type {
            let inner = self.type_info.type_interner.resolve(*inner_id);
            if matches!(
                rvalue,
                AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil))
            ) {
                return self.builder.ins().iconst(self.ptr_type, 0);
            }
            // Already a nullable handle (copy/move or nested) → pass through.
            if let AmirRvalue::Use(op) = rvalue {
                let op_ty = self.get_operand_ar_type(op);
                if matches!(op_ty, ArType::Nullable(_)) {
                    return self.translate_operand(op, Some(self.ptr_type));
                }
            }
            // Produce the inner value, then box scalars.
            let inner_clif = match crate::types::clif_type(&inner, self.ptr_type) {
                crate::types::ClifType::Concrete(t) => Some(t),
                crate::types::ClifType::Void => None,
            };
            let raw = self.translate_rvalue_inner(rvalue, inner_clif, Some(&inner));
            if inner.needs_nullable_box() {
                return self.box_nullable_scalar(raw, &inner);
            }
            return raw;
        }

        // Unbox when assigning a `T?` handle into a non-nullable `T` (e.g. `??`).
        if let AmirRvalue::Use(op) = rvalue {
            let op_ty = self.get_operand_ar_type(op);
            if let ArType::Nullable(inner_id) = &op_ty {
                let inner = self.type_info.type_interner.resolve(*inner_id);
                if expected_ar_type.is_none_or(|e| !matches!(e, ArType::Nullable(_)))
                    && inner.needs_nullable_box()
                {
                    let handle = self.translate_operand(op, Some(self.ptr_type));
                    return self.unbox_nullable_scalar(handle, &inner);
                }
            }
        }

        self.translate_rvalue_inner(rvalue, expected_ty, expected_ar_type)
    }

    fn translate_rvalue_inner(
        &mut self,
        rvalue: &AmirRvalue,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        match rvalue {
            AmirRvalue::Use(op) => {
                let val = self.translate_operand(op, expected_ty);
                let op_ty = self.get_operand_ar_type(op);
                if matches!(op, AmirOperand::Copy(_)) && self.is_named_struct_ty(&op_ty) {
                    return self.materialize_ptr_read_copy(val, &op_ty).unwrap_or(val);
                }
                val
            }
            AmirRvalue::SliceView { data, len, .. } => self.translate_slice_view(data, len),
            AmirRvalue::SliceSubslice { slice, start, len } => {
                self.translate_slice_subslice(slice, start, len, expected_ar_type)
            }
            AmirRvalue::SliceData(slice) => self.translate_slice_data(slice),
            AmirRvalue::StrBytes { source } => self.translate_str_bytes(source),
            AmirRvalue::StrView { owner } => self.translate_operand(owner, Some(self.ptr_type)),
            AmirRvalue::BlackBox { value, .. } => {
                let input = self.translate_operand(value, expected_ty);
                let input_ty = self.builder.func.dfg.value_type(input);
                let pointer_like = expected_ar_type.is_some_and(|ty| {
                    matches!(
                        ty,
                        ArType::Ptr(_)
                            | ArType::Ref(_)
                            | ArType::RefMut(_)
                            | ArType::Nullable(_)
                            | ArType::Named(_, _)
                            | ArType::Array(_, _)
                            | ArType::Tuple(_)
                            | ArType::Slice(_)
                    )
                });
                let helper = if input_ty.is_float() {
                    "ar_bench_black_box_f64"
                } else if pointer_like {
                    "ar_bench_black_box_ptr"
                } else {
                    "ar_bench_black_box_i64"
                };
                let Some(id) = self.func_ids.get(helper).copied() else {
                    self.record_ice(format!("missing {helper} runtime import"), self.func_span());
                    return self.poison_i32();
                };
                let function = self.module.declare_func_in_func(id, self.builder.func);
                let call_input = if !pointer_like && input_ty.is_int() && input_ty.bits() < 64 {
                    self.builder
                        .ins()
                        .uextend(cranelift_codegen::ir::types::I64, input)
                } else {
                    input
                };
                let call = self.builder.ins().call(function, &[call_input]);
                let result = self.builder.inst_results(call)[0];
                if !pointer_like && input_ty.is_int() && input_ty.bits() < 64 {
                    self.builder.ins().ireduce(input_ty, result)
                } else {
                    result
                }
            }
            AmirRvalue::Binary { op, left, right } => {
                self.translate_binary(*op, left, right, expected_ty)
            }
            AmirRvalue::Unary { op, operand } => self.translate_unary(*op, operand, expected_ty),
            AmirRvalue::Load(place) => self.translate_load(place, expected_ty, expected_ar_type),
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => self.translate_struct_literal(struct_symbol, fields, expected_ar_type),
            AmirRvalue::Tuple { items } => self.translate_tuple(items, expected_ar_type),
            AmirRvalue::Array { items } => self.translate_array(items, expected_ar_type),
            AmirRvalue::FieldAccess { base, field } => {
                self.translate_field_access(base, *field, expected_ty, expected_ar_type)
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => self.translate_enum_construct(*variant_tag, payload.as_ref(), expected_ar_type),
            AmirRvalue::Discriminant { value } => self.translate_discriminant(value),
            AmirRvalue::EnumPayload {
                value,
                variant,
                index,
            } => self.translate_enum_payload(value, variant, *index, expected_ty),
            AmirRvalue::IndexAccess { base, index } => {
                self.translate_index_access(base, index, expected_ty)
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                self.translate_borrow(place)
            }
            AmirRvalue::RelativeBorrow { local, .. } => self
                .builder
                .ins()
                .iconst(self.ptr_type, local.as_usize() as i64),
            AmirRvalue::Len(op) => self.translate_len(op, expected_ty),
            AmirRvalue::Alloc(op) => self.translate_alloc(op),
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack,
            } => self.translate_coroutine_ready(value, *payload_ty, *stack),
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => self.translate_gen_insert(value, *payload_ty),
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => self.translate_gen_read("ar_gen_get_raw", gen_ref, *payload_ty),
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            } => self.translate_gen_write("ar_gen_set_raw", gen_ref, value, *payload_ty, false),
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => self.translate_gen_write("ar_gen_upsert_raw", gen_ref, value, *payload_ty, true),
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => self.translate_gen_read("ar_gen_remove_raw", gen_ref, *payload_ty),
            AmirRvalue::ToStr { .. } | AmirRvalue::StringInterp { .. } => {
                self.record_ice(
                    "ToStr/StringInterp must be lowered via str rvalue path",
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }
}
