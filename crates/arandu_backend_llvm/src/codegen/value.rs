use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, AmirRvalue, TempId};
use arandu_middle::layout::TagEncoding;
use arandu_middle::types::{ArType, Primitive};

use super::{FuncEmitter, TypedValue, ice};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_enum_construct(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        variant_tag: usize,
        payload: Option<&AmirOperand>,
    ) -> Result<(), Diagnostic> {
        let layout = self
            .module
            .layout
            .layout_of_type(lhs_ty, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {lhs_ty:?}: {error}")))?;
        let slot = self.scratch_alloca(lhs_ty)?;
        let aggregate = self.module.llvm_ty(lhs_ty)?;
        let align = self.module.align_of(lhs_ty);
        self.writer.line(format!(
            "store {aggregate} zeroinitializer, ptr {slot}, align {align}"
        ));
        match layout.tag_encoding {
            Some(TagEncoding::PointerTag {
                tag_mask,
                pointer_offset,
                ..
            }) => {
                let pointer_width = self.module.layout.pointer_width();
                let tag = (variant_tag as u64) & tag_mask;
                let stored = if let Some(payload) = payload {
                    let payload_ty = self.module.operand_type(payload, self.func);
                    if !self.module.is_pointer(&payload_ty) {
                        return Err(ice(format!(
                            "pointer-tagged enum payload must be a pointer, got {payload_ty:?}"
                        )));
                    }
                    let value = self.operand(payload, &payload_ty)?;
                    let raw = self.new_value();
                    let width = if pointer_width == 4 { "i32" } else { "i64" };
                    self.writer
                        .line(format!("{raw} = ptrtoint ptr {} to {width}", value.value));
                    let clean = self.new_value();
                    self.writer.line(format!(
                        "{clean} = and {width} {raw}, {}",
                        !(tag_mask as i128)
                    ));
                    let tagged = self.new_value();
                    self.writer
                        .line(format!("{tagged} = or {width} {clean}, {tag}"));
                    tagged
                } else {
                    let width = if pointer_width == 4 { "i32" } else { "i64" };
                    let value = self.new_value();
                    self.writer.line(format!("{value} = add {width} 0, {tag}"));
                    value
                };
                let address = self.byte_gep(&slot, pointer_offset);
                let (bits, align) = if pointer_width == 4 { (32, 4) } else { (8, 8) };
                self.writer.line(format!(
                    "store i{bits} {stored}, ptr {address}, align {align}"
                ));
            }
            Some(TagEncoding::Niche {
                niche_offset,
                niche_size,
                niche_value,
                tagged_variant,
                ..
            }) => {
                if variant_tag == tagged_variant {
                    let width = if niche_size == 0 {
                        self.module.layout.pointer_width() / 8
                    } else {
                        niche_size
                    };
                    self.store_enum_integer(&slot, niche_offset, width, niche_value)?;
                } else if let Some(payload) = payload {
                    let payload_ty = self
                        .enum_payload_type(lhs_ty, variant_tag)?
                        .unwrap_or_else(|| self.module.operand_type(payload, self.func));
                    if !matches!(payload_ty, ArType::Void | ArType::Error) {
                        let value = self.operand(payload, &payload_ty)?;
                        self.store_value(&slot, &value, &payload_ty)?;
                    }
                }
            }
            Some(TagEncoding::Direct {
                tag_size,
                payload_offset,
            }) => {
                self.store_enum_integer(&slot, 0, tag_size, variant_tag as u64)?;
                if let Some(payload) = payload {
                    let payload_ty = self
                        .enum_payload_type(lhs_ty, variant_tag)?
                        .unwrap_or_else(|| self.module.operand_type(payload, self.func));
                    if !matches!(payload_ty, ArType::Void | ArType::Error) {
                        let value = self.operand(payload, &payload_ty)?;
                        let address = self.byte_gep(&slot, payload_offset);
                        self.store_value(&address, &value, &payload_ty)?;
                    }
                }
            }
            None => {
                return Err(ice(format!(
                    "enum construction requires tag encoding metadata, got none for {lhs_ty:?}"
                )));
            }
        }
        self.writer.line(format!(
            "{result} = load {aggregate}, ptr {slot}, align {align}"
        ));
        Ok(())
    }

    pub(super) fn enum_storage(
        &mut self,
        value: &AmirOperand,
    ) -> Result<(String, ArType), Diagnostic> {
        let base_ty = self.module.operand_type(value, self.func);
        match &base_ty {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                let owner = self.module.interner.resolve(*inner);
                let pointer = self.operand(value, &base_ty)?;
                Ok((pointer.value, owner))
            }
            _ => {
                let address = self.aggregate_address(value, &base_ty)?;
                Ok((address, base_ty))
            }
        }
    }

    pub(super) fn emit_discriminant(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (address, owner) = self.enum_storage(value)?;
        let layout = self
            .module
            .layout
            .layout_of_type(&owner, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {owner:?}: {error}")))?;
        let raw = match layout.tag_encoding {
            Some(TagEncoding::PointerTag {
                tag_mask,
                pointer_offset,
                ..
            }) => {
                let width = self.module.layout.pointer_width();
                let loaded = self.new_value();
                let tag_address = self.byte_gep(&address, pointer_offset);
                self.writer.line(format!(
                    "{loaded} = load i{width}, ptr {tag_address}, align {}",
                    width / 8
                ));
                let masked = self.new_value();
                self.writer
                    .line(format!("{masked} = and i{width} {loaded}, {}", tag_mask));
                TypedValue::new(format!("i{width}"), masked)
            }
            Some(TagEncoding::Niche {
                niche_offset,
                niche_size,
                niche_value,
                untagged_variant,
                tagged_variant,
            }) => {
                let width = if niche_size == 0 {
                    self.module.layout.pointer_width()
                } else {
                    niche_size * 8
                };
                let loaded = self.new_value();
                let niche_address = self.byte_gep(&address, niche_offset);
                self.writer.line(format!(
                    "{loaded} = load i{width}, ptr {niche_address}, align {}",
                    (width / 8).max(1)
                ));
                let is_niche = self.new_value();
                self.writer.line(format!(
                    "{is_niche} = icmp eq i{width} {loaded}, {niche_value}"
                ));
                let selected = self.new_value();
                self.writer.line(format!(
                    "{selected} = select i1 {is_niche}, i{width} {tagged_variant}, i{width} {untagged_variant}"
                ));
                TypedValue::new(format!("i{width}"), selected)
            }
            Some(TagEncoding::Direct { tag_size, .. }) => {
                let width = tag_size * 8;
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load i{width}, ptr {address}, align {}",
                    tag_size.max(1)
                ));
                TypedValue::new(format!("i{width}"), loaded)
            }
            None => {
                return Err(ice(format!(
                    "discriminant requires tag encoding metadata, got none for {owner:?}"
                )));
            }
        };
        self.assign_coerced(result, raw, lhs_ty)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_enum_payload(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
        field_ty: arandu_middle::types::TypeId,
        tuple_ty: Option<arandu_middle::types::TypeId>,
        index: usize,
        lhs: Option<TempId>,
    ) -> Result<(), Diagnostic> {
        let (address, owner) = self.enum_storage(value)?;
        let layout = self
            .module
            .layout
            .layout_of_type(&owner, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {owner:?}: {error}")))?;
        let payload_ty = self.module.interner.resolve(field_ty);
        if let Some(TagEncoding::PointerTag {
            tag_mask,
            pointer_offset,
            ..
        }) = layout.tag_encoding
        {
            let width = self.module.layout.pointer_width();
            let loaded = self.new_value();
            let tag_address = self.byte_gep(&address, pointer_offset);
            self.writer.line(format!(
                "{loaded} = load i{width}, ptr {tag_address}, align {}",
                width / 8
            ));
            let clean = self.new_value();
            self.writer.line(format!(
                "{clean} = and i{width} {loaded}, {}",
                !(tag_mask as i128) as u64
            ));
            return self.assign_coerced(
                result,
                TypedValue::new(format!("i{width}"), clean),
                &payload_ty,
            );
        }
        let tuple_offset = if let Some(tuple_ty) = tuple_ty {
            let payload_ty = self.module.interner.resolve(tuple_ty);
            let payload_layout = self
                .module
                .layout
                .layout_of_type(&payload_ty, self.module.interner, self.module.info)
                .map_err(|error| ice(format!("LLVM layout failed for {payload_ty:?}: {error}")))?;
            payload_layout
                .field_offsets
                .get(index)
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        let base_offset = layout.field_offsets.get(1).copied().unwrap_or(0);
        let offset = base_offset + tuple_offset;
        let payload_address = self.byte_gep(&address, offset);
        // Scalars / pointers load by value. Everything else uses Cranelift's
        // pointer-repr into the enum payload so a later coerce can copy the
        // full destination layout (e.g. thin AMIR projection → BumpArena).
        if self.module.is_pointer(lhs_ty) {
            let align = self.module.pointer_align();
            let loaded = self.new_value();
            self.writer.line(format!(
                "{loaded} = load ptr, ptr {payload_address}, align {align}"
            ));
            return self.assign_coerced(result, TypedValue::new("ptr".to_owned(), loaded), lhs_ty);
        }
        if self.module.integer_width(lhs_ty).is_some()
            || self.module.is_float(lhs_ty)
            || matches!(lhs_ty, ArType::Primitive(Primitive::Bool))
        {
            let payload_text = self.module.llvm_ty(lhs_ty)?;
            let align = self.module.align_of(lhs_ty);
            let loaded = self.new_value();
            self.writer.line(format!(
                "{loaded} = load {payload_text}, ptr {payload_address}, align {align}"
            ));
            return self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty);
        }
        self.writer.line("; enum_payload_ptr_repr");
        self.writer.line(format!(
            "{result} = getelementptr i8, ptr {payload_address}, i64 0"
        ));
        if let Some(temp) = lhs {
            self.temp_llvm_ty[temp.as_usize()] = Some("ptr".to_owned());
        }
        Ok(())
    }

    pub(super) fn emit_to_str(
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

    pub(super) fn emit_string_interp(
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
    pub(super) fn emit_fat_pointer_eq(
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

    pub(super) fn emit_coroutine_ready(
        &mut self,
        result: &str,
        value: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
    ) -> Result<(), Diagnostic> {
        let payload_ar = self.module.interner.resolve(payload_ty);
        let payload_layout = self
            .module
            .layout
            .layout_of_type(&payload_ar, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {payload_ar:?}: {error}")))?;
        // Header is 8 bytes; host poll/block_on helpers read an i64 payload, so
        // keep at least 16 bytes (matches Cranelift).
        let size = (8 + payload_layout.size.max(1)).max(16);
        let pointer = self.new_value();
        self.writer.line(format!(
            "{pointer} = call ptr @ar_rt_raw_malloc({} {size})",
            self.module.ptr_int_ty()
        ));
        let null = self.new_value();
        self.writer
            .line(format!("{null} = icmp eq ptr {pointer}, null"));
        let ok = self.new_value();
        self.writer.line(format!("{ok} = xor i1 {null}, true"));
        let trap = self.new_block();
        let continue_block = self.new_block();
        self.writer.line(format!(
            "br i1 {ok}, label %{continue_block}, label %{trap}"
        ));
        self.enter_llvm_label(&trap);
        self.writer.line("call void @llvm.trap()");
        self.writer.line("unreachable");
        self.enter_llvm_label(&continue_block);
        self.writer
            .line(format!("store i32 0, ptr {pointer}, align 4"));
        let magic = self.byte_gep(&pointer, 4);
        // CO_MAGIC = 0x4152434F ("ARCO"); must match `poll_runtime::CO_MAGIC`.
        self.writer.line(format!(
            "store i32 {}, ptr {magic}, align 4",
            0x4152_434fu32
        ));
        if !matches!(payload_ar, ArType::Void) {
            let payload = self.operand(value, &payload_ar)?;
            let payload_address = self.byte_gep(&pointer, 8);
            // Widen integer payloads to i64 so `ar_co_block_on_i64` can read them.
            let stored = if self.module.integer_width(&payload_ar).is_some() {
                self.coerce(payload, &ArType::Primitive(Primitive::I64))?
            } else {
                payload
            };
            let stored_ty = if self.module.integer_width(&payload_ar).is_some() {
                ArType::Primitive(Primitive::I64)
            } else {
                payload_ar.clone()
            };
            self.store_value(&payload_address, &stored, &stored_ty)?;
        }
        self.assign_coerced(
            result,
            TypedValue::new("ptr".to_owned(), pointer),
            &ArType::Ptr(self.module.interner.intern(ArType::Void)),
        )
    }

    pub(super) fn emit_gen_operation(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
    ) -> Result<(), Diagnostic> {
        match rvalue {
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let value = self.operand(value, &payload_ar)?;
                self.store_value(&slot, &value, &payload_ar)?;
                let inserted = self.new_value();
                self.writer.line(format!(
                    "{inserted} = call i64 @ar_gen_insert_raw(ptr {slot}, i64 {}, i64 {}, ptr null)",
                    layout.size, layout.align
                ));
                self.assign_coerced(result, TypedValue::new("i64".to_owned(), inserted), lhs_ty)
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            }
            | AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let handle = self.operand(gen_ref, &ArType::Primitive(Primitive::I64))?;
                let name = if matches!(rvalue, AmirRvalue::GenGet { .. }) {
                    "ar_gen_get_raw"
                } else {
                    "ar_gen_remove_raw"
                };
                let ok = self.new_value();
                self.writer.line(format!(
                    "{ok} = call i1 @{name}(i64 {}, ptr {slot}, i64 {}, i64 {})",
                    handle.value, layout.size, layout.align
                ));
                let fail = self.new_block();
                let cont = self.new_block();
                self.writer
                    .line(format!("br i1 {ok}, label %{cont}, label %{fail}"));
                self.enter_llvm_label(&fail);
                self.writer.line("call void @llvm.trap()");
                self.writer.line("unreachable");
                self.enter_llvm_label(&cont);
                let payload_text = self.module.llvm_ty(&payload_ar)?;
                let align = self.module.align_of(&payload_ar);
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {payload_text}, ptr {slot}, align {align}"
                ));
                self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty)
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            }
            | AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                let upsert = matches!(rvalue, AmirRvalue::GenUpsert { .. });
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let value = self.operand(value, &payload_ar)?;
                self.store_value(&slot, &value, &payload_ar)?;
                let handle = self.operand(gen_ref, &ArType::Primitive(Primitive::I64))?;
                if upsert {
                    let updated = self.new_value();
                    self.writer.line(format!(
                        "{updated} = call i64 @ar_gen_upsert_raw(i64 {}, ptr {slot}, i64 {}, i64 {}, ptr null)",
                        handle.value, layout.size, layout.align
                    ));
                    self.assign_coerced(result, TypedValue::new("i64".to_owned(), updated), lhs_ty)
                } else {
                    let ok = self.new_value();
                    self.writer.line(format!(
                        "{ok} = call i1 @ar_gen_set_raw(i64 {}, ptr {slot}, i64 {}, i64 {}, ptr null)",
                        handle.value, layout.size, layout.align
                    ));
                    let fail = self.new_block();
                    let cont = self.new_block();
                    self.writer
                        .line(format!("br i1 {ok}, label %{cont}, label %{fail}"));
                    self.enter_llvm_label(&fail);
                    self.writer.line("call void @llvm.trap()");
                    self.writer.line("unreachable");
                    self.enter_llvm_label(&cont);
                    self.assign_coerced(result, handle, lhs_ty)
                }
            }
            _ => Err(ice("unexpected GenRef operation")),
        }
    }

    pub(super) fn construct_aggregate(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        fields: &[(u64, ArType, &AmirOperand)],
    ) -> Result<(), Diagnostic> {
        let slot = self.scratch_alloca(lhs_ty)?;
        let aggregate = self.module.llvm_ty(lhs_ty)?;
        let align = self.module.align_of(lhs_ty);
        self.writer.line(format!(
            "store {aggregate} zeroinitializer, ptr {slot}, align {align}"
        ));
        for (offset, field_ty, operand) in fields {
            let address = self.byte_gep(&slot, *offset);
            let value = self.operand(operand, field_ty)?;
            self.store_value(&address, &value, field_ty)?;
        }
        self.writer.line(format!(
            "{result} = load {aggregate}, ptr {slot}, align {align}"
        ));
        Ok(())
    }

    pub(super) fn new_value(&mut self) -> String {
        let value = format!("%v{}", self.next_value);
        self.next_value += 1;
        value
    }

    /// Fresh basic-block label without a leading `%`.
    ///
    /// Callers must write `label %{name}` / `{name}:`. Using [`Self::new_value`]
    /// here would emit `%%vN` because that helper already includes `%`.
    pub(super) fn new_block(&mut self) -> String {
        let name = format!("loc{}", self.next_value);
        self.next_value += 1;
        name
    }
}
