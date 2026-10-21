use arandu_middle::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirTerminator, TempId,
};
use arandu_middle::layout::StructLayoutProvider;
use arandu_middle::literal_pool::LiteralId;
use arandu_middle::types::{ArType, Primitive};
use arandu_middle::{Diagnostic, SymbolId};

use super::{
    FuncEmitter, IntegerWidthFromLlvm, TypedValue, block_label, ensure_float_decimal, ice,
    local_name, parse_byte_aggregate_size, parse_char_literal, quote_name, temp_name,
    value_ar_type,
};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_terminator(
        &mut self,
        terminator: &AmirTerminator,
    ) -> Result<(), Diagnostic> {
        match terminator {
            AmirTerminator::Return => {
                let return_ty = self.module.interner.resolve(self.func.return_type);
                let function_name = self.module.function_name(self.func.symbol);
                if self.module.is_void(&return_ty) {
                    if function_name == "main" {
                        self.writer.line("ret i32 0");
                    } else {
                        self.writer.line("ret void");
                    }
                    return Ok(());
                }
                let Some(value) = ({
                    if let Some(slot) = self.temp_slot(TempId::from_usize(0)).map(str::to_owned) {
                        let ty_text = self.module.llvm_ty(&return_ty)?;
                        let align = self.module.align_of(&return_ty);
                        let loaded = self.new_value();
                        self.writer.line(format!(
                            "{loaded} = load {ty_text}, ptr {slot}, align {align}"
                        ));
                        Some(loaded)
                    } else {
                        self.return_value()
                    }
                }) else {
                    return Err(ice(
                        "AMIR return terminator has no preceding assignment to the return register",
                    ));
                };
                let ty_text = self.module.llvm_ty(&return_ty)?;
                self.writer.line(format!("ret {ty_text} {value}"));
                Ok(())
            }
            AmirTerminator::Goto { target, args } => {
                self.record_edge_args(*target, args)?;
                self.writer
                    .line(format!("br label %{}", block_label(*target)));
                Ok(())
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                let condition = self.operand(condition, &ArType::Primitive(Primitive::Bool))?;
                self.record_edge_args(*if_true, true_args)?;
                self.record_edge_args(*if_false, false_args)?;
                self.writer.line(format!(
                    "br i1 {}, label %{}, label %{}",
                    condition.value,
                    block_label(*if_true),
                    block_label(*if_false)
                ));
                Ok(())
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let ty = self.module.operand_type(discriminant, self.func);
                let value = self.operand(discriminant, &ty)?;
                let mut cases = Vec::with_capacity(targets.len());
                for (case, target, args) in targets {
                    self.record_edge_args(*target, args)?;
                    cases.push(format!(
                        "{} {}, label %{}",
                        value.ty,
                        case,
                        block_label(*target)
                    ));
                }
                self.record_edge_args(otherwise.0, &otherwise.1)?;
                self.writer.line(format!(
                    "switch {} {}, label %{} [ {} ]",
                    value.ty,
                    value.value,
                    block_label(otherwise.0),
                    cases.join(" ")
                ));
                Ok(())
            }
            AmirTerminator::Unreachable => {
                self.writer.line("call void @llvm.trap()");
                self.writer.line("unreachable");
                Ok(())
            }
            AmirTerminator::Suspend { resume, args, .. } => {
                // Ready-only coroutines: suspension is a direct edge to the
                // resume block, matching the C backend's A3.1 lowering.
                self.record_edge_args(*resume, args)?;
                self.writer
                    .line(format!("br label %{}", block_label(*resume)));
                Ok(())
            }
        }
    }

    pub(super) fn place(&mut self, place: &AmirPlace) -> Result<(String, ArType), Diagnostic> {
        let local = &self.func.locals[place.local.as_usize()];
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

    pub(super) fn operand(
        &mut self,
        operand: &AmirOperand,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        match operand {
            AmirOperand::Constant(constant) => self.constant(constant, expected),
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                let actual_ty = self.temp_type(*temp);
                let override_ty = self.temp_llvm_ty[temp.as_usize()].clone();
                if let Some(slot) = self.temp_slot(*temp).map(str::to_owned) {
                    let ty_text = override_ty
                        .clone()
                        .map_or_else(|| self.module.llvm_ty(&actual_ty), Ok)?;
                    let align = self.module.align_of(&actual_ty);
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load {ty_text}, ptr {slot}, align {align}"
                    ));
                    let mut loaded_value = TypedValue::new(ty_text, loaded);
                    if self.module.integer_width(&actual_ty).is_some() {
                        loaded_value =
                            loaded_value.with_source_signed(self.module.is_signed(&actual_ty));
                    }
                    return self.coerce(loaded_value, expected);
                }
                let value = if temp.as_usize() == 0 {
                    self.return_value()
                        .ok_or_else(|| ice("return register is read before it is assigned"))?
                } else {
                    self.ensure_defined(*temp)?;
                    temp_name(*temp)
                };
                let llvm_ty = override_ty.map_or_else(|| self.module.llvm_ty(&actual_ty), Ok)?;
                let mut actual = TypedValue::new(llvm_ty, value);
                // Prefer AMIR temp signedness: `u8 as u32` must zext, not sext.
                if self.module.integer_width(&actual_ty).is_some() {
                    actual = actual.with_source_signed(self.module.is_signed(&actual_ty));
                }
                self.coerce(actual, expected)
            }
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => {
                let value = TypedValue::new(
                    "ptr".to_owned(),
                    format!("@{}", quote_name(&self.module.function_name(*symbol))),
                );
                self.coerce(value, expected)
            }
        }
    }

    pub(super) fn constant(
        &self,
        constant: &AmirConstant,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        let expected_text = self.module.llvm_ty(expected)?;
        match constant {
            AmirConstant::Bool(value) => {
                // `true`/`false` are i1 tokens; integer projections (ToStr →
                // `ar_jit_bool_to_str(i8, ...)`) need numeric 0/1 literals.
                let literal = if expected_text == "i1" {
                    if *value { "true" } else { "false" }
                } else if *value {
                    "1"
                } else {
                    "0"
                };
                Ok(TypedValue::new(expected_text, literal.to_owned()))
            }
            AmirConstant::Nil => {
                if expected_text == "ptr" {
                    Ok(TypedValue::new("ptr".to_owned(), "null".to_owned()))
                } else {
                    Ok(TypedValue::new(expected_text, "0".to_owned()))
                }
            }
            AmirConstant::Pool(id) => self.literal(*id, expected, &expected_text),
        }
    }

    pub(super) fn literal(
        &self,
        id: LiteralId,
        expected: &ArType,
        expected_text: &str,
    ) -> Result<TypedValue, Diagnostic> {
        use arandu_middle::literal_pool::AmirLiteralEntry;
        match self.module.program.literal_pool.get(id) {
            AmirLiteralEntry::Int(text) => {
                let value = arandu_middle::literal_pool::parse_int_literal(text)
                    .ok_or_else(|| ice(format!("invalid integer literal {text:?}")))?;
                let width = self
                    .module
                    .integer_width(expected)
                    .or_else(|| {
                        if expected_text == "ptr" {
                            Some(self.module.layout.pointer_width())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(32);
                let masked = if width >= 128 {
                    value as u128
                } else {
                    (value as u128) & ((1u128 << width) - 1)
                };
                let rendered = if expected_text == "float" || expected_text == "double" {
                    format!("{masked}.0")
                } else {
                    masked.to_string()
                };
                Ok(TypedValue::new(expected_text.to_owned(), rendered))
            }
            AmirLiteralEntry::Float(text) => {
                let value = arandu_middle::literal_pool::parse_float_literal(text)
                    .ok_or_else(|| ice(format!("invalid float literal {text:?}")))?;
                let rendered = if expected_text == "float" {
                    format!("{:?}", value as f32)
                } else {
                    format!("{value:?}")
                };
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    ensure_float_decimal(&rendered),
                ))
            }
            AmirLiteralEntry::FloatBits(value) => {
                let rendered = if value.ty().bit_width() == 32 {
                    format!("{:?}", f32::from_bits(value.bits() as u32))
                } else {
                    format!("{:?}", f64::from_bits(value.bits()))
                };
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    ensure_float_decimal(&rendered),
                ))
            }
            AmirLiteralEntry::Char(text) => {
                let ch = parse_char_literal(text)
                    .ok_or_else(|| ice(format!("invalid char literal {text:?}")))?;
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    u32::from(ch).to_string(),
                ))
            }
            AmirLiteralEntry::Str(_) => {
                let index = id.0;
                let (name, length) = self
                    .module
                    .string_globals
                    .get(&index)
                    .ok_or_else(|| ice("string literal has no LLVM global"))?;
                if expected_text == "ptr" {
                    Ok(TypedValue::new(
                        "ptr",
                        format!(
                            "getelementptr inbounds ([{} x i8], ptr {name}, i64 0, i64 0)",
                            length + 1
                        ),
                    ))
                } else if expected_text == "{ ptr, i64 }" {
                    Ok(TypedValue::new(
                        expected_text.to_owned(),
                        format!("{{ ptr {name}, i64 {length} }}"),
                    ))
                } else if self.module.is_pointer(expected) {
                    Ok(TypedValue::new(
                        "ptr",
                        format!(
                            "getelementptr inbounds ([{} x i8], ptr {name}, i64 0, i64 0)",
                            length + 1
                        ),
                    ))
                } else {
                    Err(ice(format!(
                        "LLVM backend cannot use a string literal as {expected:?}"
                    )))
                }
            }
        }
    }

    pub(super) fn coerce(
        &mut self,
        value: TypedValue,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        let expected_text = self.module.llvm_ty(expected)?;
        if value.ty == expected_text {
            return Ok(TypedValue::new(expected_text, value.value));
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
            } else if expected_text == "{ ptr, i64 }" || self.module.is_fat_pointer(expected) {
                // Load a fat `{data,len}` descriptor from behind the pointer
                // (e.g. `ref String` / `ref str` → by-value `str` / `[]T`).
                let loaded = self.new_value();
                let align = self.module.pointer_align();
                self.writer.line(format!(
                    "{loaded} = load {{ ptr, i64 }}, ptr {}, align {align}",
                    value.value
                ));
                return Ok(TypedValue::new("{ ptr, i64 }".to_owned(), loaded));
            } else if let Some(bytes) = parse_byte_aggregate_size(&expected_text) {
                // Pointer-repr aggregate (Cranelift-style EnumPayload of a
                // Named struct): materialize by copying `bytes` from the
                // pointee into a first-class opaque value.
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {expected_text}, align 1"));
                self.writer.line(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {slot}, ptr {}, i64 {bytes}, i1 false)",
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
                self.writer.line(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {copy}, i1 false)"
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
            return Ok(TypedValue::new(expected_text, value.value));
        };
        let result = self.new_value();
        self.writer.line(format!("{result} = {instruction}"));
        Ok(TypedValue::new(expected_text, result))
    }

    pub(super) fn assign_coerced(
        &mut self,
        result: &str,
        value: TypedValue,
        lhs_ty: &ArType,
    ) -> Result<(), Diagnostic> {
        let value = self.coerce(value, lhs_ty)?;
        let rendered = value.render();
        self.writer
            .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
        Ok(())
    }

    /// Build a typed LLVM value, recording AMIR integer signedness for casts.
    pub(super) fn typed_from_ar(&self, llvm_ty: String, value: String, ar: &ArType) -> TypedValue {
        let mut typed = TypedValue::new(llvm_ty, value);
        if self.module.integer_width(ar).is_some() {
            typed = typed.with_source_signed(self.module.is_signed(ar));
        }
        typed
    }

    pub(super) fn scratch_alloca(&mut self, ty: &ArType) -> Result<String, Diagnostic> {
        let (text, align) = self.module.alloca_type(ty)?;
        let slot = self.new_value();
        self.writer
            .line(format!("{slot} = alloca {text}, align {align}"));
        Ok(slot)
    }

    pub(super) fn byte_gep(&mut self, base: &str, offset: u64) -> String {
        if offset == 0 {
            return base.to_owned();
        }
        let next = self.new_value();
        self.writer.line(format!(
            "{next} = getelementptr inbounds i8, ptr {base}, i64 {offset}"
        ));
        next
    }

    pub(super) fn aggregate_address(
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

    pub(super) fn store_value(
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

    pub(super) fn aggregate_field_address(
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

    pub(super) fn enum_payload_type(
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

    pub(super) fn store_enum_integer(
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
