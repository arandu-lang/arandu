use arandu_middle::Diagnostic;
use arandu_middle::types::ArType;

use super::super::{
    FuncEmitter, IntegerWidthFromLlvm, TypedValue, ice, parse_byte_aggregate_size, value_ar_type,
};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(in crate::codegen) fn coerce(
        &mut self,
        value: TypedValue,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        let expected_text = self.module.llvm_ty(expected)?;
        if value.ty == expected_text {
            return Ok(value);
        }
        let actual_width = self.module.integer_width_from_llvm(&value.ty);
        let expected_width = self.module.integer_width(expected);
        let actual_float = value.ty == "float" || value.ty == "double";
        let expected_float = self.module.is_float(expected);

        let instruction = if value.ty == "ptr" && expected_text == "ptr" {
            None
        } else if value.ty == "ptr" {
            if expected_width.is_some() {
                Some(format!("ptrtoint ptr {} to {expected_text}", value.value))
            } else if expected_text == self.module.fat_ptr_llvm_ty()
                || self.module.is_fat_pointer(expected)
            {
                // Load a fat `{data,len}` descriptor from behind the pointer
                // (e.g. `ref String` / `ref str` → by-value `str` / `[]T`).
                let loaded = self.new_value();
                let align = self.module.pointer_align();
                let fat = self.module.fat_ptr_llvm_ty();
                self.writer.line(format!(
                    "{loaded} = load {fat}, ptr {}, align {align}",
                    value.value
                ));
                return Ok(TypedValue::new(fat, loaded));
            } else if let Some(bytes) = parse_byte_aggregate_size(&expected_text) {
                // Pointer-repr aggregate (Cranelift-style EnumPayload of a
                // Named struct): materialize by copying `bytes` from the
                // pointee into a first-class opaque value.
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {expected_text}, align 1"));
                self.module.require_memcpy();
                let memcpy = self.module.memcpy_intrinsic();
                let n = self.module.ptr_int_ty();
                self.writer.line(format!(
                    "call void @{memcpy}(ptr {slot}, ptr {}, {n} {bytes}, i1 false)",
                    value.value
                ));
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {expected_text}, ptr {slot}, align 1"
                ));
                return Ok(TypedValue::new(expected_text, loaded));
            } else {
                return Err(ice(format!("cannot cast LLVM pointer to {expected_text}")));
            }
        } else if expected_text == "ptr" {
            if actual_width.is_some() {
                Some(format!("inttoptr {} {} to ptr", value.ty, value.value))
            } else if let Some(bytes) = parse_byte_aggregate_size(&value.ty) {
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {}, align 1", value.ty));
                self.writer.line(format!(
                    "store {} {}, ptr {slot}, align 1",
                    value.ty, value.value
                ));
                // Pointer-width opaque payloads (e.g. `Result.Ok(ptr)`) store a
                // pointer *value* as `[W x i8]`. Reinterpret those bytes as
                // `ptr`. Larger/smaller aggregates decay to the alloca address.
                if bytes == self.module.layout.pointer_width() {
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load ptr, ptr {slot}, align {}",
                        self.module.pointer_align()
                    ));
                    return Ok(TypedValue::new("ptr".to_owned(), loaded));
                }
                return Ok(TypedValue::new("ptr".to_owned(), slot));
            } else if value.ty.starts_with('[') {
                // Non-byte array → pointer: spill and decay to the alloca.
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {}, align 1", value.ty));
                self.writer.line(format!(
                    "store {} {}, ptr {slot}, align 1",
                    value.ty, value.value
                ));
                return Ok(TypedValue::new("ptr".to_owned(), slot));
            } else {
                return Err(ice(format!("cannot cast {} to LLVM pointer", value.ty)));
            }
        } else if let (Some(from_bytes), Some(to_bytes)) = (
            parse_byte_aggregate_size(&value.ty),
            parse_byte_aggregate_size(&expected_text),
        ) {
            // Opaque layout aggregates (`[N x i8]`) of different sizes are
            // rewritten through memory: zero the destination, copy the overlap.
            let src = self.new_value();
            self.writer
                .line(format!("{src} = alloca {}, align 1", value.ty));
            self.writer.line(format!(
                "store {} {}, ptr {src}, align 1",
                value.ty, value.value
            ));
            let dst = self.new_value();
            self.writer
                .line(format!("{dst} = alloca {expected_text}, align 1"));
            self.writer.line(format!(
                "store {expected_text} zeroinitializer, ptr {dst}, align 1"
            ));
            let copy = from_bytes.min(to_bytes);
            if copy > 0 {
                self.module.require_memcpy();
                let memcpy = self.module.memcpy_intrinsic();
                let n = self.module.ptr_int_ty();
                self.writer.line(format!(
                    "call void @{memcpy}(ptr {dst}, ptr {src}, {n} {copy}, i1 false)"
                ));
            }
            let loaded = self.new_value();
            self.writer.line(format!(
                "{loaded} = load {expected_text}, ptr {dst}, align 1"
            ));
            return Ok(TypedValue::new(expected_text, loaded));
        } else if actual_float && expected_float {
            if value.ty == expected_text {
                None
            } else if value.ty == "float" {
                Some(format!("fpext float {} to double", value.value))
            } else {
                Some(format!("fptrunc double {} to float", value.value))
            }
        } else if actual_float && expected_width.is_some() {
            let op = if self.module.is_signed(expected) {
                "fptosi"
            } else {
                "fptoui"
            };
            Some(format!(
                "{op} {} {} to {expected_text}",
                value.ty, value.value
            ))
        } else if expected_float {
            let Some(width) = actual_width else {
                return Err(ice(format!("cannot cast {} to {expected_text}", value.ty)));
            };
            let source_signed = value
                .source_signed
                .unwrap_or_else(|| self.module.is_signed(&value_ar_type(&value.ty)));
            let op = if width == 1 || !source_signed {
                "uitofp"
            } else {
                "sitofp"
            };
            Some(format!(
                "{op} {} {} to {expected_text}",
                value.ty, value.value
            ))
        } else if let (Some(from), Some(to)) = (actual_width, expected_width) {
            if from == to {
                None
            } else if from == 1 {
                Some(format!(
                    "zext {} {} to {expected_text}",
                    value.ty, value.value
                ))
            } else if from < to {
                // Use AMIR source signedness when known. Never infer from the
                // LLVM type string alone (`i8` is shared by `i8` and `u8`).
                let source_signed = value
                    .source_signed
                    .unwrap_or_else(|| self.module.is_signed(&value_ar_type(&value.ty)));
                let op = if source_signed { "sext" } else { "zext" };
                Some(format!(
                    "{op} {} {} to {expected_text}",
                    value.ty, value.value
                ))
            } else {
                Some(format!(
                    "trunc {} {} to {expected_text}",
                    value.ty, value.value
                ))
            }
        } else {
            return Err(ice(format!(
                "cannot cast LLVM value of type {} to {expected_text}",
                value.ty
            )));
        };
        let Some(instruction) = instruction else {
            let mut coerced = TypedValue::new(expected_text, value.value);
            if self.module.integer_width(expected).is_some() {
                coerced = coerced.with_source_signed(self.module.is_signed(expected));
            }
            return Ok(coerced);
        };
        let result = self.new_value();
        self.writer.line(format!("{result} = {instruction}"));
        Ok(TypedValue::new(expected_text, result))
    }

    pub(in crate::codegen) fn assign_coerced(
        &mut self,
        result: &str,
        value: TypedValue,
        lhs_ty: &ArType,
    ) -> Result<(), Diagnostic> {
        let value = self.coerce(value, lhs_ty)?;
        if let Some(lhs) = self.pending_assignment.take()
            && self.temp_slots[lhs.as_usize()].is_none()
        {
            self.bind_temp(lhs, value)?;
            return Ok(());
        }
        let rendered = value.render();
        self.writer
            .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
        Ok(())
    }

    /// Build a typed LLVM value, recording AMIR integer signedness for casts.
    pub(in crate::codegen) fn typed_from_ar(
        &self,
        llvm_ty: String,
        value: String,
        ar: &ArType,
    ) -> TypedValue {
        let mut typed = TypedValue::new(llvm_ty, value);
        if self.module.integer_width(ar).is_some() {
            typed = typed.with_source_signed(self.module.is_signed(ar));
        }
        typed
    }

    pub(in crate::codegen) fn scratch_alloca(&mut self, ty: &ArType) -> Result<String, Diagnostic> {
        let (text, align) = self.module.alloca_type(ty)?;
        let slot = self.new_value();
        self.writer
            .line(format!("{slot} = alloca {text}, align {align}"));
        Ok(slot)
    }

    pub(in crate::codegen) fn byte_gep(&mut self, base: &str, offset: u64) -> String {
        if offset == 0 {
            return base.to_owned();
        }
        let next = self.new_value();
        self.writer.line(format!(
            "{next} = getelementptr inbounds i8, ptr {base}, i64 {offset}"
        ));
        next
    }

    pub(in crate::codegen) fn store_value(
        &mut self,
        address: &str,
        value: &TypedValue,
        ty: &ArType,
    ) -> Result<(), Diagnostic> {
        let align = self.module.align_of(ty);
        self.writer.line(format!(
            "store {}, ptr {address}, align {align}",
            value.render()
        ));
        Ok(())
    }

    pub(in crate::codegen) fn store_enum_integer(
        &mut self,
        base: &str,
        offset: u64,
        width: u64,
        value: u64,
    ) -> Result<(), Diagnostic> {
        let (ty, align) = match width {
            1 => ("i8", 1),
            2 => ("i16", 2),
            4 => ("i32", 4),
            8 => ("i64", 8),
            other => {
                return Err(ice(format!(
                    "LLVM enum tag width {other} is not a power-of-two byte width"
                )));
            }
        };
        let address = self.byte_gep(base, offset);
        self.writer
            .line(format!("store {ty} {value}, ptr {address}, align {align}"));
        Ok(())
    }
}
