use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirRvalue};
use arandu_middle::types::ArType;

mod aggregate;
mod binary;
mod composite;
mod memory;
mod use_arm;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_rvalue(
        &mut self,
        rvalue: &AmirRvalue,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        match rvalue {
            AmirRvalue::VectorCompare { .. }
            | AmirRvalue::VectorSelect { .. }
            | AmirRvalue::VectorOp { .. }
            | AmirRvalue::VectorSplat { .. }
            | AmirRvalue::VectorShuffle { .. }
            | AmirRvalue::VectorExtractLane { .. }
            | AmirRvalue::VectorInsertLane { .. } => {
                self.emit_vector_rvalue(rvalue, func, expected_ar_type)
            }
            AmirRvalue::Use(op) => {
                self.emit_use_rvalue(op, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::BlackBox { value, .. } => {
                self.emit_black_box(value, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::Binary { op, left, right } => {
                self.emit_binary_rvalue(op, left, right, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::FieldAccess { base, field } => {
                self.emit_field_access(base, field, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::Discriminant { value } => {
                self.emit_discriminant(value, func);
            }
            AmirRvalue::EnumPayload {
                value,
                index,
                tuple_ty,
                ..
            } => {
                self.emit_enum_payload(value, index, tuple_ty, func, expected_c_type);
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                self.emit_enum_construct(
                    variant_tag,
                    payload,
                    func,
                    expected_ar_type,
                    expected_c_type,
                );
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                self.emit_struct_literal(
                    struct_symbol,
                    fields,
                    func,
                    expected_ar_type,
                    expected_c_type,
                );
            }
            AmirRvalue::Unary { op, operand } => {
                self.emit_unary_rvalue(op, operand, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::Load(place) => {
                self.emit_load(place, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::Borrow(place) => {
                self.emit_borrow(place, func, expected_ar_type);
            }
            AmirRvalue::BorrowMut(place) => {
                self.emit_borrow_mut(place, func, expected_ar_type);
            }
            AmirRvalue::Array { items } => {
                self.emit_array_rvalue(items, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::Tuple { items } => {
                self.emit_tuple_rvalue(items, func, expected_ar_type, expected_c_type);
            }
            AmirRvalue::Len(op) => {
                self.emit_len(op, func);
            }
            AmirRvalue::SliceView { data, len, .. } => {
                self.emit_slice_view(data, len, func, expected_c_type);
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                self.emit_slice_subslice(
                    slice,
                    start,
                    len,
                    func,
                    expected_ar_type,
                    expected_c_type,
                );
            }
            AmirRvalue::SliceData(slice) => {
                self.emit_slice_data(slice, func, expected_c_type);
            }
            AmirRvalue::StrBytes { source } => {
                self.emit_str_bytes(source, func, expected_c_type);
            }
            AmirRvalue::StrView { owner } => {
                self.emit_str_view(owner, func, expected_c_type);
            }
            AmirRvalue::IndexAccess {
                base,
                index,
                in_bounds,
            } => {
                self.emit_index_access(base, index, *in_bounds, func);
            }
            AmirRvalue::Alloc(op) => {
                self.emit_alloc(op, func);
            }
            // Heap CoroutineReady (stack:true is multi-stmt in emit_stmt).
            // A3.6 layout: disc@0 (u32 Ready=0), payload@8.
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack: false,
            } => {
                self.emit_coroutine_ready_heap(value, payload_ty, func);
            }
            AmirRvalue::CoroutineReady { stack: true, .. } => {
                self.emit_coroutine_ready_stack();
            }
            // A3.4: pin-free index (LocalId), not a raw address.
            AmirRvalue::RelativeBorrow { local, .. } => {
                self.emit_relative_borrow(local);
            }
            AmirRvalue::GenInsert { .. }
            | AmirRvalue::GenGet { .. }
            | AmirRvalue::GenSet { .. }
            | AmirRvalue::GenUpsert { .. }
            | AmirRvalue::GenRemove { .. } => {
                self.emit_gen_ref_rvalue();
            }
            AmirRvalue::StringInterp { parts } => {
                self.emit_string_interp(parts, func);
            }
            AmirRvalue::ToStr { value, src_ty } => {
                self.emit_to_str(value, src_ty, func);
            }
        }
    }
}
