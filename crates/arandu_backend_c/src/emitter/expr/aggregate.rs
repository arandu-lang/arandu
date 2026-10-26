use super::super::CEmitter;
use arandu_middle::SymbolId;
use arandu_middle::amir::{AmirFunc, AmirOperand};
use arandu_middle::types::{ArType, TypeId};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_field_access(
        &mut self,
        base: &AmirOperand,
        field: &usize,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
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
        let Some(offset) = layout.field_offsets.get(*field).copied() else {
            self.record_codegen_ice(
                func,
                format!("FieldAccess index {field} is outside the layout for {struct_ty:?}"),
            );
            let _ = write!(&mut self.output, "/* invalid FieldAccess index */ 0");
            return;
        };

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

    pub(super) fn emit_discriminant(&mut self, value: &AmirOperand, func: &AmirFunc) {
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
        let (enum_ty, address) = match base_ty {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                (self.interner.resolve(inner), format!("t{base_temp}"))
            }
            other => (other, format!("&t{base_temp}")),
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
                "({{ uintptr_t _raw = 0; memcpy(&_raw, (uint8_t*){} + {}, sizeof(_raw)); (int64_t)(_raw & 0x{:x}ULL); }})",
                address, pointer_offset, tag_mask
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
                "({{ uintptr_t _niche = 0; memcpy(&_niche, (uint8_t*){} + {}, sizeof(_niche)); (_niche == {}) ? (int64_t){} : (int64_t){}; }})",
                address, niche_offset, niche_value, tagged_variant, untagged_variant
            );
        } else {
            let _ = write!(
                &mut self.output,
                "({{ int64_t _tag = 0; memcpy(&_tag, (uint8_t*){} + 0, sizeof(_tag)); _tag; }})",
                address
            );
        }
    }

    pub(super) fn emit_enum_payload(
        &mut self,
        value: &AmirOperand,
        index: &usize,
        tuple_ty: &Option<TypeId>,
        func: &AmirFunc,
        expected_c_type: &str,
    ) {
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
        let (enum_ty, address) = match base_ty {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                (self.interner.resolve(inner), format!("t{base_temp}"))
            }
            other => (other, format!("&t{base_temp}")),
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
                "({{ uintptr_t _raw = 0; memcpy(&_raw, (uint8_t*){} + {}, sizeof(_raw)); ({expected_c_type})(_raw & ~0x{:x}ULL); }})",
                address, pointer_offset, tag_mask
            );
            return;
        }
        let base_offset = layout.field_offsets.get(1).copied().unwrap_or(0);
        let tuple_offset = if let Some(tuple) = tuple_ty {
            let tuple = self.interner.resolve(*tuple);
            let tuple_layout = self.checked_layout(&tuple);
            let Some(offset) = tuple_layout.field_offsets.get(*index).copied() else {
                self.record_codegen_ice(
                    func,
                    "EnumPayload field is outside its instantiated tuple layout",
                );
                return;
            };
            offset
        } else {
            0
        };
        let Some(payload_offset) = base_offset.checked_add(tuple_offset) else {
            self.record_codegen_ice(func, "EnumPayload offset overflow");
            return;
        };
        let _ = write!(
            &mut self.output,
            "({{ {expected_c_type} _payload = {{0}}; memcpy(&_payload, (uint8_t*){} + {}, sizeof(_payload)); _payload; }})",
            address, payload_offset
        );
    }

    pub(super) fn emit_enum_construct(
        &mut self,
        variant_tag: &usize,
        payload: &Option<AmirOperand>,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
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
        } else if let Some(arandu_middle::layout::TagEncoding::Niche { tagged_variant, .. }) =
            enum_layout.tag_encoding
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
                ArType::Named(_, _) => self.operand_ty(func, p),
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
            if matches!(payload_ty, ArType::Void) || self.checked_layout(&payload_ty).size == 0 {
                let _ = write!(
                    &mut self.output,
                    "({{ (void)({payload_str}); {expected_c_type} _res = {{0}}; {tag_c} _tag = {}; memcpy(&_res, &_tag, sizeof(_tag) < sizeof(_res) ? sizeof(_tag) : sizeof(_res)); _res; }})",
                    variant_tag
                );
            } else {
                let _ = write!(
                    &mut self.output,
                    "({{ {expected_c_type} _res = {{0}}; struct {{ {tag_c} tag; {payload_c_ty} payload; }} _s = {{ {}, {} }}; memcpy(&_res, &_s, sizeof(_s) < sizeof(_res) ? sizeof(_s) : sizeof(_res)); _res; }})",
                    variant_tag, payload_str
                );
            }
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

    pub(super) fn emit_struct_literal(
        &mut self,
        struct_symbol: &SymbolId,
        fields: &Vec<(arandu_middle::SmolStr, AmirOperand)>,
        func: &AmirFunc,
        expected_ar_type: &ArType,
        expected_c_type: &str,
    ) {
        let struct_ty = match expected_ar_type {
            ArType::Named(id, _) if id == struct_symbol => expected_ar_type.clone(),
            _ => arandu_middle::types::ArType::named(*struct_symbol, &[], self.interner),
        };
        let layout = self.checked_layout(&struct_ty);
        let field_defs = self
            .provider
            .get_struct_fields_for_type(&struct_ty, self.interner);
        let mut resolved_fields = Vec::new();
        for (name, op) in fields {
            let Some(field_idx) = field_defs
                .and_then(|fields| fields.get(name.as_str()))
                .map(|field| field.index)
            else {
                self.record_codegen_ice(
                    func,
                    format!("StructLiteral references unknown field `{name}`"),
                );
                continue;
            };
            let Some(offset) = layout.field_offsets.get(field_idx).copied() else {
                self.record_codegen_ice(
                    func,
                    format!("StructLiteral field `{name}` index {field_idx} is outside the layout"),
                );
                continue;
            };
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
}
