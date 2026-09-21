use std::borrow::Cow;

use super::{CEmitter, sanitize_c_ident};
use arandu_middle::amir::{AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProjection};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};

const C_TRUE: &str = "true";
const C_FALSE: &str = "false";
const C_NULL: &str = "0";

impl<'a> CEmitter<'a> {
    #[inline]
    pub(super) fn is_64bit_target(&self) -> bool {
        self.layout.pointer_width() == 8
    }

    pub(super) fn format_operand_str(&self, op: &AmirOperand) -> String {
        match op {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => format!("t{}", t.as_usize()),
            AmirOperand::FunctionRef(id) | AmirOperand::GlobalRef(id) => {
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
            ArType::Primitive(Primitive::Uint) => {
                if self.is_64bit_target() {
                    Cow::Borrowed("uint64_t")
                } else {
                    Cow::Borrowed("uint32_t")
                }
            }
            ArType::IntLiteral => {
                if self.is_64bit_target() {
                    Cow::Borrowed("int64_t")
                } else {
                    Cow::Borrowed("int32_t")
                }
            }
            ArType::Primitive(Primitive::Int) => {
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
            ArType::Named(id, _) => Cow::Owned(sanitize_c_ident(&self.symbols.get(*id).name)),
            ArType::Slice(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                Cow::Owned(format!("ArType_Slice_{}", sanitize_c_ident(&inner_name)))
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
                let inner_name = self.format_type(&self.interner.resolve(*inner));
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

    pub(super) fn format_place(&mut self, place: &AmirPlace, func: &AmirFunc) -> String {
        let local_idx = place.local.as_usize();
        let mut current_ty = self.local_ty(func, place.local);
        let mut path = format!("l{}", local_idx);

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
                    let struct_id = match &struct_ty {
                        ArType::Named(id, _) => *id,
                        _ => arandu_middle::SymbolId::DUMMY,
                    };
                    let layout = self.checked_layout(&struct_ty);
                    let field_name = self
                        .symbols
                        .get(*field_symbol_id)
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap_or("");
                    let field_idx = match self.provider.get_struct_fields(struct_id) {
                        Some(fields) => fields.get(field_name).map(|f| f.index).unwrap_or(0),
                        None => 0,
                    };
                    let offset = layout.field_offsets.get(field_idx).copied().unwrap_or(0);

                    let field_ty = self.instantiated_field_ty(&struct_ty, field_name);
                    let field_c_ty = self.format_type(&field_ty);
                    // If path is already a pointer (heap/ptr local without Deref), GEP from
                    // the pointer value; otherwise take address of the stack lvalue.
                    if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                    ) {
                        path = format!("*({}*)((uint8_t*){} + {})", field_c_ty, path, offset);
                    } else {
                        path = format!("*({}*)((uint8_t*)&{} + {})", field_c_ty, path, offset);
                    }
                    current_ty = field_ty;
                }
                AmirProjection::Index(index_op) => {
                    if current_ty.is_borrowed_slice_abi(self.interner)
                        && let ArType::Ref(inner) | ArType::RefMut(inner) = &current_ty
                    {
                        current_ty = self.interner.resolve(*inner);
                    }
                    let is_vec = arandu_middle::types::is_vec_type(&current_ty, self.symbols);
                    let elem_ty = match arandu_middle::types::index_elem_type(
                        &current_ty,
                        self.symbols,
                        self.interner,
                    ) {
                        Some(id) => self.interner.resolve(id),
                        None => ArType::Error,
                    };
                    let elem_c_ty = self.format_type(&elem_ty);
                    let index_str = self.format_operand(index_op, func);

                    if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                    ) {
                        path = format!("(({}*){})[{}]", elem_c_ty, path, index_str);
                    } else if matches!(current_ty, ArType::Slice(_)) || is_vec {
                        path = format!(
                            "(( {}* )(*(void**)((uint8_t*)&{} + 0)))[{}]",
                            elem_c_ty, path, index_str
                        );
                    } else {
                        path = format!("(({}*)&{})[{}]", elem_c_ty, path, index_str);
                    }
                    current_ty = elem_ty;
                }
            }
        }
        path
    }
}
