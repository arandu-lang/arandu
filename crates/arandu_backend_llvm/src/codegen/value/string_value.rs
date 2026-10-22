use arandu_middle::Diagnostic;
use arandu_middle::amir::AmirOperand;
use arandu_middle::types::{ArType, Primitive};

use super::super::{FuncEmitter, TypedValue, ice};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(in crate::codegen) fn emit_to_str(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
        src_ty: arandu_middle::types::TypeId,
    ) -> Result<(), Diagnostic> {
        let src = self.module.interner.resolve(src_ty);
        if matches!(src, ArType::Primitive(Primitive::Str)) {
            let value = self.operand(value, lhs_ty)?;
            return self.assign_coerced(result, value, lhs_ty);
        }
        let (helper, argument_ty) = match &src {
            ArType::Primitive(Primitive::Bool) => {
                ("ar_jit_bool_to_str", ArType::Primitive(Primitive::U8))
            }
            ArType::Primitive(Primitive::Char) => {
                ("ar_jit_char_to_str", ArType::Primitive(Primitive::U32))
            }
            ArType::FloatLiteral => ("ar_jit_f64_to_str", ArType::Primitive(Primitive::F64)),
            ArType::Primitive(p) if p.is_float() => {
                ("ar_jit_f64_to_str", ArType::Primitive(Primitive::F64))
            }
            ArType::IntLiteral => ("ar_jit_i64_to_str", ArType::Primitive(Primitive::I64)),
            ArType::Primitive(p) if p.is_integer() && p.is_signed() => {
                ("ar_jit_i64_to_str", ArType::Primitive(Primitive::I64))
            }
            ArType::Primitive(p) if p.is_integer() => {
                ("ar_jit_u64_to_str", ArType::Primitive(Primitive::U64))
            }
            ArType::Err => (
                "ar_jit_err_to_str",
                ArType::Ptr(self.module.interner.intern(ArType::Void)),
            ),
            other => {
                return Err(ice(format!(
                    "LLVM ToStr does not support source type {other:?}"
                )));
            }
        };
        let value = self.operand(value, &argument_ty)?;
        let len_slot = self.scratch_alloca(&ArType::Primitive(Primitive::I64))?;
        self.module.require_to_str_helper(helper)?;
        let pointer = self.new_value();
        self.writer.line(format!(
            "{pointer} = call ptr @{helper}({}, ptr {len_slot})",
            value.render()
        ));
        let length = self.new_value();
        self.writer
            .line(format!("{length} = load i64, ptr {len_slot}, align 8"));
        let fat = self.module.fat_ptr_llvm_ty();
        let len_ty = self.module.ptr_int_ty();
        let stored = if len_ty == "i64" {
            length
        } else {
            let truncated = self.new_value();
            self.writer
                .line(format!("{truncated} = trunc i64 {length} to i32"));
            truncated
        };
        let first = self.new_value();
        self.writer.line(format!(
            "{first} = insertvalue {fat} undef, ptr {pointer}, 0"
        ));
        self.writer.line(format!(
            "{result} = insertvalue {fat} {first}, {len_ty} {stored}, 1"
        ));
        Ok(())
    }

    pub(in crate::codegen) fn emit_string_interp(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        parts: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        // Match Cranelift: always return a fresh malloc buffer. AMIR frees each
        // input part *and* the interp result; aliasing a single part (or leaking
        // intermediate `ar_str_concat` buffers) is a double-free / leak.
        let fat = self.module.fat_ptr_llvm_ty();
        let n = self.module.ptr_int_ty();
        self.module.require_memcpy();
        let memcpy = self.module.memcpy_intrinsic();
        if parts.is_empty() {
            let first = self.new_value();
            self.writer
                .line(format!("{first} = insertvalue {fat} undef, ptr null, 0"));
            self.writer
                .line(format!("{result} = insertvalue {fat} {first}, {n} 0, 1"));
            return Ok(());
        }
        let string_ty = ArType::Primitive(Primitive::Str);
        let mut part_vals = Vec::with_capacity(parts.len());
        for part in parts {
            part_vals.push(self.operand(part, &string_ty)?);
        }
        let mut total = self.new_value();
        self.writer.line(format!("{total} = add {n} 0, 0"));
        let mut ptrs = Vec::with_capacity(part_vals.len());
        let mut lens = Vec::with_capacity(part_vals.len());
        for part in &part_vals {
            let ptr = self.new_value();
            self.writer
                .line(format!("{ptr} = extractvalue {fat} {}, 0", part.value));
            let len = self.new_value();
            self.writer
                .line(format!("{len} = extractvalue {fat} {}, 1", part.value));
            let next_total = self.new_value();
            self.writer
                .line(format!("{next_total} = add {n} {total}, {len}"));
            total = next_total;
            ptrs.push(ptr);
            lens.push(len);
        }
        let alloc_size = self.new_value();
        self.writer
            .line(format!("{alloc_size} = add {n} {total}, 1"));
        self.module.require_raw_malloc();
        let buf = self.new_value();
        self.writer.line(format!(
            "{buf} = call ptr @ar_rt_raw_malloc({n} {alloc_size})"
        ));
        let mut offset = self.new_value();
        self.writer.line(format!("{offset} = add {n} 0, 0"));
        for (ptr, len) in ptrs.iter().zip(&lens) {
            let dest = self.new_value();
            self.writer.line(format!(
                "{dest} = getelementptr inbounds i8, ptr {buf}, {n} {offset}"
            ));
            self.writer.line(format!(
                "call void @{memcpy}(ptr {dest}, ptr {ptr}, {n} {len}, i1 false)"
            ));
            let next_offset = self.new_value();
            self.writer
                .line(format!("{next_offset} = add {n} {offset}, {len}"));
            offset = next_offset;
        }
        let nul_addr = self.new_value();
        self.writer.line(format!(
            "{nul_addr} = getelementptr inbounds i8, ptr {buf}, {n} {total}"
        ));
        self.writer
            .line(format!("store i8 0, ptr {nul_addr}, align 1"));
        let first = self.new_value();
        self.writer
            .line(format!("{first} = insertvalue {fat} undef, ptr {buf}, 0"));
        let owned = self.new_value();
        self.writer.line(format!(
            "{owned} = insertvalue {fat} {first}, {n} {total}, 1"
        ));
        self.assign_coerced(result, TypedValue::new(fat, owned), lhs_ty)
    }

    /// Compare fat pointers (`str` / slices) by length then `memcmp`.
    ///
    /// LLVM rejects `icmp` on `{ ptr, i64 }` aggregates; this mirrors the
    /// Cranelift / C backend lowering.
    pub(in crate::codegen) fn emit_fat_pointer_eq(
        &mut self,
        result: &str,
        left: &AmirOperand,
        right: &AmirOperand,
        operand_ty: &ArType,
        negate: bool,
    ) -> Result<(), Diagnostic> {
        let fat = self.module.fat_ptr_llvm_ty();
        let n = self.module.ptr_int_ty();
        let left = self.operand(left, operand_ty)?;
        let right = self.operand(right, operand_ty)?;
        let left_ptr = self.new_value();
        self.writer
            .line(format!("{left_ptr} = extractvalue {fat} {}, 0", left.value));
        let left_len = self.new_value();
        self.writer
            .line(format!("{left_len} = extractvalue {fat} {}, 1", left.value));
        let right_ptr = self.new_value();
        self.writer.line(format!(
            "{right_ptr} = extractvalue {fat} {}, 0",
            right.value
        ));
        let right_len = self.new_value();
        self.writer.line(format!(
            "{right_len} = extractvalue {fat} {}, 1",
            right.value
        ));
        let len_eq = self.new_value();
        self.writer
            .line(format!("{len_eq} = icmp eq {n} {left_len}, {right_len}"));
        let same_length = self.new_block();
        let different_lengths = self.new_block();
        let empty_strings = self.new_block();
        let compare_bytes = self.new_block();
        let join = self.new_block();
        self.writer.line(format!(
            "br i1 {len_eq}, label %{same_length}, label %{different_lengths}"
        ));

        self.enter_llvm_label(&different_lengths);
        self.writer.line(format!("br label %{join}"));

        self.enter_llvm_label(&same_length);
        let is_empty = self.new_value();
        self.writer
            .line(format!("{is_empty} = icmp eq {n} {left_len}, 0"));
        self.writer.line(format!(
            "br i1 {is_empty}, label %{empty_strings}, label %{compare_bytes}"
        ));

        self.enter_llvm_label(&empty_strings);
        self.writer.line(format!("br label %{join}"));

        self.enter_llvm_label(&compare_bytes);
        self.module.require_memcmp();
        let cmp = self.new_value();
        self.writer.line(format!(
            "{cmp} = call i32 @memcmp(ptr {left_ptr}, ptr {right_ptr}, {n} {left_len})"
        ));
        let bytes_eq = self.new_value();
        self.writer
            .line(format!("{bytes_eq} = icmp eq i32 {cmp}, 0"));
        self.writer.line(format!("br label %{join}"));

        self.enter_llvm_label(&join);
        let eq = self.new_value();
        self.writer.line(format!(
            "{eq} = phi i1 [ false, %{different_lengths} ], [ true, %{empty_strings} ], [ {bytes_eq}, %{compare_bytes} ]"
        ));
        if negate {
            self.writer.line(format!("{result} = xor i1 {eq}, true"));
        } else {
            self.writer
                .line(format!("{result} = select i1 true, i1 {eq}, i1 {eq}"));
        }
        Ok(())
    }
}
