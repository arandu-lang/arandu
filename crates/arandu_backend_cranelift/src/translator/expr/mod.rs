//! AMIR rvalue translation to Cranelift IR.

pub(super) mod aggregates;
pub(super) mod enums;
pub(super) mod gen_arena;
pub(super) mod memory;
pub(super) mod ops;
mod simd;

use arandu_middle::amir::{AmirOperand, AmirRvalue};
use arandu_middle::types::ArType;
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_rvalue(
        &mut self,
        rvalue: &AmirRvalue,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        let mut val = self.translate_rvalue_inner(rvalue, expected_ty, expected_ar_type);
        if let Some(target_ty) = expected_ty {
            let val_ty = self.builder.func.dfg.value_type(val);
            if val_ty != target_ty && val_ty.is_int() && target_ty.is_int() {
                if val_ty.bits() < target_ty.bits() {
                    let is_unsigned = expected_ar_type
                        .map(crate::types::ar_type_is_unsigned_integer)
                        .unwrap_or(false);
                    if is_unsigned {
                        val = self.builder.ins().uextend(target_ty, val);
                    } else {
                        val = self.builder.ins().sextend(target_ty, val);
                    }
                } else if val_ty.bits() > target_ty.bits() {
                    val = self.builder.ins().ireduce(target_ty, val);
                }
            }
        }
        val
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
            AmirRvalue::VectorCompare { .. }
            | AmirRvalue::VectorSelect { .. }
            | AmirRvalue::VectorOp { .. }
            | AmirRvalue::VectorSplat { .. }
            | AmirRvalue::VectorShuffle { .. }
            | AmirRvalue::VectorExtractLane { .. }
            | AmirRvalue::VectorInsertLane { .. }
            | AmirRvalue::VectorBitmask { .. }
            | AmirRvalue::VectorLoadUnaligned { .. } => self.translate_vector(rvalue, expected_ty),
            AmirRvalue::Select {
                condition,
                if_true,
                if_false,
            } => {
                let cond =
                    self.translate_operand(condition, Some(cranelift_codegen::ir::types::I8));
                let arm_true = self.translate_typed_operand(if_true, expected_ty, expected_ar_type);
                let arm_false =
                    self.translate_typed_operand(if_false, expected_ty, expected_ar_type);
                self.builder.ins().select(cond, arm_true, arm_false)
            }
            AmirRvalue::Use(op) => {
                let val = self.translate_typed_operand(op, expected_ty, expected_ar_type);
                let op_ty = self.get_operand_ar_type(op);
                if matches!(op, AmirOperand::Copy(_) | AmirOperand::Move(_))
                    && self.is_inline_aggregate_ty(&op_ty)
                {
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
                if input_ty.is_vector() {
                    let slot = self.builder.create_sized_stack_slot(
                        cranelift_codegen::ir::StackSlotData {
                            kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            size: 16,
                            align_shift: 4,
                            key: None,
                        },
                    );
                    self.builder
                        .ins()
                        .stack_store(self.ptr_type, input, slot, 0);
                    let address = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                    let Some(id) = self.resolve_function("ar_bench_black_box_ptr") else {
                        self.record_ice(
                            "missing vector black box runtime barrier",
                            self.func_span(),
                        );
                        return self.poison_i32();
                    };
                    let callee = self.module.declare_func_in_func(id, self.builder.func);
                    let call = self.builder.ins().call(callee, &[address]);
                    let address = self.builder.inst_results(call)[0];
                    return self.builder.ins().load(
                        input_ty,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        address,
                        0,
                    );
                }
                let pointer_like = expected_ar_type.is_some_and(|ty| {
                    matches!(
                        ty,
                        ArType::Ptr(_)
                            | ArType::Ref(_)
                            | ArType::RefMut(_)
                            | ArType::Named(_, _)
                            | ArType::Array(_, _)
                            | ArType::Tuple(_)
                            | ArType::Slice(_)
                            | ArType::MutSlice(_)
                    )
                });
                let helper = if input_ty == cranelift_codegen::ir::types::F32 {
                    "ar_bench_black_box_i64"
                } else if input_ty.is_float() {
                    "ar_bench_black_box_f64"
                } else if pointer_like {
                    "ar_bench_black_box_ptr"
                } else {
                    "ar_bench_black_box_i64"
                };
                let Some(id) = self.resolve_function(helper) else {
                    self.record_ice(format!("missing {helper} runtime import"), self.func_span());
                    return self.poison_i32();
                };
                let function = self.module.declare_func_in_func(id, self.builder.func);
                let call_input = if input_ty == cranelift_codegen::ir::types::F32 {
                    // Transfer bits through the integer barrier: promotion to
                    // f64 would change signaling NaNs and their payloads.
                    let bits = self.builder.ins().bitcast(
                        cranelift_codegen::ir::types::I32,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        input,
                    );
                    self.builder
                        .ins()
                        .uextend(cranelift_codegen::ir::types::I64, bits)
                } else if !pointer_like && input_ty.is_int() && input_ty.bits() < 64 {
                    self.builder
                        .ins()
                        .uextend(cranelift_codegen::ir::types::I64, input)
                } else {
                    input
                };
                let call = self.builder.ins().call(function, &[call_input]);
                let result = self.builder.inst_results(call)[0];
                if input_ty == cranelift_codegen::ir::types::F32 {
                    let bits = self
                        .builder
                        .ins()
                        .ireduce(cranelift_codegen::ir::types::I32, result);
                    self.builder.ins().bitcast(
                        input_ty,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        bits,
                    )
                } else if !pointer_like && input_ty.is_int() && input_ty.bits() < 64 {
                    self.builder.ins().ireduce(input_ty, result)
                } else {
                    result
                }
            }
            AmirRvalue::Binary { op, left, right } => {
                self.translate_binary(*op, left, right, expected_ty, expected_ar_type)
            }
            AmirRvalue::Unary { op, operand } => {
                self.translate_unary(*op, operand, expected_ty, expected_ar_type)
            }
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
                index,
                field_ty,
                tuple_ty,
                ..
            } => self.translate_enum_payload(value, *field_ty, *tuple_ty, *index, expected_ty),
            AmirRvalue::IndexAccess {
                base,
                index,
                in_bounds,
            } => self.translate_index_access(base, index, *in_bounds, expected_ty),
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                self.translate_borrow(place)
            }
            AmirRvalue::RelativeBorrow { local, .. } => self
                .builder
                .ins()
                .iconst(self.ptr_type, local.as_usize() as i64),
            AmirRvalue::Len(op) => self.translate_len(op, expected_ty),
            AmirRvalue::Alloc(op) => self.translate_alloc(op),
            AmirRvalue::ArenaAlloc { size, align, .. } => self.translate_arena_alloc(*size, *align),
            AmirRvalue::ArenaCheckpoint => self.translate_arena_checkpoint(),
            AmirRvalue::StaticAddr(symbol) => self.translate_static_addr(*symbol),
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
