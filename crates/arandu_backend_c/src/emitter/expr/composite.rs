use super::super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand};
use arandu_middle::types::{ArType, TypeId};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_array_rvalue(
        &mut self,
        items: &[AmirOperand],
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        let array_layout = self.checked_layout(expected_ar_type);
        if items.is_empty() || array_layout.size == 0 {
            let _ = write!(
                &mut self.output,
                "({{ {expected_c_type} _res = {{0}}; _res; }})"
            );
        } else if let ArType::Array(len, _) = expected_ar_type
            && *len > 0
        {
            let _ = write!(
                &mut self.output,
                "({{ {expected_c_type} _res = {{ .data = {{"
            );
            for (i, op) in items.iter().take(*len as usize).enumerate() {
                if i > 0 {
                    let _ = write!(&mut self.output, ", ");
                }
                let op_str = self.format_operand(op, func);
                let _ = write!(&mut self.output, "{}", op_str);
            }
            let _ = write!(&mut self.output, "}} }}; _res; }})");
        } else {
            let elem_ty = match expected_ar_type {
                ArType::Array(_, inner) => self.interner.resolve(*inner),
                _ => ArType::Error,
            };
            let elem_c_ty = self.format_type(&elem_ty);
            let _ = write!(
                &mut self.output,
                "({{ {expected_c_type} _res = {{0}}; {elem_c_ty} _arr[] = {{"
            );
            for (i, op) in items.iter().enumerate() {
                if i > 0 {
                    let _ = write!(&mut self.output, ", ");
                }
                let op_str = self.format_operand(op, func);
                let _ = write!(&mut self.output, "{}", op_str);
            }
            let _ = write!(
                &mut self.output,
                "}}; memcpy(&_res, _arr, sizeof(_arr) < sizeof(_res) ? sizeof(_arr) : sizeof(_res)); _res; }})"
            );
        }
    }

    pub(super) fn emit_tuple_rvalue(
        &mut self,
        items: &[AmirOperand],
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        if items.is_empty() {
            let _ = write!(
                &mut self.output,
                "({{ {expected_c_type} _res = {{0}}; _res; }})"
            );
        } else {
            let tys = match expected_ar_type {
                ArType::Tuple(tys) => self.interner.type_args(*tys),
                _ => Vec::new(),
            };
            let _ = write!(
                &mut self.output,
                "({{ {expected_c_type} _res = {{0}}; struct {{"
            );
            for (i, _) in items.iter().enumerate() {
                let field_ty = if i < tys.len() {
                    self.interner.resolve(tys[i])
                } else {
                    ArType::Error
                };
                let field_c_ty = self.format_type(&field_ty);
                let _ = write!(&mut self.output, " {} f_{};", field_c_ty, i);
            }
            let _ = write!(&mut self.output, "}} _s = {{");
            for (i, op) in items.iter().enumerate() {
                if i > 0 {
                    let _ = write!(&mut self.output, ", ");
                }
                let op_str = self.format_operand(op, func);
                let _ = write!(&mut self.output, "{}", op_str);
            }
            let _ = write!(
                &mut self.output,
                "}}; memcpy(&_res, &_s, sizeof(_s) < sizeof(_res) ? sizeof(_s) : sizeof(_res)); _res; }})"
            );
        }
    }

    pub(super) fn emit_coroutine_ready_heap(
        &mut self,
        value: &AmirOperand,
        payload_ty: &TypeId,
        func: &AmirFunc,
    ) {
        let payload_ar = self.interner.resolve(*payload_ty);
        let payload_c = self.format_type(&payload_ar);
        let v = self.format_operand(value, func);
        let payload_size = self.checked_layout(&payload_ar).size.max(1);
        let size = 8 + payload_size;
        let _ = write!(
            &mut self.output,
            "ar_co_make_ready_heap({size}, &({payload_c}){{{v}}}, sizeof({payload_c}))"
        );
    }

    pub(super) fn emit_coroutine_ready_stack(&mut self) {
        // Should have been handled as multi-stmt Assign; fallback null.
        let _ = write!(&mut self.output, "((void*)0)");
    }

    pub(super) fn emit_gen_ref_rvalue(&mut self) {
        unreachable!("GenRef operations must be lowered as statements via ar_gen_*_raw");
    }

    pub(super) fn emit_string_interp(&mut self, parts: &[AmirOperand], func: &AmirFunc) {
        // Emit a call to the runtime helper: ar_str_concat_n(n, part0, part1, ..., partN-1)
        // Each part must already be of type ArStr.
        let n = parts.len();
        let part_strs: Vec<String> = parts.iter().map(|p| self.format_operand(p, func)).collect();
        let _ = write!(
            &mut self.output,
            "ar_str_concat_n({}, {})",
            n,
            part_strs.join(", ")
        );
    }

    pub(super) fn emit_to_str(&mut self, value: &AmirOperand, src_ty: &TypeId, func: &AmirFunc) {
        use arandu_middle::types::{ArType, Primitive};
        let src = self.interner.resolve(*src_ty);
        let val = self.format_operand(value, func);
        match &src {
            ArType::Primitive(Primitive::Str) => {
                let _ = write!(&mut self.output, "{val}");
            }
            ArType::Primitive(Primitive::Bool) => {
                let _ = write!(&mut self.output, "ar_bool_to_str({val})");
            }
            ArType::Primitive(Primitive::Char) => {
                let _ = write!(&mut self.output, "ar_char_to_str((uint32_t)({val}))");
            }
            ArType::FloatLiteral => {
                let _ = write!(&mut self.output, "ar_f64_to_str((double)({val}))");
            }
            ArType::Primitive(p) if p.is_float() => {
                let _ = write!(&mut self.output, "ar_f64_to_str((double)({val}))");
            }
            ArType::IntLiteral => {
                let _ = write!(&mut self.output, "ar_i64_to_str((int64_t)({val}))");
            }
            ArType::Primitive(p) if p.is_integer() && p.is_signed() => {
                let _ = write!(&mut self.output, "ar_i64_to_str((int64_t)({val}))");
            }
            ArType::Primitive(p) if p.is_integer() => {
                let _ = write!(&mut self.output, "ar_u64_to_str((uint64_t)({val}))");
            }
            other => {
                self.record_codegen_ice(
                    func,
                    format!("ToStr reached C codegen for unsupported type {other:?}"),
                );
                let _ = write!(
                    &mut self.output,
                    "/* invalid ToStr source */ ar_str_pack((const uint8_t*)\"\", 0)"
                );
            }
        }
    }
}
