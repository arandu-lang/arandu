use arandu_middle::amir::{AmirOperand, AmirPlace, AmirProjection};
use arandu_middle::types::{ArType, Primitive};
use arandu_middle::{Diagnostic, SymbolId};

use super::super::{FuncEmitter, ice, local_name};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(in crate::codegen) fn place(
        &mut self,
        place: &AmirPlace,
    ) -> Result<(String, ArType), Diagnostic> {
        let local = &self.func.locals[place.local.as_usize()];
        if !local.is_memory {
            // `prune_dummy_loads_stores` guarantees that non-memory locals have
            // no surviving place access. Reaching here means AMIR and this
            // backend disagree, and emitting an address would silently read or
            // write a stack slot that no longer exists.
            return Err(ice(format!(
                "AMIR local %l{} ({:?}) is not memory-backed but was used as a place",
                local.id.as_usize(),
                local.symbol
            )));
        }
        let mut address = local_name(local.id);
        let mut ty = self.module.interner.resolve(local.ty);
        for projection in &place.projections {
            match projection {
                AmirProjection::Deref => {
                    let pointee = match &ty {
                        ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner)
                        | ArType::Nullable(inner) => self.module.interner.resolve(*inner),
                        other => {
                            return Err(ice(format!(
                                "deref projection requires a pointer, got {other:?}"
                            )));
                        }
                    };
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load ptr, ptr {address}, align {}",
                        self.module.pointer_align()
                    ));
                    address = loaded;
                    ty = pointee;
                }
                AmirProjection::Field(symbol) => {
                    let (offset, field_ty) = self.field_projection(&ty, *symbol)?;
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = field_ty;
                }
                AmirProjection::TupleField(index) => {
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&ty, self.module.interner, self.module.info)
                        .map_err(|error| ice(format!("LLVM layout failed for {ty:?}: {error}")))?;
                    let offset = layout.field_offsets.get(*index).copied().ok_or_else(|| {
                        ice(format!("tuple field {index} is out of range for {ty:?}"))
                    })?;
                    let ArType::Tuple(args) = &ty else {
                        return Err(ice(format!(
                            "tuple projection requires a tuple, got {ty:?}"
                        )));
                    };
                    let field_ty = self
                        .module
                        .interner
                        .resolve(self.module.interner.type_args(*args)[*index]);
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = field_ty;
                }
                AmirProjection::Payload {
                    index,
                    field_ty,
                    tuple_ty,
                    ..
                } => {
                    // Result/Option/enum layout: field_offsets[0] = tag,
                    // field_offsets[1] = payload. `index` selects a field
                    // inside the payload tuple when `tuple_ty` is set — never
                    // the tag slot. Matches Cranelift/Wasm place lowering.
                    let owner_ty = match &ty {
                        ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                            self.module.interner.resolve(*inner)
                        }
                        other => other.clone(),
                    };
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&owner_ty, self.module.interner, self.module.info)
                        .map_err(|error| {
                            ice(format!("LLVM layout failed for {owner_ty:?}: {error}"))
                        })?;
                    let mut offset = layout.field_offsets.get(1).copied().unwrap_or(0);
                    if let Some(tuple_ty) = tuple_ty {
                        let payload_ty = self.module.interner.resolve(*tuple_ty);
                        let payload_layout = self
                            .module
                            .layout
                            .layout_of_type(&payload_ty, self.module.interner, self.module.info)
                            .map_err(|error| {
                                ice(format!("LLVM layout failed for {payload_ty:?}: {error}"))
                            })?;
                        offset = offset.saturating_add(
                            payload_layout
                                .field_offsets
                                .get(*index)
                                .copied()
                                .unwrap_or(0),
                        );
                    }
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = self.module.interner.resolve(*field_ty);
                }
                AmirProjection::IndexConstant(index) => {
                    let element = match &ty {
                        ArType::Array(_, element) => self.module.interner.resolve(*element),
                        ArType::Tuple(args) => self
                            .module
                            .interner
                            .resolve(self.module.interner.type_args(*args)[*index]),
                        other => {
                            return Err(ice(format!(
                                "index projection requires an array or tuple, got {other:?}"
                            )));
                        }
                    };
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&ty, self.module.interner, self.module.info)
                        .map_err(|error| ice(format!("LLVM layout failed for {ty:?}: {error}")))?;
                    let offset = layout
                        .field_offsets
                        .get(*index)
                        .copied()
                        .unwrap_or_else(|| {
                            self.module
                                .layout
                                .layout_of_type(&element, self.module.interner, self.module.info)
                                .map_or(0, |layout| layout.size * *index as u64)
                        });
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = element;
                }
                AmirProjection::Index(index) => {
                    let (base_ptr, element) = match &ty {
                        ArType::Array(_, element) => {
                            (address.clone(), self.module.interner.resolve(*element))
                        }
                        ArType::Slice(element) | ArType::MutSlice(element) => {
                            // `address` points at a fat `{data,len}` descriptor.
                            let element = self.module.interner.resolve(*element);
                            let data = self.new_value();
                            let align = self.module.pointer_align();
                            self.writer
                                .line(format!("{data} = load ptr, ptr {address}, align {align}"));
                            (data, element)
                        }
                        ArType::Primitive(Primitive::Str) => {
                            let data = self.new_value();
                            let align = self.module.pointer_align();
                            self.writer
                                .line(format!("{data} = load ptr, ptr {address}, align {align}"));
                            (data, ArType::Primitive(Primitive::U8))
                        }
                        other => {
                            return Err(ice(format!(
                                "dynamic index projection requires an array or slice, got {other:?}"
                            )));
                        }
                    };
                    let stride = self
                        .module
                        .layout
                        .layout_of_type(&element, self.module.interner, self.module.info)
                        .map_err(|error| {
                            ice(format!("LLVM layout failed for {element:?}: {error}"))
                        })?
                        .size;
                    let index_ty = self.module.operand_type(index, self.func);
                    let index = self.operand(index, &index_ty)?;
                    let index = self.coerce(index, &self.module.i64_ty())?;
                    let offset = self.new_value();
                    self.writer
                        .line(format!("{offset} = mul i64 {}, {stride}", index.value));
                    let next = self.new_value();
                    self.writer.line(format!(
                        "{next} = getelementptr inbounds i8, ptr {base_ptr}, i64 {offset}"
                    ));
                    address = next;
                    ty = element;
                }
                AmirProjection::Variant(_) => {}
            }
        }
        Ok((address, ty))
    }

    pub(super) fn field_projection(
        &self,
        base: &ArType,
        field: SymbolId,
    ) -> Result<(u64, ArType), Diagnostic> {
        let fields = self
            .module
            .info
            .get_struct_fields_for_type(base, self.module.interner)
            .ok_or_else(|| {
                eprintln!(
                    "LLVM field projection missing fields: base={base:?} field={field:?} name={:?}",
                    self.module.symbols.try_get(field).map(|s| s.name.as_str())
                );
                ice(format!("no struct fields for {base:?}"))
            })?;
        let field_symbol = self.module.symbols.try_get(field);
        let field_name = field_symbol.map(|symbol| symbol.name.as_str());
        let info = fields
            .iter()
            .find(|candidate| {
                candidate.symbol == Some(field)
                    || field_name.is_some_and(|name| candidate.name.as_str() == name)
            })
            .ok_or_else(|| ice(format!("field {field:?} is not present in {base:?}")))?;
        let layout = self
            .module
            .layout
            .layout_of_type(base, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {base:?}: {error}")))?;
        let offset = layout
            .field_offsets
            .get(info.index)
            .copied()
            .ok_or_else(|| {
                ice(format!(
                    "field index {} is out of range for {base:?}",
                    info.index
                ))
            })?;
        Ok((offset, self.module.interner.resolve(info.ty)))
    }

    pub(in crate::codegen) fn aggregate_address(
        &mut self,
        operand: &AmirOperand,
        ty: &ArType,
    ) -> Result<String, Diagnostic> {
        if self.module.is_pointer(ty) {
            let value = self.operand(operand, ty)?;
            return Ok(value.value);
        }
        let value = self.operand(operand, ty)?;
        let slot = self.scratch_alloca(ty)?;
        self.store_value(&slot, &value, ty)?;
        Ok(slot)
    }

    pub(super) fn field_type_by_index(
        &self,
        owner: &ArType,
        index: usize,
    ) -> Result<ArType, Diagnostic> {
        let Some(fields) = self
            .module
            .info
            .get_struct_fields_for_type(owner, self.module.interner)
        else {
            return match owner {
                ArType::Tuple(args) => self
                    .module
                    .interner
                    .type_args(*args)
                    .get(index)
                    .copied()
                    .map(|id| self.module.interner.resolve(id))
                    .ok_or_else(|| ice(format!("tuple field index {index} is outside {owner:?}"))),
                ArType::Result(ok, err) => {
                    Ok(self
                        .module
                        .interner
                        .resolve(if index == 0 { *ok } else { *err }))
                }
                ArType::Option(inner) | ArType::Poll(inner) => {
                    Ok(self.module.interner.resolve(*inner))
                }
                other => Err(ice(format!("no struct fields for {other:?}"))),
            };
        };
        let field = fields
            .iter()
            .find(|candidate| candidate.index == index)
            .ok_or_else(|| ice(format!("field index {index} is not present in {owner:?}")))?;
        arandu_middle::layout::instantiated_field_type(
            owner,
            field.name.as_str(),
            self.module.interner,
            self.module.info,
        )
        .map(|id| self.module.interner.resolve(id))
        .ok_or_else(|| ice(format!("cannot instantiate field {index} of {owner:?}")))
    }

    pub(in crate::codegen) fn aggregate_field_address(
        &mut self,
        owner: &ArType,
        base: &str,
        index: usize,
    ) -> Result<(String, ArType), Diagnostic> {
        let layout = self
            .module
            .layout
            .layout_of_type(owner, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {owner:?}: {error}")))?;
        let offset = layout
            .field_offsets
            .get(index)
            .copied()
            .ok_or_else(|| ice(format!("field index {index} is outside {owner:?}")))?;
        let field_ty = self.field_type_by_index(owner, index)?;
        Ok((self.byte_gep(base, offset), field_ty))
    }

    pub(in crate::codegen) fn enum_payload_type(
        &self,
        owner: &ArType,
        variant_tag: usize,
    ) -> Result<Option<ArType>, Diagnostic> {
        let payload = match owner {
            ArType::Named(..) => arandu_middle::layout::instantiated_enum_variant_payload_type(
                owner,
                variant_tag,
                self.module.interner,
                self.module.info,
            )
            .map(|id| self.module.interner.resolve(id)),
            ArType::Result(ok, err) => Some(if variant_tag == 0 {
                self.module.interner.resolve(*ok)
            } else {
                self.module.interner.resolve(*err)
            }),
            ArType::Option(inner) if variant_tag == 1 => Some(self.module.interner.resolve(*inner)),
            ArType::Poll(inner) if variant_tag == 0 => Some(self.module.interner.resolve(*inner)),
            _ => None,
        };
        Ok(payload)
    }
}
