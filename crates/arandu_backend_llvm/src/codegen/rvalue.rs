use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, AmirRvalue, TempId, VectorCompareOp, VectorOp};
use arandu_middle::layout::StructLayoutProvider;
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{ArType, Primitive, VectorType};

use super::{FuncEmitter, TypedValue, ice, local_name, parse_byte_aggregate_size};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_rvalue(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
        lhs: Option<TempId>,
    ) -> Result<(), Diagnostic> {
        let fat = self.module.fat_ptr_llvm_ty();
        match rvalue {
            AmirRvalue::Use(operand) => {
                let value = self.operand(operand, lhs_ty)?;
                let rendered = value.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
            AmirRvalue::Binary { op, left, right } => {
                self.emit_binary(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::Unary { op, operand } => self.emit_unary(result, lhs_ty, *op, operand),
            AmirRvalue::Load(place) => {
                let (address, place_ty) = self.place(place)?;
                let loaded_ty = self.module.llvm_ty(&place_ty)?;
                let align = self.module.align_of(&place_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {loaded_ty}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(loaded_ty, raw, &place_ty),
                    lhs_ty,
                )
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                let (address, _) = self.place(place)?;
                self.writer.line(format!(
                    "{} = getelementptr i8, ptr {address}, i64 0",
                    result
                ));
                Ok(())
            }
            AmirRvalue::RelativeBorrow { local, .. } => {
                self.writer.line(format!(
                    "{} = getelementptr i8, ptr {}, i64 0",
                    result,
                    local_name(*local)
                ));
                Ok(())
            }
            AmirRvalue::Alloc(size) => {
                let size_ty = self.module.operand_type(size, self.func);
                let size = self.operand(size, &size_ty)?;
                let size = self.coerce(size, &self.module.usize_ty())?;
                self.writer.line(format!(
                    "{} = call ptr @ar_rt_raw_malloc({})",
                    result,
                    size.render()
                ));
                Ok(())
            }
            AmirRvalue::Len(value) => {
                let ty = self.module.operand_type(value, self.func);
                if let ArType::Array(length, _) = &ty {
                    let value = length.to_string();
                    return self.assign_coerced(
                        result,
                        TypedValue::new(self.module.llvm_ty(lhs_ty)?, value),
                        lhs_ty,
                    );
                }
                let value = self.operand(value, &ty)?;
                let aggregate = self.module.llvm_ty(&ty)?;
                if aggregate == fat {
                    self.writer.line(format!(
                        "{} = extractvalue {aggregate} {}, 1",
                        result, value.value
                    ));
                    Ok(())
                } else if value.ty == "ptr" {
                    // `ref String` / `ref str` / `ref []T`: length sits at
                    // pointer-width offset in the shared fat-descriptor layout.
                    let len_addr = self.byte_gep(&value.value, self.module.layout.pointer_width());
                    let loaded = self.new_value();
                    let align = self.module.pointer_align();
                    let len_ty = self.module.ptr_int_ty();
                    self.writer.line(format!(
                        "{loaded} = load {len_ty}, ptr {len_addr}, align {align}"
                    ));
                    self.assign_coerced(result, TypedValue::new(len_ty.to_owned(), loaded), lhs_ty)
                } else {
                    Err(ice(format!(
                        "LLVM lowering does not implement len for {ty:?}"
                    )))
                }
            }
            AmirRvalue::StrView { owner } => {
                // Intrinsic returns `ref str`: a thin pointer to a `{data,len}`
                // descriptor. Owned String/Path layouts share that prefix, so a
                // `ref Owner` is already a valid `ref str` (Cranelift pass-through).
                let ty = self.module.operand_type(owner, self.func);
                let value = self.operand(owner, &ty)?;
                if value.ty == "ptr" || self.module.is_pointer(&ty) {
                    return self.assign_coerced(
                        result,
                        TypedValue::new("ptr".to_owned(), value.value),
                        lhs_ty,
                    );
                }
                if value.ty == fat || parse_byte_aggregate_size(&value.ty).is_some() {
                    // By-value descriptor / opaque String: spill and return its address.
                    let slot = self.new_value();
                    let align = self.module.pointer_align();
                    self.writer
                        .line(format!("{slot} = alloca {}, align {align}", value.ty));
                    self.writer.line(format!(
                        "store {} {}, ptr {slot}, align {align}",
                        value.ty, value.value
                    ));
                    return self.assign_coerced(
                        result,
                        TypedValue::new("ptr".to_owned(), slot),
                        lhs_ty,
                    );
                }
                Err(ice(format!(
                    "LLVM lowering does not implement strView for {ty:?}"
                )))
            }
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
            AmirRvalue::Discriminant { value } => self.emit_discriminant(result, lhs_ty, value),
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
            AmirRvalue::SliceView { data, len, .. } => {
                let pointer_ty = self.module.void_ptr_ty();
                let data = self.operand(data, &pointer_ty)?;
                let length_ty = self.module.usize_ty();
                let len = self.operand(len, &length_ty)?;
                let len = self.coerce(len, &self.module.usize_ty())?;
                let first = self.new_value();
                self.writer.line(format!(
                    "{first} = insertvalue {fat} undef, ptr {}, 0",
                    data.value
                ));
                self.writer.line(format!(
                    "{result} = insertvalue {fat} {first}, {} {}, 1",
                    len.ty, len.value
                ));
                Ok(())
            }
            AmirRvalue::SliceData(slice) => {
                let slice_ty = self.module.operand_type(slice, self.func);
                let slice = self.operand(slice, &slice_ty)?;
                self.writer
                    .line(format!("{result} = extractvalue {fat} {}, 0", slice.value));
                Ok(())
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let slice_ty = self.module.operand_type(slice, self.func);
                let slice = self.operand(slice, &slice_ty)?;
                let pointer = self.new_value();
                self.writer
                    .line(format!("{pointer} = extractvalue {fat} {}, 0", slice.value));
                let start_ty = self.module.usize_ty();
                let start = self.operand(start, &start_ty)?;
                let start = self.coerce(start, &self.module.usize_ty())?;
                let index_ll = start.ty.clone();
                let element_ty = match &slice_ty {
                    ArType::Slice(inner) | ArType::MutSlice(inner) => {
                        self.module.interner.resolve(*inner)
                    }
                    other => {
                        return Err(ice(format!(
                            "slice subslice requires a slice, got {other:?}"
                        )));
                    }
                };
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let offset = self.new_value();
                self.writer.line(format!(
                    "{offset} = mul {index_ll} {}, {stride}",
                    start.value
                ));
                let data = self.new_value();
                self.writer.line(format!(
                    "{data} = getelementptr inbounds i8, ptr {pointer}, {index_ll} {offset}"
                ));
                let length_ty = self.module.usize_ty();
                let len = self.operand(len, &length_ty)?;
                let len = self.coerce(len, &self.module.usize_ty())?;
                let first = self.new_value();
                self.writer
                    .line(format!("{first} = insertvalue {fat} undef, ptr {data}, 0"));
                self.writer.line(format!(
                    "{result} = insertvalue {fat} {first}, {} {}, 1",
                    len.ty, len.value
                ));
                Ok(())
            }
            AmirRvalue::StrBytes { source } => {
                let source_ty = self.module.operand_type(source, self.func);
                let source = self.operand(source, &source_ty)?;
                self.assign_coerced(result, source, lhs_ty)
            }
            AmirRvalue::CoroutineReady {
                value, payload_ty, ..
            } => self.emit_coroutine_ready(result, value, *payload_ty),
            AmirRvalue::GenInsert { .. }
            | AmirRvalue::GenGet { .. }
            | AmirRvalue::GenSet { .. }
            | AmirRvalue::GenUpsert { .. }
            | AmirRvalue::GenRemove { .. } => self.emit_gen_operation(result, lhs_ty, rvalue),
            AmirRvalue::StringInterp { parts } => self.emit_string_interp(result, lhs_ty, parts),
            AmirRvalue::ToStr { value, src_ty } => self.emit_to_str(result, lhs_ty, value, *src_ty),
            AmirRvalue::VectorOp { op, left, right } => {
                self.emit_vector_op(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                self.emit_vector_compare(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => self.emit_vector_select(result, lhs_ty, mask, if_true, if_false),
            AmirRvalue::VectorSplat { value } => self.emit_vector_splat(result, lhs_ty, value),
            AmirRvalue::VectorShuffle { left, right, mask } => {
                self.emit_vector_shuffle(result, lhs_ty, left, right, mask)
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                let ty = self.module.operand_type(vector, self.func);
                let vector = self.operand(vector, &ty)?;
                let (_, _, lane_ty) = self.module.vector_info(&ty).ok_or_else(|| {
                    ice(format!(
                        "vector lane extraction requires a vector, got {ty:?}"
                    ))
                })?;
                let lane_ty = ArType::Primitive(lane_ty);
                let lane_text = self.module.llvm_ty(&lane_ty)?;
                self.writer.line(format!(
                    "{} = extractelement {} {}, i32 {lane}",
                    result, vector.ty, vector.value
                ));
                let _ = lane_text;
                Ok(())
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                let ty = self.module.operand_type(vector, self.func);
                let vector = self.operand(vector, &ty)?;
                let (_, _, lane_ty) = self.module.vector_info(&ty).ok_or_else(|| {
                    ice(format!("vector lane insert requires a vector, got {ty:?}"))
                })?;
                let lane_ty = ArType::Primitive(lane_ty);
                let value = self.operand(value, &lane_ty)?;
                self.writer.line(format!(
                    "{} = insertelement {} {}, {} {}, i32 {lane}",
                    result, vector.ty, vector.value, value.ty, value.value
                ));
                Ok(())
            }
            AmirRvalue::BlackBox { value, .. } => {
                let value = self.operand(value, lhs_ty)?;
                let rendered = value.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
        }
    }

    pub(super) fn emit_binary(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        if matches!(
            op,
            BinaryOp::NullCoalesce | BinaryOp::RangeExclusive | BinaryOp::RangeInclusive
        ) {
            return Err(ice(format!(
                "LLVM backend does not yet lower binary operator {op:?}"
            )));
        }

        let comparison = matches!(
            op,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::LtEqual
                | BinaryOp::GtEqual
        );
        let operand_ty = if comparison {
            self.module.operand_type(left, self.func)
        } else {
            lhs_ty.clone()
        };
        if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
            && self.module.is_fat_pointer(&operand_ty)
        {
            return self.emit_fat_pointer_eq(
                result,
                left,
                right,
                &operand_ty,
                matches!(op, BinaryOp::NotEqual),
            );
        }
        let left = self.operand(left, &operand_ty)?;
        let right = self.operand(right, &operand_ty)?;
        let signed = self.module.is_signed(&operand_ty);
        let float = self.module.is_float(&operand_ty);

        let expression = match op {
            BinaryOp::Equal => {
                if float {
                    format!("fcmp oeq {}, {}", left.render(), right.value)
                } else {
                    format!("icmp eq {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::NotEqual => {
                if float {
                    format!("fcmp une {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ne {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Lt => {
                if float {
                    format!("fcmp olt {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp slt {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ult {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Gt => {
                if float {
                    format!("fcmp ogt {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sgt {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ugt {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::LtEqual => {
                if float {
                    format!("fcmp ole {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sle {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ule {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::GtEqual => {
                if float {
                    format!("fcmp oge {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sge {}, {}", left.render(), right.value)
                } else {
                    format!("icmp uge {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Add => self.arithmetic("add", "fadd", &left, &right, float),
            BinaryOp::Sub => self.arithmetic("sub", "fsub", &left, &right, float),
            BinaryOp::Mul => self.arithmetic("mul", "fmul", &left, &right, float),
            BinaryOp::Div => {
                if float {
                    format!("fdiv {}, {}", left.render(), right.value)
                } else if signed {
                    format!("sdiv {}, {}", left.render(), right.value)
                } else {
                    format!("udiv {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Mod => {
                if float {
                    format!("frem {}, {}", left.render(), right.value)
                } else if signed {
                    format!("srem {}, {}", left.render(), right.value)
                } else {
                    format!("urem {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::BitOr => self.arithmetic("or", "or", &left, &right, false),
            BinaryOp::BitXor => self.arithmetic("xor", "xor", &left, &right, false),
            BinaryOp::BitAnd => self.arithmetic("and", "and", &left, &right, false),
            BinaryOp::Or | BinaryOp::And => {
                let instruction = if op == BinaryOp::Or { "or" } else { "and" };
                format!("{instruction} {}, {}", left.render(), right.value)
            }
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                let Some(width) = self.module.integer_width(&operand_ty) else {
                    return Err(ice(format!(
                        "shift requires an integer, got {operand_ty:?}"
                    )));
                };
                let shift_ty = left.ty.clone();
                let mask = self.new_value();
                self.writer.line(format!(
                    "{mask} = and {shift_ty} {}, {}",
                    right.value,
                    width.saturating_sub(1)
                ));
                let instruction = match op {
                    BinaryOp::ShiftLeft => "shl",
                    BinaryOp::ShiftRight if signed => "ashr",
                    BinaryOp::ShiftRight => "lshr",
                    _ => unreachable!(),
                };
                format!("{instruction} {shift_ty} {}, {mask}", left.value)
            }
            _ => {
                return Err(ice(format!(
                    "LLVM backend does not yet lower binary operator {op:?}"
                )));
            }
        };
        self.writer.line(format!("{} = {expression}", result));
        Ok(())
    }

    pub(super) fn arithmetic(
        &self,
        integer: &str,
        float: &str,
        left: &TypedValue,
        right: &TypedValue,
        is_float: bool,
    ) -> String {
        let instruction = if is_float { float } else { integer };
        format!("{instruction} {}, {}", left.render(), right.value)
    }

    pub(super) fn emit_unary(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: UnaryOp,
        operand: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        match op {
            UnaryOp::Neg => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let value = self.operand(operand, &operand_ty)?;
                if self.module.is_float(&operand_ty) {
                    self.writer
                        .line(format!("{} = fneg {}", result, value.render()));
                } else {
                    self.writer
                        .line(format!("{} = sub {} 0, {}", result, value.ty, value.value));
                }
                Ok(())
            }
            UnaryOp::Not | UnaryOp::BitNot => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let value = self.operand(operand, &operand_ty)?;
                let all_ones = if value.ty == "i1" {
                    "true".to_owned()
                } else {
                    "-1".to_owned()
                };
                self.writer.line(format!(
                    "{} = xor {} {}, {all_ones}",
                    result, value.ty, value.value
                ));
                Ok(())
            }
            UnaryOp::Await => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = self.operand(operand, &operand_ty)?;
                let payload_ty = match &operand_ty {
                    ArType::Coroutine(inner) => self.module.interner.resolve(*inner),
                    _ => lhs_ty.clone(),
                };
                let address = self.byte_gep(&pointer.value, 8);
                let payload_text = self.module.llvm_ty(&payload_ty)?;
                let align = self.module.align_of(&payload_ty);
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {payload_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty)
            }
            UnaryOp::Deref => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = self.operand(operand, &operand_ty)?;
                let pointee = match &operand_ty {
                    ArType::Ptr(inner)
                    | ArType::Ref(inner)
                    | ArType::RefMut(inner)
                    | ArType::Nullable(inner) => self.module.interner.resolve(*inner),
                    other => {
                        return Err(ice(format!("LLVM deref requires a pointer, got {other:?}")));
                    }
                };
                let pointee_text = self.module.llvm_ty(&pointee)?;
                let align = self.module.align_of(&pointee);
                self.writer.line(format!(
                    "{} = load {pointee_text}, ptr {}, align {align}",
                    result, pointer.value
                ));
                let _ = lhs_ty;
                Ok(())
            }
            other => Err(ice(format!(
                "LLVM backend does not yet lower unary operator {other:?}"
            ))),
        }
    }

    pub(super) fn emit_vector_op(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: VectorOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (_, _, lane) = self.module.vector_info(lhs_ty).ok_or_else(|| {
            ice(format!(
                "vector operation requires a vector, got {lhs_ty:?}"
            ))
        })?;
        let left = self.operand(left, lhs_ty)?;
        let right = self.operand(right, lhs_ty)?;
        let float = lane.is_float();
        let signed = lane.is_signed();
        let instruction = match op {
            VectorOp::Add => {
                if float {
                    "fadd"
                } else {
                    "add"
                }
            }
            VectorOp::Sub => {
                if float {
                    "fsub"
                } else {
                    "sub"
                }
            }
            VectorOp::Mul => {
                if float {
                    "fmul"
                } else {
                    "mul"
                }
            }
            VectorOp::Div => {
                if float {
                    "fdiv"
                } else if signed {
                    "sdiv"
                } else {
                    "udiv"
                }
            }
            VectorOp::BitAnd => "and",
            VectorOp::BitOr => "or",
            VectorOp::BitXor => "xor",
        };
        self.writer.line(format!(
            "{} = {instruction} {}, {}",
            result,
            left.render(),
            right.value
        ));
        Ok(())
    }

    pub(super) fn emit_vector_compare(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: VectorCompareOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let ty = self.module.operand_type(left, self.func);
        let (_, lanes, lane) = self
            .module
            .vector_info(&ty)
            .ok_or_else(|| ice(format!("vector comparison requires a vector, got {ty:?}")))?;
        let left = self.operand(left, &ty)?;
        let right = self.operand(right, &ty)?;
        let lane_ty = ArType::Primitive(lane);
        let lane_text = self.module.llvm_ty(&lane_ty)?;
        let predicate = if lane.is_float() {
            match op {
                VectorCompareOp::Eq => "oeq",
                VectorCompareOp::Ne => "une",
                VectorCompareOp::Lt => "olt",
                VectorCompareOp::Le => "ole",
                VectorCompareOp::Gt => "ogt",
                VectorCompareOp::Ge => "oge",
            }
        } else {
            match op {
                VectorCompareOp::Eq => "eq",
                VectorCompareOp::Ne => "ne",
                VectorCompareOp::Lt => {
                    if lane.is_signed() {
                        "slt"
                    } else {
                        "ult"
                    }
                }
                VectorCompareOp::Le => {
                    if lane.is_signed() {
                        "sle"
                    } else {
                        "ule"
                    }
                }
                VectorCompareOp::Gt => {
                    if lane.is_signed() {
                        "sgt"
                    } else {
                        "ugt"
                    }
                }
                VectorCompareOp::Ge => {
                    if lane.is_signed() {
                        "sge"
                    } else {
                        "uge"
                    }
                }
            }
        };
        let instruction = if lane.is_float() { "fcmp" } else { "icmp" };
        let mask = self.new_value();
        self.writer.line(format!(
            "{mask} = {instruction} {predicate} {}, {}",
            left.render(),
            right.value
        ));
        let extended = self.new_value();
        self.writer.line(format!(
            "{extended} = zext <{lanes} x i1> {mask} to <{lanes} x {lane_text}>"
        ));
        let negated = self.new_value();
        self.writer.line(format!(
            "{negated} = sub <{lanes} x {lane_text}> zeroinitializer, {extended}"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <{lanes} x {lane_text}> {negated} to {result_ty}"
        ));
        Ok(())
    }

    pub(super) fn emit_vector_select(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        mask: &AmirOperand,
        if_true: &AmirOperand,
        if_false: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let mask_ty = ArType::Vector(VectorType::U8x16);
        let mask = self.operand(mask, &mask_ty)?;
        let if_true = self.operand(if_true, lhs_ty)?;
        let if_false = self.operand(if_false, lhs_ty)?;
        let mask_bytes = mask.value;
        let true_bytes = self.new_value();
        self.writer.line(format!(
            "{true_bytes} = bitcast {} to <16 x i8>",
            if_true.render()
        ));
        let false_bytes = self.new_value();
        self.writer.line(format!(
            "{false_bytes} = bitcast {} to <16 x i8>",
            if_false.render()
        ));
        let inverse = self.new_value();
        self.writer.line(format!(
            "{inverse} = xor <16 x i8> {mask_bytes}, <i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1>"
        ));
        let selected_true = self.new_value();
        self.writer.line(format!(
            "{selected_true} = and <16 x i8> {mask_bytes}, {true_bytes}"
        ));
        let selected_false = self.new_value();
        self.writer.line(format!(
            "{selected_false} = and <16 x i8> {inverse}, {false_bytes}"
        ));
        let combined = self.new_value();
        self.writer.line(format!(
            "{combined} = or <16 x i8> {selected_true}, {selected_false}"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <16 x i8> {combined} to {result_ty}"
        ));
        Ok(())
    }

    pub(super) fn emit_vector_splat(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (_, lanes, lane) = self
            .module
            .vector_info(lhs_ty)
            .ok_or_else(|| ice(format!("vector splat requires a vector, got {lhs_ty:?}")))?;
        let lane_ty = ArType::Primitive(lane);
        let value = self.operand(value, &lane_ty)?;
        let vector = self.module.llvm_ty(lhs_ty)?;
        let inserted = self.new_value();
        self.writer.line(format!(
            "{inserted} = insertelement {vector} undef, {}, i32 0",
            value.render()
        ));
        self.writer.line(format!(
            "{} = shufflevector {vector} {inserted}, {vector} undef, <{lanes} x i32> zeroinitializer",
            result
        ));
        Ok(())
    }

    pub(super) fn emit_vector_shuffle(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        left: &AmirOperand,
        right: &AmirOperand,
        mask: &[u8; 16],
    ) -> Result<(), Diagnostic> {
        let ty = self.module.operand_type(left, self.func);
        let left = self.operand(left, &ty)?;
        let right = self.operand(right, &ty)?;
        let left_bytes = self.new_value();
        self.writer.line(format!(
            "{left_bytes} = bitcast {} to <16 x i8>",
            left.render()
        ));
        let right_bytes = self.new_value();
        self.writer.line(format!(
            "{right_bytes} = bitcast {} to <16 x i8>",
            right.render()
        ));
        let mask = mask
            .iter()
            .map(|index| format!("i32 {index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let shuffled = self.new_value();
        self.writer.line(format!(
            "{shuffled} = shufflevector <16 x i8> {left_bytes}, <16 x i8> {right_bytes}, <16 x i32> <{mask}>"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <16 x i8> {shuffled} to {result_ty}"
        ));
        Ok(())
    }
}
