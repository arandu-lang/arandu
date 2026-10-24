use std::borrow::Cow;

use super::{CEmitter, sanitize_c_ident};
use arandu_middle::amir::{AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProjection};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};

const C_TRUE: &str = "true";
const C_FALSE: &str = "false";
const C_NULL: &str = "0";

impl<'a> CEmitter<'a> {
    pub(super) fn temp_slot(&self, temp: arandu_middle::amir::TempId) -> usize {
        self.temp_slots
            .get(temp.as_usize())
            .copied()
            .unwrap_or(temp.as_usize())
    }
    #[inline]
    pub(super) fn is_64bit_target(&self) -> bool {
        self.layout.pointer_width() == 8
    }

    pub(super) fn format_operand_str(&self, op: &AmirOperand) -> String {
        match op {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => self
                .temp_expressions
                .get(t.as_usize())
                .and_then(Option::as_ref)
                .cloned()
                .unwrap_or_else(|| format!("t{}", self.temp_slot(*t))),
            AmirOperand::FunctionRef(id) => self.c_function_name(*id),
            AmirOperand::GlobalRef(id) => {
                let sym = self.symbols.get(*id);
                sanitize_c_ident(self.symbols.host_func_name(sym))
            }
            AmirOperand::Constant(c) => match c {
                AmirConstant::Pool(id) => match self.program.literal_pool.get(*id) {
                    AmirLiteralEntry::Int(v) => {
                        arandu_middle::literal_pool::int_literal_c_source(v)
                            .unwrap_or_else(|| v.to_string())
                    }
                    AmirLiteralEntry::Float(v) => {
                        arandu_middle::literal_pool::float_literal_c_source(v)
                            .unwrap_or_else(|| v.to_string())
                    }
                    AmirLiteralEntry::FloatBits(value) => {
                        if value.ty().bit_width() == 32 {
                            format!("ar_f32_from_bits(UINT32_C({}))", value.bits())
                        } else {
                            format!("ar_f64_from_bits(UINT64_C({}))", value.bits())
                        }
                    }
                    AmirLiteralEntry::Str(_) => {
                        // Prefer named constant when available; compound literal fallback
                        // is handled in format_operand for pool constants.
                        "((ArStr){ .ptr = (const uint8_t*)\"\", .len = 0 })".to_string()
                    }
                    AmirLiteralEntry::Char(v) => {
                        let scalar = v.chars().next().unwrap_or('\0') as u32;
                        format!("UINT32_C({scalar})")
                    }
                },
                AmirConstant::Bool(b) => {
                    if *b {
                        C_TRUE.to_string()
                    } else {
                        C_FALSE.to_string()
                    }
                }
                AmirConstant::Nil => C_NULL.to_string(),
            },
        }
    }

    pub(super) fn format_operand(&self, op: &AmirOperand, _func: &AmirFunc) -> String {
        // Delegates to `format_operand_str` for most operands. Pool string literals are a
        // special case: they must be emitted as an `ArStr` fat-pointer (ptr + len) rather
        // than a raw pointer, using a compound-literal array cast.
        match op {
            AmirOperand::Constant(AmirConstant::Pool(id)) => {
                match self.program.literal_pool.get(*id) {
                    AmirLiteralEntry::Str(s) => {
                        // LayoutEngine Str: named constant or compound literal {ptr, len}.
                        let _ = s;
                        format!("AR_STR_{}", id.0)
                    }
                    _ => self.format_operand_str(op),
                }
            }
            _ => self.format_operand_str(op),
        }
    }

    pub(super) fn format_type(&self, ty: &ArType) -> Cow<'static, str> {
        match ty {
            ArType::Vector(vector) => Cow::Owned(format!("ArVector_{}", vector.name())),
            ArType::Primitive(Primitive::I8) => Cow::Borrowed("int8_t"),
            ArType::Primitive(Primitive::I16) => Cow::Borrowed("int16_t"),
            ArType::Primitive(Primitive::I32) => Cow::Borrowed("int32_t"),
            ArType::Primitive(Primitive::I64) => Cow::Borrowed("int64_t"),
            ArType::Primitive(Primitive::U8) | ArType::Primitive(Primitive::Byte) => {
                Cow::Borrowed("uint8_t")
            }
            ArType::Primitive(Primitive::U16) => Cow::Borrowed("uint16_t"),
            ArType::Primitive(Primitive::U32) => Cow::Borrowed("uint32_t"),
            ArType::Primitive(Primitive::U64) => Cow::Borrowed("uint64_t"),
            ArType::Primitive(Primitive::F32) => Cow::Borrowed("float"),
            ArType::Primitive(Primitive::F64) => Cow::Borrowed("double"),
            ArType::Primitive(Primitive::Uint) => Cow::Borrowed("uint32_t"),
            ArType::Primitive(Primitive::Int) => Cow::Borrowed("int32_t"),
            ArType::IntLiteral => Cow::Borrowed("int32_t"),
            ArType::Primitive(Primitive::USize) => {
                if self.is_64bit_target() {
                    Cow::Borrowed("uint64_t")
                } else {
                    Cow::Borrowed("uint32_t")
                }
            }
            ArType::Primitive(Primitive::ISize) => {
                if self.is_64bit_target() {
                    Cow::Borrowed("int64_t")
                } else {
                    Cow::Borrowed("int32_t")
                }
            }
            ArType::Primitive(Primitive::Bool) => Cow::Borrowed("bool"),
            ArType::Primitive(Primitive::Char) => Cow::Borrowed("uint32_t"),
            ArType::Primitive(Primitive::Str) => Cow::Borrowed("ArStr"),
            ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => Cow::Borrowed("double"),
            ArType::Void => Cow::Borrowed("void"),
            // Coroutine values point to runtime state, independently of their
            // result type. LayoutEngine also models them as one target pointer.
            ArType::Coroutine(_) => Cow::Borrowed("void*"),
            ArType::Ref(inner) | ArType::RefMut(inner)
                if ty.slice_abi_element(self.interner).is_some() =>
            {
                self.format_type(&self.interner.resolve(*inner))
            }
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => Cow::Owned(format!(
                "{}*",
                self.format_type(&self.interner.resolve(*inner))
            )),
            ArType::GenRef => Cow::Borrowed("int64_t"),
            ArType::Named(id, args) => {
                let arguments = self.interner.type_args(*args);
                let name = if arguments.is_empty() {
                    self.symbols.get(*id).name.to_string()
                } else {
                    arandu_middle::symbol_mangling::mangle_symbol(
                        &arandu_middle::symbol_mangling::InstantiationKey {
                            symbol: *id,
                            type_args: &arguments,
                        },
                        self.interner,
                        self.symbols,
                    )
                };
                Cow::Owned(sanitize_c_ident(&name))
            }
            ArType::Slice(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                Cow::Owned(format!("ArType_Slice_{}", sanitize_c_ident(&inner_name)))
            }
            ArType::MutSlice(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                Cow::Owned(format!("ArType_MutSlice_{}", sanitize_c_ident(&inner_name)))
            }
            ArType::Array(len, inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                Cow::Owned(format!(
                    "ArType_Array_{}_{}",
                    len,
                    sanitize_c_ident(&inner_name)
                ))
            }
            ArType::Nullable(inner) => {
                let inner_ty = self.interner.resolve(*inner);
                // Nullable thin references retain their null-or-pointer ABI.
                // Fat slice borrows need a separate descriptor representation.
                if matches!(
                    inner_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                ) && inner_ty.slice_abi_element(self.interner).is_none()
                {
                    return self.format_type(&inner_ty);
                }
                let inner_name = self.format_type(&inner_ty);
                Cow::Owned(format!("ArType_Nullable_{}", sanitize_c_ident(&inner_name)))
            }
            ArType::Option(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                Cow::Owned(format!("ArType_Option_{}", sanitize_c_ident(&inner_name)))
            }
            ArType::Result(ok, err) => {
                let ok_name = self.format_type(&self.interner.resolve(*ok));
                let err_name = self.format_type(&self.interner.resolve(*err));
                Cow::Owned(format!(
                    "ArType_Result_{}_{}",
                    sanitize_c_ident(&ok_name),
                    sanitize_c_ident(&err_name)
                ))
            }
            ArType::Tuple(tys) => {
                let mut name = "ArType_Tuple".to_string();
                for &t in self.interner.type_args(*tys).iter() {
                    name.push('_');
                    name.push_str(&self.format_type(&self.interner.resolve(t)));
                }
                Cow::Owned(sanitize_c_ident(&name))
            }
            ArType::Func(params, ret) => {
                let mut name = "ArFunc".to_string();
                for &p in self.interner.type_args(*params).iter() {
                    name.push('_');
                    name.push_str(&self.format_type(&self.interner.resolve(p)));
                }
                name.push_str("_to_");
                name.push_str(&self.format_type(&self.interner.resolve(*ret)));
                Cow::Owned(sanitize_c_ident(&name))
            }
            _ => Cow::Owned(format!("ArType_{}", sanitize_c_ident(&format!("{:?}", ty)))),
        }
    }

    /// Bounds-checked subscript for a fixed-length array element.
    ///
    /// Mirrors the Cranelift/Wasm trap: an index outside `[0, len)` aborts
    /// instead of reading or writing out of bounds.
    pub(super) fn checked_array_index(index: &str, len: u64) -> String {
        format!("ar_index_check((uint64_t)({index}), UINT64_C({len}))")
    }

    /// Bounds-checked subscript for an element of the fat slice lvalue `slice`.
    pub(super) fn checked_slice_index(&self, index: &str, slice: &str) -> String {
        let off = self.layout.fat_ptr_len_offset();
        let len_ty = if self.layout.fat_ptr_len_size() == 4 {
            "int32_t"
        } else {
            "int64_t"
        };
        format!(
            "ar_index_check((uint64_t)({index}), (uint64_t)(*({len_ty}*)((uint8_t*)&{slice} + {off})))"
        )
    }

    pub(super) fn format_place(&mut self, place: &AmirPlace, func: &AmirFunc) -> String {
        let local_idx = place.local.as_usize();
        let mut current_ty = self.local_ty(func, place.local);
        let mut path = format!("l{}", local_idx);
        let mut address = format!("&{path}");

        for proj in &place.projections {
            match proj {
                // BC.4a: place through pointer value — lvalue is `*path`.
                AmirProjection::Deref => {
                    let borrowed_slice = current_ty.is_borrowed_slice_abi(self.interner);
                    current_ty = match &current_ty {
                        ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner)
                        | ArType::Nullable(inner) => self.interner.resolve(*inner),
                        other => other.clone(),
                    };
                    if !borrowed_slice {
                        address = path.clone();
                        path = format!("(*{})", path);
                    }
                }
                AmirProjection::Field(field_symbol_id) => {
                    // After Deref, current_ty is the pointee; unwrap residual ptr-likes.
                    let struct_ty = match &current_ty {
                        ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner)
                        | ArType::Nullable(inner) => self.interner.resolve(*inner),
                        other => other.clone(),
                    };
                    let layout = self.checked_layout(&struct_ty);
                    let Some(field_symbol) = self.symbols.try_get(*field_symbol_id) else {
                        self.record_codegen_ice(
                            func,
                            format!("place projection references unknown field symbol {field_symbol_id:?}"),
                        );
                        return "/* invalid field symbol */ 0".to_string();
                    };
                    let field_name = field_symbol.name.rsplit('.').next().unwrap_or("");
                    let Some(field_idx) = self
                        .provider
                        .get_struct_fields_for_type(&struct_ty, self.interner)
                        .and_then(|fields| fields.get(field_name))
                        .map(|field| field.index)
                    else {
                        self.record_codegen_ice(
                            func,
                            format!("place projection references unknown field `{field_name}`"),
                        );
                        return "/* invalid field projection */ 0".to_string();
                    };
                    let Some(offset) = layout.field_offsets.get(field_idx).copied() else {
                        self.record_codegen_ice(
                            func,
                            format!(
                                "place field `{field_name}` index {field_idx} is outside the layout"
                            ),
                        );
                        return "/* invalid field layout */ 0".to_string();
                    };

                    let field_ty = self.instantiated_field_ty(&struct_ty, field_name);
                    let field_c_ty = self.format_type(&field_ty);
                    // If path is already a pointer (heap/ptr local without Deref), GEP from
                    // the pointer value; otherwise take address of the stack lvalue.
                    let base = if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                    ) {
                        path.clone()
                    } else {
                        address.clone()
                    };
                    address = format!("({field_c_ty}*)((uint8_t*)({base}) + {offset})");
                    path = format!("*({address})");
                    current_ty = field_ty;
                }
                AmirProjection::Variant(_) => {}
                AmirProjection::Payload {
                    index,
                    field_ty,
                    tuple_ty,
                    ..
                } => {
                    let enum_ty = match &current_ty {
                        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                            self.interner.resolve(*inner)
                        }
                        other => other.clone(),
                    };
                    let enum_layout = self.checked_layout(&enum_ty);
                    let payload_offset = enum_layout.field_offsets.get(1).copied().unwrap_or(0);
                    let tuple_offset = tuple_ty
                        .map(|tuple_ty| {
                            self.checked_layout(&self.interner.resolve(tuple_ty))
                                .field_offsets
                                .get(*index)
                                .copied()
                                .unwrap_or(0)
                        })
                        .unwrap_or(0);
                    let offset = payload_offset.saturating_add(tuple_offset);
                    let field_c_ty = self.format_type(&self.interner.resolve(*field_ty));
                    if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                    ) {
                        path = format!("*({field_c_ty}*)((uint8_t*){path} + {offset})");
                    } else {
                        path = format!("*({field_c_ty}*)((uint8_t*)&{path} + {offset})");
                    }
                    current_ty = self.interner.resolve(*field_ty);
                }
                AmirProjection::TupleField(index) => {
                    let tuple_layout = self.checked_layout(&current_ty);
                    let offset = tuple_layout.field_offsets.get(*index).copied().unwrap_or(0);
                    let field_ty = match &current_ty {
                        ArType::Tuple(args) => self
                            .interner
                            .type_args(*args)
                            .get(*index)
                            .copied()
                            .map(|field_ty| self.interner.resolve(field_ty))
                            .unwrap_or(ArType::Error),
                        _ => ArType::Error,
                    };
                    let field_c_ty = self.format_type(&field_ty);
                    path = format!("*({field_c_ty}*)((uint8_t*)&{path} + {offset})");
                    current_ty = field_ty;
                }
                AmirProjection::Index(index_op) => {
                    if current_ty.is_borrowed_slice_abi(self.interner)
                        && let ArType::Ref(inner) | ArType::RefMut(inner) = &current_ty
                    {
                        current_ty = self.interner.resolve(*inner);
                    }
                    let pointee_array = match &current_ty {
                        ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                            match self.interner.resolve(*inner) {
                                ArType::Array(len, elem) => Some((len, elem)),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    let is_vec = arandu_middle::types::is_vec_type(&current_ty, self.symbols);
                    let elem_ty = if let Some((_, elem_id)) = pointee_array {
                        self.interner.resolve(elem_id)
                    } else {
                        match arandu_middle::types::index_elem_type(
                            &current_ty,
                            self.symbols,
                            self.interner,
                        ) {
                            Some(id) => self.interner.resolve(id),
                            None => ArType::Error,
                        }
                    };
                    let elem_c_ty = self.format_type(&elem_ty);
                    let index_str = self.format_operand(index_op, func);

                    if let Some((len, elem_id)) = pointee_array
                        && len > 0
                        && self.checked_layout(&ArType::Array(len, elem_id)).size > 0
                    {
                        let index_str = Self::checked_array_index(&index_str, len);
                        path = format!("({})->data[{}]", path, index_str);
                    } else if let ArType::Array(len, _) = &current_ty
                        && *len > 0
                        && self.checked_layout(&current_ty).size > 0
                    {
                        let index_str = Self::checked_array_index(&index_str, *len);
                        path = format!("({}).data[{}]", path, index_str);
                    } else if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                    ) {
                        path = format!("(({}*){})[{}]", elem_c_ty, path, index_str);
                    } else if matches!(current_ty, ArType::Slice(_) | ArType::MutSlice(_)) {
                        let index_str = self.checked_slice_index(&index_str, &path);
                        path = format!(
                            "(( {}* )(*(void**)((uint8_t*)&{} + 0)))[{}]",
                            elem_c_ty, path, index_str
                        );
                    } else if is_vec {
                        path = format!(
                            "(( {}* )(*(void**)((uint8_t*)&{} + 0)))[{}]",
                            elem_c_ty, path, index_str
                        );
                    } else {
                        let index_str = match &current_ty {
                            ArType::Array(len, _) => Self::checked_array_index(&index_str, *len),
                            _ => index_str,
                        };
                        path = format!("(({}*)&{})[{}]", elem_c_ty, path, index_str);
                    }
                    current_ty = elem_ty;
                }
                AmirProjection::IndexConstant(index) => {
                    if current_ty.is_borrowed_slice_abi(self.interner)
                        && let ArType::Ref(inner) | ArType::RefMut(inner) = &current_ty
                    {
                        current_ty = self.interner.resolve(*inner);
                    }
                    let pointee_array = match &current_ty {
                        ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                            match self.interner.resolve(*inner) {
                                ArType::Array(len, elem) => Some((len, elem)),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    let is_vec = arandu_middle::types::is_vec_type(&current_ty, self.symbols);
                    let elem_ty = if let Some((_, elem_id)) = pointee_array {
                        self.interner.resolve(elem_id)
                    } else {
                        match arandu_middle::types::index_elem_type(
                            &current_ty,
                            self.symbols,
                            self.interner,
                        ) {
                            Some(id) => self.interner.resolve(id),
                            None => ArType::Error,
                        }
                    };
                    let elem_c_ty = self.format_type(&elem_ty);
                    if let Some((len, elem_id)) = pointee_array
                        && len > 0
                        && self.checked_layout(&ArType::Array(len, elem_id)).size > 0
                    {
                        path = format!("({})->data[{}]", path, index);
                    } else if matches!(&current_ty, ArType::Array(len, _) if *len > 0)
                        && self.checked_layout(&current_ty).size > 0
                    {
                        path = format!("({}).data[{}]", path, index);
                    } else if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                    ) {
                        path = format!("(({}*){})[{}]", elem_c_ty, path, index);
                    } else if matches!(current_ty, ArType::Slice(_) | ArType::MutSlice(_)) || is_vec
                    {
                        path = format!(
                            "(( {}* )(*(void**)((uint8_t*)&{} + 0)))[{}]",
                            elem_c_ty, path, index
                        );
                    } else {
                        path = format!("(({}*)&{})[{}]", elem_c_ty, path, index);
                    }
                    current_ty = elem_ty;
                }
            }
            if !matches!(proj, AmirProjection::Deref | AmirProjection::Field(_)) {
                address = format!("&({path})");
            }
        }
        path
    }
}
