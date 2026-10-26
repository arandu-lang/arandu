use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, TempId};
use arandu_middle::layout::TagEncoding;
use arandu_middle::types::{ArType, Primitive};

use super::super::{FuncEmitter, TypedValue, ice};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(in crate::codegen) fn emit_enum_construct(
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

    pub(in crate::codegen) fn emit_discriminant(
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
    pub(in crate::codegen) fn emit_enum_payload(
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
}
