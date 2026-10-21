use super::super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirPlace, LocalId};
use arandu_middle::types::{ArType, Primitive};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_load(
        &mut self,
        place: &AmirPlace,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        if self.checked_layout(expected_ar_type).size == 0 {
            // C represents a source ZST with a one-byte placeholder.
            // Loading a final ZST field must not read beyond its
            // containing allocation's target-computed layout.
            let _ = write!(&mut self.output, "({expected_c_type}){{0}}");
            return;
        }
        let place_str = self.format_place(place, func);
        let _ = write!(&mut self.output, "{}", place_str);
    }

    pub(super) fn emit_borrow(
        &mut self,
        place: &AmirPlace,
        func: &AmirFunc,
        expected_ar_type: &ArType,
    ) {
        let place_str = self.format_place(place, func);
        if expected_ar_type.is_borrowed_slice_abi(self.interner) {
            let _ = write!(&mut self.output, "{}", place_str);
        } else {
            let _ = write!(&mut self.output, "&{}", place_str);
        }
    }

    pub(super) fn emit_borrow_mut(
        &mut self,
        place: &AmirPlace,
        func: &AmirFunc,
        expected_ar_type: &ArType,
    ) {
        let place_str = self.format_place(place, func);
        if expected_ar_type.is_borrowed_slice_abi(self.interner) {
            let _ = write!(&mut self.output, "{}", place_str);
        } else {
            let _ = write!(&mut self.output, "&{}", place_str);
        }
    }

    pub(super) fn emit_len(&mut self, op: &AmirOperand, func: &AmirFunc) {
        let op_str = self.format_operand(op, func);
        let op_ty = match op {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
            _ => ArType::Error,
        };
        if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
            // LayoutEngine Str fat pointer: second field is len.
            let _ = write!(&mut self.output, "({}).len", op_str);
        } else if op_ty.slice_abi_element(self.interner).is_some() {
            // Slice fat pointer: len offset from LayoutEngine (not magic +8).
            let off = self.layout.fat_ptr_len_offset();
            let len_ty = if self.layout.fat_ptr_len_size() == 4 {
                "int32_t"
            } else {
                "int64_t"
            };
            let _ = write!(
                &mut self.output,
                "({{ {len_ty} _len = 0; memcpy(&_len, (uint8_t*)&{op_str} + {off}, sizeof(_len)); _len; }})"
            );
        } else if let ArType::Array(len, _) = op_ty {
            let _ = write!(&mut self.output, "{}", len);
        } else {
            self.record_codegen_ice(
                func,
                format!("Len reached C codegen for unsupported type {op_ty:?}"),
            );
            let _ = write!(&mut self.output, "/* invalid Len operand */ 0");
        }
    }

    pub(super) fn emit_slice_view(
        &mut self,
        data: &AmirOperand,
        len: &AmirOperand,
        func: &AmirFunc,
        expected_c_type: &str,
    ) {
        let data = self.format_operand(data, func);
        let len = self.format_operand(len, func);
        let off = self.layout.fat_ptr_len_offset();
        let len_ty = if self.layout.fat_ptr_len_size() == 4 {
            "int32_t"
        } else {
            "int64_t"
        };
        let _ = write!(
            &mut self.output,
            "({{ {expected_c_type} view = {{0}}; void* _ptr = (void*)({data}); {len_ty} _len = ({len_ty})({len}); memcpy((uint8_t*)&view + 0, &_ptr, sizeof(_ptr)); memcpy((uint8_t*)&view + {off}, &_len, sizeof(_len)); view; }})"
        );
    }

    pub(super) fn emit_slice_subslice(
        &mut self,
        slice: &AmirOperand,
        start: &AmirOperand,
        len: &AmirOperand,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        let slice = self.format_operand(slice, func);
        let start = self.format_operand(start, func);
        let len = self.format_operand(len, func);
        let off = self.layout.fat_ptr_len_offset();
        let len_ty = if self.layout.fat_ptr_len_size() == 4 {
            "int32_t"
        } else {
            "int64_t"
        };
        let elem_c = match expected_ar_type {
            ArType::Slice(inner) | ArType::MutSlice(inner) => {
                self.format_type(&self.interner.resolve(*inner))
            }
            _ => std::borrow::Cow::Borrowed("uint8_t"),
        };
        let _ = write!(
            &mut self.output,
            "({{ {expected_c_type} view = {{0}}; void* _orig_ptr = 0; memcpy(&_orig_ptr, (uint8_t*)&{slice} + 0, sizeof(_orig_ptr)); void* _ptr = (void*)((( {elem_c}*)_orig_ptr) + ({start})); {len_ty} _len = ({len_ty})({len}); memcpy((uint8_t*)&view + 0, &_ptr, sizeof(_ptr)); memcpy((uint8_t*)&view + {off}, &_len, sizeof(_len)); view; }})"
        );
    }

    pub(super) fn emit_slice_data(
        &mut self,
        slice: &AmirOperand,
        func: &AmirFunc,
        expected_c_type: &str,
    ) {
        let slice = self.format_operand(slice, func);
        let _ = write!(
            &mut self.output,
            "({{ void* _ptr = 0; memcpy(&_ptr, (uint8_t*)&{slice} + 0, sizeof(_ptr)); ({expected_c_type})_ptr; }})"
        );
    }

    pub(super) fn emit_str_bytes(
        &mut self,
        source: &AmirOperand,
        func: &AmirFunc,
        expected_c_type: &str,
    ) {
        let source = self.format_operand(source, func);
        let off = self.layout.fat_ptr_len_offset();
        let len_ty = if self.layout.fat_ptr_len_size() == 4 {
            "int32_t"
        } else {
            "int64_t"
        };
        let _ = write!(
            &mut self.output,
            "({{ {expected_c_type} view = {{0}}; void* _ptr = (void*)({source}).ptr; {len_ty} _len = ({len_ty})({source}).len; memcpy((uint8_t*)&view + 0, &_ptr, sizeof(_ptr)); memcpy((uint8_t*)&view + {off}, &_len, sizeof(_len)); view; }})"
        );
    }

    pub(super) fn emit_str_view(
        &mut self,
        owner: &AmirOperand,
        func: &AmirFunc,
        expected_c_type: &str,
    ) {
        let owner = self.format_operand(owner, func);
        let _ = write!(&mut self.output, "({expected_c_type})({owner})");
    }

    pub(super) fn emit_index_access(
        &mut self,
        base: &AmirOperand,
        index: &AmirOperand,
        func: &AmirFunc,
    ) {
        let base_ty = match base {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
            _ => ArType::Error,
        };
        let deref_ty = match &base_ty {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                self.interner.resolve(*inner)
            }
            other => other.clone(),
        };
        let is_vec = arandu_middle::types::is_vec_type(&deref_ty, self.symbols);
        let elem_ty =
            match arandu_middle::types::index_elem_type(&deref_ty, self.symbols, self.interner) {
                Some(id) => self.interner.resolve(id),
                None => ArType::Error,
            };
        let elem_c_ty = self.format_type(&elem_ty);
        let base_str = self.format_operand(base, func);
        let index_str = self.format_operand(index, func);

        let is_nonempty_array = matches!(&deref_ty, ArType::Array(len, _) if *len > 0)
            && self.checked_layout(&deref_ty).size > 0;

        if is_nonempty_array {
            if matches!(base_ty, ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)) {
                let _ = write!(&mut self.output, "({})->data[{}]", base_str, index_str);
            } else {
                let _ = write!(&mut self.output, "({}).data[{}]", base_str, index_str);
            }
        } else if matches!(base_ty, ArType::Ptr(_)) {
            let _ = write!(
                &mut self.output,
                "(({}*){})[{}]",
                elem_c_ty, base_str, index_str
            );
        } else if base_ty.slice_abi_element(self.interner).is_some() || is_vec {
            let _ = write!(
                &mut self.output,
                "(({}*)(*(void**)((uint8_t*)&{} + 0)))[{}]",
                elem_c_ty, base_str, index_str
            );
        } else {
            let _ = write!(
                &mut self.output,
                "(({}*)&{})[{}]",
                elem_c_ty, base_str, index_str
            );
        }
    }

    pub(super) fn emit_alloc(&mut self, op: &AmirOperand, func: &AmirFunc) {
        // Byte-count allocation via libc malloc (RC-RVALUE-GAPS).
        let size_str = self.format_operand(op, func);
        let _ = write!(&mut self.output, "malloc((size_t)({}))", size_str);
    }

    pub(super) fn emit_relative_borrow(&mut self, local: &LocalId) {
        let _ = write!(&mut self.output, "((void*)(uintptr_t){})", local.as_usize());
    }
}
