use super::super::{FuncEmitter, ice};
use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirRvalue, TempId};
use arandu_middle::types::{ArType, Primitive};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_aggregate_rvalue(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
        lhs: Option<TempId>,
    ) -> Result<(), Diagnostic> {
        let fat = self.module.fat_ptr_llvm_ty();
        match rvalue {
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let struct_ty = match lhs_ty {
                    ArType::Named(id, _) if *id == *struct_symbol => lhs_ty.clone(),
                    _ => ArType::named(*struct_symbol, &[], self.module.interner),
                };
                let field_defs = self
                    .module
                    .info
                    .get_struct_fields_for_type(&struct_ty, self.module.interner)
                    .ok_or_else(|| {
                        eprintln!(
                            "LLVM StructLiteral missing fields: lhs={lhs_ty:?} struct={struct_ty:?} symbol={struct_symbol:?} fields={fields:?}"
                        );
                        ice(format!("no struct fields for {struct_ty:?}"))
                    })?;
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&struct_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {struct_ty:?}: {error}"))
                    })?;
                let mut resolved = Vec::with_capacity(fields.len());
                for (name, operand) in fields {
                    let field = field_defs
                        .get(name.as_str())
                        .ok_or_else(|| ice(format!("unknown struct field {name:?}")))?;
                    let offset = layout
                        .field_offsets
                        .get(field.index)
                        .copied()
                        .ok_or_else(|| ice(format!("field {name:?} is outside {struct_ty:?}")))?;
                    let field_ty = arandu_middle::layout::instantiated_field_type(
                        &struct_ty,
                        name.as_str(),
                        self.module.interner,
                        self.module.info,
                    )
                    .map(|id| self.module.interner.resolve(id))
                    .ok_or_else(|| ice(format!("cannot instantiate field {name:?}")))?;
                    resolved.push((offset, field_ty, operand));
                }
                self.construct_aggregate(result, &struct_ty, &resolved)
            }
            AmirRvalue::Tuple { items } => {
                let ArType::Tuple(types) = lhs_ty else {
                    return Err(ice(format!(
                        "tuple literal requires a tuple result type, got {lhs_ty:?}"
                    )));
                };
                let types = self.module.interner.type_args(*types);
                if types.len() != items.len() {
                    return Err(ice(format!(
                        "tuple literal has {} items for {} types",
                        items.len(),
                        types.len()
                    )));
                }
                let layout = self
                    .module
                    .layout
                    .layout_of_type(lhs_ty, self.module.interner, self.module.info)
                    .map_err(|error| ice(format!("LLVM layout failed for {lhs_ty:?}: {error}")))?;
                let mut resolved = Vec::with_capacity(items.len());
                for (index, operand) in items.iter().enumerate() {
                    let offset = layout.field_offsets.get(index).copied().unwrap_or(0);
                    let field_ty = self.module.interner.resolve(types[index]);
                    resolved.push((offset, field_ty, operand));
                }
                self.construct_aggregate(result, lhs_ty, &resolved)
            }
            AmirRvalue::Array { items } => {
                let ArType::Array(length, element) = lhs_ty else {
                    return Err(ice(format!(
                        "array literal requires an array result type, got {lhs_ty:?}"
                    )));
                };
                if items.len() > *length as usize {
                    return Err(ice(format!(
                        "array literal has {} items for length {length}",
                        items.len()
                    )));
                }
                let element_ty = self.module.interner.resolve(*element);
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let mut resolved = Vec::with_capacity(items.len());
                for (index, operand) in items.iter().enumerate() {
                    resolved.push((index as u64 * stride, element_ty.clone(), operand));
                }
                self.construct_aggregate(result, lhs_ty, &resolved)
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => self.emit_enum_construct(result, lhs_ty, *variant_tag, payload.as_ref()),
            AmirRvalue::EnumPayload {
                value,
                field_ty,
                tuple_ty,
                index,
                ..
            } => self.emit_enum_payload(result, lhs_ty, value, *field_ty, *tuple_ty, *index, lhs),
            AmirRvalue::FieldAccess { base, field } => {
                let base_ty = self.module.operand_type(base, self.func);
                let (owner, address) = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        let owner = self.module.interner.resolve(*inner);
                        let pointer = self.operand(base, &base_ty)?;
                        (owner, pointer.value)
                    }
                    _ => (base_ty.clone(), self.aggregate_address(base, &base_ty)?),
                };
                let (address, field_ty) = self.aggregate_field_address(&owner, &address, *field)?;
                // Result/Option/enum payload fields often lower as `.1` FieldAccess
                // (not EnumPayload). Non-scalar payloads use pointer-repr so a
                // later coerce can copy the full destination layout.
                let owner_is_enum = matches!(
                    &owner,
                    ArType::Option(_) | ArType::Result(_, _) | ArType::Poll(_) | ArType::Named(..)
                ) && self
                    .module
                    .layout
                    .layout_of_type(&owner, self.module.interner, self.module.info)
                    .ok()
                    .and_then(|l| l.tag_encoding)
                    .is_some();
                if owner_is_enum
                    && !self.module.is_pointer(&field_ty)
                    && !self.module.is_pointer(lhs_ty)
                    && self.module.integer_width(&field_ty).is_none()
                    && self.module.integer_width(lhs_ty).is_none()
                    && !self.module.is_float(&field_ty)
                    && !self.module.is_float(lhs_ty)
                    && !matches!(field_ty, ArType::Primitive(Primitive::Bool))
                    && !matches!(lhs_ty, ArType::Primitive(Primitive::Bool))
                {
                    self.writer.line("; enum_field_ptr_repr");
                    self.writer
                        .line(format!("{result} = getelementptr i8, ptr {address}, i64 0"));
                    if let Some(temp) = lhs {
                        self.temp_llvm_ty[temp.as_usize()] = Some("ptr".to_owned());
                    }
                    return Ok(());
                }
                let field_text = self.module.llvm_ty(&field_ty)?;
                let align = self.module.align_of(&field_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {field_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(field_text, raw, &field_ty),
                    lhs_ty,
                )
            }
            AmirRvalue::IndexAccess { base, index } => {
                let base_ty = self.module.operand_type(base, self.func);
                let deref_ty = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        self.module.interner.resolve(*inner)
                    }
                    other => other.clone(),
                };
                if let Some((_, _, _)) = self.module.vector_info(&deref_ty) {
                    let vector = self.operand(base, &base_ty)?;
                    let index_ty = ArType::Primitive(Primitive::I32);
                    let index = self.operand(index, &index_ty)?;
                    self.writer.line(format!(
                        "{result} = extractelement {} {}, i32 {}",
                        vector.ty, vector.value, index.value
                    ));
                    return Ok(());
                }
                let element_id = arandu_middle::types::index_elem_type(
                    &deref_ty,
                    self.module.symbols,
                    self.module.interner,
                )
                .ok_or_else(|| {
                    ice(format!(
                        "index access requires an indexable type, got {deref_ty:?}"
                    ))
                })?;
                let element_ty = self.module.interner.resolve(element_id);
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let index_ty = self.module.usize_ty();
                let index = self.operand(index, &index_ty)?;
                let index = self.coerce(index, &self.module.usize_ty())?;
                let index_ll = index.ty.clone();
                let base_address = if matches!(
                    &base_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                ) {
                    let pointer = self.operand(base, &base_ty)?;
                    pointer.value
                } else if matches!(&deref_ty, ArType::Array(..)) {
                    self.aggregate_address(base, &deref_ty)?
                } else if matches!(&deref_ty, ArType::Slice(_) | ArType::MutSlice(_)) {
                    let descriptor = self.operand(base, &deref_ty)?;
                    let pointer = self.new_value();
                    self.writer.line(format!(
                        "{pointer} = extractvalue {fat} {}, 0",
                        descriptor.value
                    ));
                    pointer
                } else {
                    return Err(ice(format!(
                        "LLVM backend does not implement index access for {deref_ty:?}"
                    )));
                };
                let offset = self.new_value();
                self.writer.line(format!(
                    "{offset} = mul {index_ll} {}, {stride}",
                    index.value
                ));
                let address = self.new_value();
                self.writer.line(format!(
                    "{address} = getelementptr inbounds i8, ptr {base_address}, {index_ll} {offset}"
                ));
                let element_text = self.module.llvm_ty(&element_ty)?;
                let align = self.module.align_of(&element_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {element_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(element_text, raw, &element_ty),
                    lhs_ty,
                )
            }
            _ => unreachable!(),
        }
    }
}
