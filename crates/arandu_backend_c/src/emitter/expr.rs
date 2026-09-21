use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue};
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{ArType, Primitive};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_rvalue(
        &mut self,
        rvalue: &AmirRvalue,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        match rvalue {
            AmirRvalue::Use(op) => {
                let op_str = self.format_operand(op, func);
                let source_ty = match op {
                    AmirOperand::Copy(temp) | AmirOperand::Move(temp) => self.temp_ty(func, *temp),
                    _ => ArType::Error,
                };
                let source_is_pointer = matches!(
                    &source_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                ) && !source_ty.is_borrowed_slice_abi(self.interner);
                let dest_is_pointer = matches!(
                    expected_ar_type,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                ) && !expected_ar_type.is_borrowed_slice_abi(self.interner);
                if dest_is_pointer {
                    if source_is_pointer {
                        let _ = write!(&mut self.output, "({expected_c_type})({op_str})");
                    } else if matches!(source_ty, ArType::Primitive(_) | ArType::IntLiteral) {
                        let _ =
                            write!(&mut self.output, "({expected_c_type})(uintptr_t)({op_str})");
                    } else {
                        let _ = write!(&mut self.output, "*({expected_c_type}*)&({op_str})");
                    }
                } else if source_is_pointer
                    && matches!(expected_ar_type, ArType::Primitive(_) | ArType::IntLiteral)
                {
                    let _ = write!(&mut self.output, "({expected_c_type})(uintptr_t)({op_str})");
                } else {
                    let _ = write!(&mut self.output, "{op_str}");
                }
            }
            AmirRvalue::BlackBox { value, .. } => {
                let operand = self.format_operand(value, func);
                match expected_ar_type {
                    ArType::Primitive(Primitive::Str) => {
                        let _ = write!(
                            &mut self.output,
                            "({{ volatile {expected_c_type} opaque = ({operand}); opaque; }})"
                        );
                    }
                    ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => {
                        let _ = write!(
                            &mut self.output,
                            "ar_bench_black_box_f64((double)({operand}))"
                        );
                    }
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) => {
                        let _ = write!(
                            &mut self.output,
                            "({expected_c_type})ar_bench_black_box_ptr((void*)({operand}))"
                        );
                    }
                    ArType::Primitive(_) | ArType::IntLiteral => {
                        let _ = write!(
                            &mut self.output,
                            "({expected_c_type})ar_bench_black_box_i64((int64_t)({operand}))"
                        );
                    }
                    _ => {
                        let _ = write!(
                            &mut self.output,
                            "({{ volatile {expected_c_type} opaque = ({operand}); opaque; }})"
                        );
                    }
                }
            }
            AmirRvalue::Binary { op, left, right } => {
                if matches!(op, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive) {
                    let left_str = self.format_operand(left, func);
                    let right_str = self.format_operand(right, func);
                    let _ = write!(
                        &mut self.output,
                        "ar_make_range((intptr_t)({}), (intptr_t)({}))",
                        left_str, right_str
                    );
                } else {
                    let left_str = self.format_operand(left, func);
                    let right_str = self.format_operand(right, func);
                    let left_ty = match left {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
                        _ => ArType::Error,
                    };
                    if matches!(left_ty, ArType::Primitive(Primitive::Str)) {
                        if matches!(op, BinaryOp::Equal) {
                            let _ =
                                write!(&mut self.output, "ar_str_eq({}, {})", left_str, right_str);
                            return;
                        } else if matches!(op, BinaryOp::NotEqual) {
                            let _ =
                                write!(&mut self.output, "!ar_str_eq({}, {})", left_str, right_str);
                            return;
                        }
                    }
                    let op_str = match op {
                        BinaryOp::Add => "+",
                        BinaryOp::Sub => "-",
                        BinaryOp::Mul => "*",
                        BinaryOp::Div => "/",
                        BinaryOp::Mod => "%",
                        BinaryOp::Equal => "==",
                        BinaryOp::NotEqual => "!=",
                        BinaryOp::Lt => "<",
                        BinaryOp::LtEqual => "<=",
                        BinaryOp::Gt => ">",
                        BinaryOp::GtEqual => ">=",
                        BinaryOp::And => "&&",
                        BinaryOp::Or => "||",
                        BinaryOp::BitAnd => "&",
                        BinaryOp::BitOr => "|",
                        BinaryOp::BitXor => "^",
                        BinaryOp::ShiftLeft => "<<",
                        BinaryOp::ShiftRight => ">>",
                        BinaryOp::NullCoalesce => {
                            self.record_codegen_ice(
                                func,
                                "NullCoalesce reached C codegen before CFG lowering",
                            );
                            "/* invalid NullCoalesce */"
                        }
                        BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => unreachable!(),
                        _ => unreachable!(),
                    };
                    let _ = write!(&mut self.output, "{} {} {}", left_str, op_str, right_str);
                }
            }
            AmirRvalue::FieldAccess { base, field } => {
                let expected_layout = self.checked_layout(expected_ar_type);
                if expected_layout.size == 0 {
                    let _ = write!(&mut self.output, "({expected_c_type}){{0}}");
                    return;
                }
                let base_ty = match base {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
                    _ => {
                        self.record_codegen_ice(
                            func,
                            "FieldAccess reached C codegen with a non-temporary base",
                        );
                        let _ = write!(&mut self.output, "/* invalid FieldAccess base */ 0");
                        return;
                    }
                };
                let base_is_pointer =
                    matches!(base_ty, ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_));
                let struct_ty = match base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        self.interner.resolve(inner)
                    }
                    other => other,
                };
                let layout = self.checked_layout(&struct_ty);
                let offset = layout.field_offsets.get(*field).copied().unwrap_or(0);

                let base_temp = match base {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => t.as_usize(),
                    _ => 0,
                };
                let address = if base_is_pointer {
                    format!("t{base_temp}")
                } else {
                    format!("&t{base_temp}")
                };
                let _ = write!(
                    &mut self.output,
                    "*({}*)((uint8_t*){} + {})",
                    expected_c_type, address, offset
                );
            }
            AmirRvalue::Discriminant { value } => {
                let base_temp = match value {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => t.as_usize(),
                    _ => {
                        self.record_codegen_ice(
                            func,
                            "Discriminant reached C codegen with a non-temporary base",
                        );
                        let _ = write!(&mut self.output, "/* invalid Discriminant base */ 0");
                        return;
                    }
                };
                let base_ty = self.interner.resolve(func.temps[base_temp].ty);
                let enum_ty = match base_ty {
                    ArType::Ptr(inner) => self.interner.resolve(inner),
                    other => other,
                };
                let layout = self.checked_layout(&enum_ty);
                if let Some(arandu_middle::layout::TagEncoding::PointerTag {
                    tag_mask,
                    pointer_offset,
                    ..
                }) = layout.tag_encoding
                {
                    let _ = write!(
                        &mut self.output,
                        "({{ uintptr_t _raw = 0; memcpy(&_raw, (uint8_t*)&t{} + {}, sizeof(_raw)); (int64_t)(_raw & 0x{:x}ULL); }})",
                        base_temp, pointer_offset, tag_mask
                    );
                } else if let Some(arandu_middle::layout::TagEncoding::Niche {
                    niche_offset,
                    niche_value,
                    untagged_variant,
                    tagged_variant,
                    ..
                }) = layout.tag_encoding
                {
                    let _ = write!(
                        &mut self.output,
                        "({{ uintptr_t _niche = 0; memcpy(&_niche, (uint8_t*)&t{} + {}, sizeof(_niche)); (_niche == {}) ? (int64_t){} : (int64_t){}; }})",
                        base_temp, niche_offset, niche_value, tagged_variant, untagged_variant
                    );
                } else {
                    let _ = write!(
                        &mut self.output,
                        "({{ int64_t _tag = 0; memcpy(&_tag, (uint8_t*)&t{} + 0, sizeof(_tag)); _tag; }})",
                        base_temp
                    );
                }
            }
            AmirRvalue::EnumPayload {
                value,
                variant: _,
                index: _,
            } => {
                let base_temp = match value {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => t.as_usize(),
                    _ => {
                        self.record_codegen_ice(
                            func,
                            "EnumPayload reached C codegen with a non-temporary base",
                        );
                        let _ = write!(&mut self.output, "/* invalid EnumPayload base */ 0");
                        return;
                    }
                };

                let base_ty = self.interner.resolve(func.temps[base_temp].ty);
                let enum_ty = match base_ty {
                    ArType::Ptr(inner) => self.interner.resolve(inner),
                    other => other,
                };
                let layout = self.checked_layout(&enum_ty);
                if let Some(arandu_middle::layout::TagEncoding::PointerTag {
                    tag_mask,
                    pointer_offset,
                    ..
                }) = layout.tag_encoding
                {
                    let _ = write!(
                        &mut self.output,
                        "({{ uintptr_t _raw = 0; memcpy(&_raw, (uint8_t*)&t{} + {}, sizeof(_raw)); ({expected_c_type})(_raw & ~0x{:x}ULL); }})",
                        base_temp, pointer_offset, tag_mask
                    );
                    return;
                }
                let enum_id = match enum_ty {
                    ArType::Named(id, _) => id,
                    _ => arandu_middle::SymbolId::DUMMY,
                };

                let mut payload_offset = 0;
                if matches!(
                    layout.tag_encoding,
                    Some(arandu_middle::layout::TagEncoding::Niche { .. })
                ) {
                    payload_offset = 0;
                } else if arandu_middle::layout::StructLayoutProvider::get_enum_variants(
                    self.provider,
                    enum_id,
                )
                .is_some()
                    || matches!(
                        enum_ty,
                        ArType::Option(_) | ArType::Result(_, _) | ArType::Poll(_)
                    )
                {
                    // Tag is pointer-width on the target layout (i686 → 4, host64 → 8).
                    let tag_size = self.layout.pointer_width() as usize;
                    payload_offset = tag_size;
                }
                let _ = write!(
                    &mut self.output,
                    "({{ {expected_c_type} _payload = {{0}}; memcpy(&_payload, (uint8_t*)&t{} + {}, sizeof(_payload)); _payload; }})",
                    base_temp, payload_offset
                );
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                let enum_layout = self.checked_layout(expected_ar_type);
                if let Some(arandu_middle::layout::TagEncoding::PointerTag {
                    tag_mask,
                    pointer_offset,
                    ..
                }) = enum_layout.tag_encoding
                {
                    let tag_val = (*variant_tag as u64) & tag_mask;
                    if let Some(p) = payload {
                        let payload_str = self.format_operand(p, func);
                        let _ = write!(
                            &mut self.output,
                            "({{ {expected_c_type} _res = {{0}}; uintptr_t _ptr = (uintptr_t)({payload_str}); uintptr_t _tagged = (_ptr & ~0x{:x}ULL) | 0x{:x}ULL; memcpy((uint8_t*)&_res + {}, &_tagged, sizeof(_tagged)); _res; }})",
                            tag_mask, tag_val, pointer_offset
                        );
                    } else {
                        let _ = write!(
                            &mut self.output,
                            "({{ {expected_c_type} _res = {{0}}; uintptr_t _tagged = 0x{:x}ULL; memcpy((uint8_t*)&_res + {}, &_tagged, sizeof(_tagged)); _res; }})",
                            tag_val, pointer_offset
                        );
                    }
                } else if let Some(arandu_middle::layout::TagEncoding::Niche {
                    tagged_variant,
                    ..
                }) = enum_layout.tag_encoding
                {
                    if *variant_tag == tagged_variant {
                        let _ = write!(
                            &mut self.output,
                            "({{ {expected_c_type} _res = {{0}}; _res; }})"
                        );
                    } else if let Some(p) = payload {
                        let payload_str = self.format_operand(p, func);
                        let p_ty = match p {
                            AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
                            _ => match expected_ar_type {
                                ArType::Option(inner) => self.interner.resolve(*inner),
                                _ => ArType::Error,
                            },
                        };
                        let p_c_ty = self.format_type(&p_ty);
                        let _ = write!(
                            &mut self.output,
                            "({{ {expected_c_type} _res = {{0}}; {p_c_ty} _p = {}; memcpy(&_res, &_p, sizeof(_p) < sizeof(_res) ? sizeof(_p) : sizeof(_res)); _res; }})",
                            payload_str
                        );
                    } else {
                        let _ = write!(
                            &mut self.output,
                            "({{ {expected_c_type} _res = {{0}}; _res; }})"
                        );
                    }
                } else if let Some(p) = payload {
                    let payload_str = self.format_operand(p, func);
                    let payload_ty = match expected_ar_type {
                        ArType::Named(id, _) => self
                            .provider
                            .get_enum_variants(*id)
                            .and_then(|variants| {
                                variants.get(*variant_tag).and_then(|v| v.payload_ty)
                            })
                            .map(|ty_id| self.interner.resolve(ty_id))
                            .unwrap_or(ArType::Error),
                        ArType::Option(inner) => {
                            if *variant_tag == 1 {
                                self.interner.resolve(*inner)
                            } else {
                                ArType::Error
                            }
                        }
                        ArType::Result(ok, err) => {
                            if *variant_tag == 0 {
                                self.interner.resolve(*ok)
                            } else {
                                self.interner.resolve(*err)
                            }
                        }
                        _ => ArType::Error,
                    };
                    let payload_c_ty = self.format_type(&payload_ty);
                    // Tag width follows target pointer width (same as payload projection).
                    let tag_c = if self.layout.pointer_width() == 4 {
                        "int32_t"
                    } else {
                        "int64_t"
                    };
                    let _ = write!(
                        &mut self.output,
                        "({{ {expected_c_type} _res = {{0}}; struct {{ {tag_c} tag; {payload_c_ty} payload; }} _s = {{ {}, {} }}; memcpy(&_res, &_s, sizeof(_s) < sizeof(_res) ? sizeof(_s) : sizeof(_res)); _res; }})",
                        variant_tag, payload_str
                    );
                } else {
                    let tag_c = if self.layout.pointer_width() == 4 {
                        "int32_t"
                    } else {
                        "int64_t"
                    };
                    let _ = write!(
                        &mut self.output,
                        "({{ {expected_c_type} _res = {{0}}; {tag_c} _tag = {}; memcpy(&_res, &_tag, sizeof(_tag) < sizeof(_res) ? sizeof(_tag) : sizeof(_res)); _res; }})",
                        variant_tag
                    );
                }
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let struct_ty = match expected_ar_type {
                    ArType::Named(id, _) if id == struct_symbol => expected_ar_type.clone(),
                    _ => arandu_middle::types::ArType::named(*struct_symbol, &[], self.interner),
                };
                let layout = self.checked_layout(&struct_ty);
                let field_defs = self.provider.get_struct_fields(*struct_symbol);
                let mut resolved_fields = Vec::new();
                for (i, (name, op)) in fields.iter().enumerate() {
                    let field_idx = self
                        .provider
                        .get_struct_fields(*struct_symbol)
                        .and_then(|m| m.get(name.as_str()))
                        .map(|f| f.index)
                        .unwrap_or(i);
                    let offset = layout.field_offsets.get(field_idx).copied().unwrap_or(0);
                    let field_ty = if field_defs.is_some() {
                        self.instantiated_field_ty(&struct_ty, name)
                    } else {
                        ArType::Error
                    };
                    let field_layout = self.checked_layout(&field_ty);
                    if field_layout.size == 0 {
                        continue;
                    }
                    let field_c_ty = self.format_type(&field_ty);
                    let op_str = self.format_operand(op, func);
                    resolved_fields.push((offset, field_c_ty, op_str));
                }
                resolved_fields.sort_by_key(|f| f.0);

                let _ = write!(&mut self.output, "({{ {expected_c_type} _res = {{0}};");
                for (offset, field_c_ty, op_str) in &resolved_fields {
                    let _ = write!(
                        &mut self.output,
                        " *({field_c_ty}*)((uint8_t*)&_res + {offset}) = {op_str};"
                    );
                }
                let _ = write!(&mut self.output, " _res; }})");
            }
            AmirRvalue::Unary { op, operand } => {
                let op_val = self.format_operand(operand, func);
                match op {
                    UnaryOp::Neg => {
                        let _ = write!(&mut self.output, "-{}", op_val);
                    }
                    UnaryOp::Not => {
                        let _ = write!(&mut self.output, "!{}", op_val);
                    }
                    UnaryOp::BitNot => {
                        let _ = write!(&mut self.output, "~{}", op_val);
                    }
                    // A3.6: await = poll until Ready; disc@0, payload@8.
                    // Load/store the **expected payload C type** (not i64 cast). Casting
                    // block_on_i64 → float/ptr was the root of nonsensical C for non-int.
                    UnaryOp::Await => {
                        let is_float = matches!(
                            expected_ar_type,
                            ArType::Primitive(Primitive::Float) | ArType::FloatLiteral
                        );
                        let is_ptr = matches!(
                            expected_ar_type,
                            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                        );
                        let helper = if is_float {
                            "ar_co_await_f64"
                        } else if is_ptr {
                            "ar_co_await_ptr"
                        } else {
                            "ar_co_await_i64"
                        };
                        let _ = write!(
                            &mut self.output,
                            "({expected_c_type}){helper}((uint8_t*)({op_val}))"
                        );
                    }
                    UnaryOp::Deref => {
                        let operand_ty = match operand {
                            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                                self.temp_ty(func, *temp)
                            }
                            _ => ArType::Error,
                        };
                        if operand_ty.is_borrowed_slice_abi(self.interner) {
                            let _ = write!(&mut self.output, "{}", op_val);
                        } else {
                            let _ = write!(&mut self.output, "*{}", op_val);
                        }
                    }
                    UnaryOp::Ref | UnaryOp::RefMut => {
                        self.record_codegen_ice(
                            func,
                            "Unary Ref/RefMut reached C codegen before Borrow lowering",
                        );
                        let _ = write!(&mut self.output, "/* invalid reference rvalue */ 0");
                    }
                    _ => {
                        let _ = write!(&mut self.output, "{}", op_val);
                    }
                }
            }
            AmirRvalue::Load(place) => {
                let place_str = self.format_place(place, func);
                let _ = write!(&mut self.output, "{}", place_str);
            }
            AmirRvalue::Borrow(place) => {
                let place_str = self.format_place(place, func);
                if expected_ar_type.is_borrowed_slice_abi(self.interner) {
                    let _ = write!(&mut self.output, "{}", place_str);
                } else {
                    let _ = write!(&mut self.output, "&{}", place_str);
                }
            }
            AmirRvalue::BorrowMut(place) => {
                let place_str = self.format_place(place, func);
                if expected_ar_type.is_borrowed_slice_abi(self.interner) {
                    let _ = write!(&mut self.output, "{}", place_str);
                } else {
                    let _ = write!(&mut self.output, "&{}", place_str);
                }
            }
            AmirRvalue::Array { items } => {
                if items.is_empty() {
                    let _ = write!(
                        &mut self.output,
                        "({{ {expected_c_type} _res = {{0}}; _res; }})"
                    );
                } else {
                    let elem_ty = match expected_ar_type {
                        ArType::Array(_, inner) => self.interner.resolve(*inner),
                        _ => ArType::Error,
                    };
                    let elem_c_ty = self.format_type(&elem_ty);
                    let _ = write!(
                        &mut self.output,
                        "({{ {expected_c_type} _res = {{0}}; {elem_c_ty} _arr[] = {{"
                    );
                    for (i, op) in items.iter().enumerate() {
                        if i > 0 {
                            let _ = write!(&mut self.output, ", ");
                        }
                        let op_str = self.format_operand(op, func);
                        let _ = write!(&mut self.output, "{}", op_str);
                    }
                    let _ = write!(
                        &mut self.output,
                        "}}; memcpy(&_res, _arr, sizeof(_arr) < sizeof(_res) ? sizeof(_arr) : sizeof(_res)); _res; }})"
                    );
                }
            }
            AmirRvalue::Tuple { items } => {
                if items.is_empty() {
                    let _ = write!(
                        &mut self.output,
                        "({{ {expected_c_type} _res = {{0}}; _res; }})"
                    );
                } else {
                    let tys = match expected_ar_type {
                        ArType::Tuple(tys) => self.interner.type_args(*tys),
                        _ => Vec::new(),
                    };
                    let _ = write!(
                        &mut self.output,
                        "({{ {expected_c_type} _res = {{0}}; struct {{"
                    );
                    for (i, _) in items.iter().enumerate() {
                        let field_ty = if i < tys.len() {
                            self.interner.resolve(tys[i])
                        } else {
                            ArType::Error
                        };
                        let field_c_ty = self.format_type(&field_ty);
                        let _ = write!(&mut self.output, " {} f_{};", field_c_ty, i);
                    }
                    let _ = write!(&mut self.output, "}} _s = {{");
                    for (i, op) in items.iter().enumerate() {
                        if i > 0 {
                            let _ = write!(&mut self.output, ", ");
                        }
                        let op_str = self.format_operand(op, func);
                        let _ = write!(&mut self.output, "{}", op_str);
                    }
                    let _ = write!(
                        &mut self.output,
                        "}}; memcpy(&_res, &_s, sizeof(_s) < sizeof(_res) ? sizeof(_s) : sizeof(_res)); _res; }})"
                    );
                }
            }
            AmirRvalue::Len(op) => {
                let op_str = self.format_operand(op, func);
                let op_ty = match op {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
                    _ => ArType::Error,
                };
                if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                    // LayoutEngine Str fat pointer: second field is len.
                    let _ = write!(&mut self.output, "({}).len", op_str);
                } else if op_ty.slice_abi_element(self.interner).is_some() {
                    // Slice fat pointer: len offset from LayoutEngine (not magic +8).
                    let off = self.layout.fat_ptr_len_offset();
                    let len_ty = if self.layout.fat_ptr_len_size() == 4 {
                        "int32_t"
                    } else {
                        "int64_t"
                    };
                    let _ = write!(
                        &mut self.output,
                        "({{ {len_ty} _len = 0; memcpy(&_len, (uint8_t*)&{op_str} + {off}, sizeof(_len)); _len; }})"
                    );
                } else if let ArType::Array(len, _) = op_ty {
                    let _ = write!(&mut self.output, "{}", len);
                } else {
                    self.record_codegen_ice(
                        func,
                        format!("Len reached C codegen for unsupported type {op_ty:?}"),
                    );
                    let _ = write!(&mut self.output, "/* invalid Len operand */ 0");
                }
            }
            AmirRvalue::SliceView { data, len, .. } => {
                let data = self.format_operand(data, func);
                let len = self.format_operand(len, func);
                let off = self.layout.fat_ptr_len_offset();
                let len_ty = if self.layout.fat_ptr_len_size() == 4 {
                    "int32_t"
                } else {
                    "int64_t"
                };
                let _ = write!(
                    &mut self.output,
                    "({{ {expected_c_type} view = {{0}}; void* _ptr = (void*)({data}); {len_ty} _len = ({len_ty})({len}); memcpy((uint8_t*)&view + 0, &_ptr, sizeof(_ptr)); memcpy((uint8_t*)&view + {off}, &_len, sizeof(_len)); view; }})"
                );
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let slice = self.format_operand(slice, func);
                let start = self.format_operand(start, func);
                let len = self.format_operand(len, func);
                let off = self.layout.fat_ptr_len_offset();
                let len_ty = if self.layout.fat_ptr_len_size() == 4 {
                    "int32_t"
                } else {
                    "int64_t"
                };
                let elem_c = match expected_ar_type {
                    ArType::Slice(inner) => self.format_type(&self.interner.resolve(*inner)),
                    _ => std::borrow::Cow::Borrowed("uint8_t"),
                };
                let _ = write!(
                    &mut self.output,
                    "({{ {expected_c_type} view = {{0}}; void* _orig_ptr = 0; memcpy(&_orig_ptr, (uint8_t*)&{slice} + 0, sizeof(_orig_ptr)); void* _ptr = (void*)((( {elem_c}*)_orig_ptr) + ({start})); {len_ty} _len = ({len_ty})({len}); memcpy((uint8_t*)&view + 0, &_ptr, sizeof(_ptr)); memcpy((uint8_t*)&view + {off}, &_len, sizeof(_len)); view; }})"
                );
            }
            AmirRvalue::SliceData(slice) => {
                let slice = self.format_operand(slice, func);
                let _ = write!(
                    &mut self.output,
                    "({{ void* _ptr = 0; memcpy(&_ptr, (uint8_t*)&{slice} + 0, sizeof(_ptr)); ({expected_c_type})_ptr; }})"
                );
            }
            AmirRvalue::StrBytes { source } => {
                let source = self.format_operand(source, func);
                let off = self.layout.fat_ptr_len_offset();
                let len_ty = if self.layout.fat_ptr_len_size() == 4 {
                    "int32_t"
                } else {
                    "int64_t"
                };
                let _ = write!(
                    &mut self.output,
                    "({{ {expected_c_type} view = {{0}}; void* _ptr = (void*)({source}).ptr; {len_ty} _len = ({len_ty})({source}).len; memcpy((uint8_t*)&view + 0, &_ptr, sizeof(_ptr)); memcpy((uint8_t*)&view + {off}, &_len, sizeof(_len)); view; }})"
                );
            }
            AmirRvalue::StrView { owner } => {
                let owner = self.format_operand(owner, func);
                let _ = write!(&mut self.output, "({expected_c_type})({owner})");
            }
            AmirRvalue::IndexAccess { base, index } => {
                let base_ty = match base {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => self.temp_ty(func, *t),
                    _ => ArType::Error,
                };
                let deref_ty = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        self.interner.resolve(*inner)
                    }
                    other => other.clone(),
                };
                let is_vec = arandu_middle::types::is_vec_type(&deref_ty, self.symbols);
                let elem_ty = match arandu_middle::types::index_elem_type(
                    &deref_ty,
                    self.symbols,
                    self.interner,
                ) {
                    Some(id) => self.interner.resolve(id),
                    None => ArType::Error,
                };
                let elem_c_ty = self.format_type(&elem_ty);
                let base_str = self.format_operand(base, func);
                let index_str = self.format_operand(index, func);

                if matches!(base_ty, ArType::Ptr(_)) {
                    let _ = write!(
                        &mut self.output,
                        "(({}*){})[{}]",
                        elem_c_ty, base_str, index_str
                    );
                } else if base_ty.slice_abi_element(self.interner).is_some() || is_vec {
                    let _ = write!(
                        &mut self.output,
                        "(({}*)(*(void**)((uint8_t*)&{} + 0)))[{}]",
                        elem_c_ty, base_str, index_str
                    );
                } else {
                    let _ = write!(
                        &mut self.output,
                        "(({}*)&{})[{}]",
                        elem_c_ty, base_str, index_str
                    );
                }
            }
            AmirRvalue::Alloc(op) => {
                // Byte-count allocation via libc malloc (RC-RVALUE-GAPS).
                let size_str = self.format_operand(op, func);
                let _ = write!(&mut self.output, "malloc((size_t)({}))", size_str);
            }
            // Heap CoroutineReady (stack:true is multi-stmt in emit_stmt).
            // A3.6 layout: disc@0 (u32 Ready=0), payload@8.
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack: false,
            } => {
                let payload_ar = self.interner.resolve(*payload_ty);
                let payload_c = self.format_type(&payload_ar);
                let v = self.format_operand(value, func);
                let payload_size = self.checked_layout(&payload_ar).size.max(1);
                let size = 8 + payload_size;
                let _ = write!(
                    &mut self.output,
                    "ar_co_make_ready_heap({size}, &({payload_c}){{{v}}}, sizeof({payload_c}))"
                );
            }
            AmirRvalue::CoroutineReady { stack: true, .. } => {
                // Should have been handled as multi-stmt Assign; fallback null.
                let _ = write!(&mut self.output, "((void*)0)");
            }
            // A3.4: pin-free index (LocalId), not a raw address.
            AmirRvalue::RelativeBorrow { local, .. } => {
                let _ = write!(&mut self.output, "((void*)(uintptr_t){})", local.as_usize());
            }
            AmirRvalue::GenInsert { .. }
            | AmirRvalue::GenGet { .. }
            | AmirRvalue::GenSet { .. }
            | AmirRvalue::GenUpsert { .. }
            | AmirRvalue::GenRemove { .. } => {
                unreachable!("GenRef operations must be lowered as statements via ar_gen_*_raw");
            }
            AmirRvalue::StringInterp { parts } => {
                // Emit a call to the runtime helper: ar_str_concat_n(n, part0, part1, ..., partN-1)
                // Each part must already be of type ArStr.
                let n = parts.len();
                let part_strs: Vec<String> =
                    parts.iter().map(|p| self.format_operand(p, func)).collect();
                let _ = write!(
                    &mut self.output,
                    "ar_str_concat_n({}, {})",
                    n,
                    part_strs.join(", ")
                );
            }
            AmirRvalue::ToStr { value, src_ty } => {
                use arandu_middle::types::{ArType, Primitive};
                let src = self.interner.resolve(*src_ty);
                let val = self.format_operand(value, func);
                match &src {
                    ArType::Primitive(Primitive::Str) => {
                        let _ = write!(&mut self.output, "{val}");
                    }
                    ArType::Primitive(Primitive::Bool) => {
                        let _ = write!(&mut self.output, "ar_bool_to_str({val})");
                    }
                    ArType::Primitive(Primitive::Char) => {
                        let _ = write!(&mut self.output, "ar_char_to_str((uint32_t)({val}))");
                    }
                    ArType::FloatLiteral => {
                        let _ = write!(&mut self.output, "ar_f64_to_str((double)({val}))");
                    }
                    ArType::Primitive(p) if p.is_float() => {
                        let _ = write!(&mut self.output, "ar_f64_to_str((double)({val}))");
                    }
                    ArType::IntLiteral => {
                        let _ = write!(&mut self.output, "ar_i64_to_str((int64_t)({val}))");
                    }
                    ArType::Primitive(p) if p.is_integer() && p.is_signed() => {
                        let _ = write!(&mut self.output, "ar_i64_to_str((int64_t)({val}))");
                    }
                    ArType::Primitive(p) if p.is_integer() => {
                        let _ = write!(&mut self.output, "ar_u64_to_str((uint64_t)({val}))");
                    }
                    other => {
                        self.record_codegen_ice(
                            func,
                            format!("ToStr reached C codegen for unsupported type {other:?}"),
                        );
                        let _ = write!(
                            &mut self.output,
                            "/* invalid ToStr source */ ar_str_pack((const uint8_t*)\"\", 0)"
                        );
                    }
                }
            }
        }
    }
}
