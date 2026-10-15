//! Bounded target-layout serialization of literal aggregate initializers.
//! This is an optimization, not CTFE: aliases, relocations and dynamic values
//! fail closed and retain ordinary AMIR evaluation. Padding is always zero.
use arandu_middle::amir::visit::{for_each_stmt_operand, for_each_terminator_operand};
use arandu_middle::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, InstrId, TempId,
};
use arandu_middle::ctfe::{ConstFloat, FloatType};
use arandu_middle::layout::{
    DataLayout, LayoutEngine, StructLayoutProvider, TagEncoding,
    instantiated_enum_variant_payload_type, instantiated_field_type,
};
use arandu_middle::literal_pool::{AmirLiteralEntry, AmirLiteralPool, parse_int_literal};
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
use rustc_hash::FxHashMap;

#[derive(Debug)]
pub struct StaticInitializer {
    pub bytes: Vec<u8>,
    pub alignment: u64,
}

struct Serializer<'a> {
    interner: &'a TypeInterner,
    provider: &'a dyn StructLayoutProvider,
    pool: &'a AmirLiteralPool,
    engine: LayoutEngine,
    little: bool,
    definitions: FxHashMap<TempId, Option<&'a AmirRvalue>>,
    uses: FxHashMap<TempId, usize>,
}

impl Serializer<'_> {
    fn operand(
        &self,
        operand: &AmirOperand,
        ty: TypeId,
        output: &mut [u8],
        depth: usize,
        fuel: &mut usize,
    ) -> Option<()> {
        *fuel = fuel.checked_sub(1)?;
        if depth > 64 {
            return None;
        }
        match operand {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                // Scalar SSA literals cannot alias mutable storage. Aggregate
                // temporaries may: only a single consuming use is admitted.
                if !matches!(self.interner.resolve(ty), ArType::Primitive(_))
                    && self.uses.get(temp) != Some(&1)
                {
                    return None;
                }
                self.rvalue(
                    self.definitions.get(temp).copied().flatten()?,
                    ty,
                    output,
                    depth + 1,
                    fuel,
                )
            }
            AmirOperand::Constant(constant) => self.scalar(constant, ty, output),
            AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => None,
        }
    }

    fn scalar(&self, constant: &AmirConstant, ty: TypeId, output: &mut [u8]) -> Option<()> {
        let ArType::Primitive(primitive) = self.interner.resolve(ty) else {
            return None;
        };
        let value = match constant {
            AmirConstant::Bool(value) if primitive == Primitive::Bool => u128::from(*value),
            AmirConstant::Pool(id) => match self.pool.entries.get(usize::try_from(id.0).ok()?)? {
                AmirLiteralEntry::Int(text)
                    if primitive.is_integer() || primitive == Primitive::Byte =>
                {
                    u128::from_le_bytes(parse_int_literal(text)?.to_le_bytes())
                }
                AmirLiteralEntry::Char(text) if primitive == Primitive::Char => {
                    u128::from(u32::from(text.chars().next()?))
                }
                AmirLiteralEntry::Float(text) if primitive.is_float() => u128::from(
                    ConstFloat::parse(
                        FloatType::new(primitive, self.engine.data_layout).ok()?,
                        text,
                    )
                    .ok()?
                    .bits(),
                ),
                AmirLiteralEntry::FloatBits(value) if primitive.is_float() => {
                    let destination = FloatType::new(primitive, self.engine.data_layout).ok()?;
                    let bits = if destination.bit_width() == value.ty().bit_width() {
                        value.bits()
                    } else {
                        value.cast(destination).ok()?.bits()
                    };
                    u128::from(bits)
                }
                _ => return None,
            },
            _ => return None,
        };
        if !matches!(output.len(), 1 | 2 | 4 | 8) {
            return None;
        }
        let bytes = if self.little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        let source = if self.little {
            &bytes[..output.len()]
        } else {
            &bytes[16 - output.len()..]
        };
        output.copy_from_slice(source);
        Some(())
    }

    fn field(
        &self,
        operand: &AmirOperand,
        ty: TypeId,
        offset: u64,
        output: &mut [u8],
        depth: usize,
        fuel: &mut usize,
    ) -> Option<()> {
        let layout = self
            .engine
            .layout_of_type(&self.interner.resolve(ty), self.interner, self.provider)
            .ok()?;
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(usize::try_from(layout.size).ok()?)?;
        if layout.size == 0 {
            return Some(());
        }
        self.operand(operand, ty, output.get_mut(start..end)?, depth + 1, fuel)
    }

    fn rvalue(
        &self,
        rvalue: &AmirRvalue,
        ty: TypeId,
        output: &mut [u8],
        depth: usize,
        fuel: &mut usize,
    ) -> Option<()> {
        if depth > 64 {
            return None;
        }
        let owner = self.interner.resolve(ty);
        let layout = self
            .engine
            .layout_of_type(&owner, self.interner, self.provider)
            .ok()?;
        if usize::try_from(layout.size).ok()? != output.len() {
            return None;
        }
        match (rvalue, &owner) {
            (AmirRvalue::Use(value), _) => self.operand(value, ty, output, depth + 1, fuel),
            (
                AmirRvalue::EnumConstruct {
                    variant_tag,
                    payload,
                },
                _,
            ) => {
                let payload_ty = match &owner {
                    ArType::Option(inner) => match variant_tag {
                        0 => None,
                        1 => Some(*inner),
                        _ => return None,
                    },
                    ArType::Result(ok, error) => match variant_tag {
                        0 => Some(*ok),
                        1 => Some(*error),
                        _ => return None,
                    },
                    ArType::Named(_, _) => {
                        let variant = self
                            .provider
                            .get_enum_variants_for_type(&owner, self.interner)?
                            .get(*variant_tag)?
                            .clone();
                        if variant.payload_ty.is_some() {
                            Some(instantiated_enum_variant_payload_type(
                                &owner,
                                *variant_tag,
                                self.interner,
                                self.provider,
                            )?)
                        } else {
                            None
                        }
                    }
                    _ => return None,
                };
                let TagEncoding::Direct {
                    tag_size,
                    payload_offset,
                } = layout.tag_encoding?
                else {
                    // Pointer/niche representations need a separate proof of
                    // relocation-free payload provenance.
                    return None;
                };
                let tag = u64::try_from(*variant_tag).ok()?;
                let size = usize::try_from(tag_size).ok()?;
                if !matches!(size, 1 | 2 | 4 | 8) || (size < 8 && tag >= (1_u64 << (size * 8))) {
                    return None;
                }
                let bytes = if self.little {
                    tag.to_le_bytes()
                } else {
                    tag.to_be_bytes()
                };
                output.get_mut(..size)?.copy_from_slice(if self.little {
                    &bytes[..size]
                } else {
                    &bytes[8 - size..]
                });
                match (payload, payload_ty) {
                    (Some(value), Some(inner)) => {
                        self.field(value, inner, payload_offset, output, depth, fuel)
                    }
                    (None, None) => Some(()),
                    _ => None,
                }
            }
            (AmirRvalue::Array { items }, ArType::Array(count, inner))
                if usize::try_from(*count).ok()? == items.len() =>
            {
                let inner_layout = self
                    .engine
                    .layout_of_type(&self.interner.resolve(*inner), self.interner, self.provider)
                    .ok()?;
                for (index, item) in items.iter().enumerate() {
                    self.field(
                        item,
                        *inner,
                        u64::try_from(index).ok()?.checked_mul(inner_layout.size)?,
                        output,
                        depth,
                        fuel,
                    )?;
                }
                Some(())
            }
            (AmirRvalue::Tuple { items }, ArType::Tuple(args)) => {
                let args = self.interner.type_args(*args);
                if args.len() != items.len() {
                    return None;
                }
                for (index, (item, &inner)) in items.iter().zip(args.iter()).enumerate() {
                    self.field(
                        item,
                        inner,
                        *layout.field_offsets.get(index)?,
                        output,
                        depth,
                        fuel,
                    )?;
                }
                Some(())
            }
            (
                AmirRvalue::StructLiteral {
                    struct_symbol,
                    fields,
                },
                ArType::Named(symbol, _),
            ) if symbol == struct_symbol => {
                let declared = self
                    .provider
                    .get_struct_fields_for_type(&owner, self.interner)?;
                if fields.len() != declared.len() {
                    return None;
                }
                let mut seen = vec![false; fields.len()];
                for (name, item) in fields {
                    let field = declared.get(name)?;
                    if *seen.get(field.index)? {
                        return None;
                    }
                    *seen.get_mut(field.index)? = true;
                    let inner =
                        instantiated_field_type(&owner, name, self.interner, self.provider)?;
                    self.field(
                        item,
                        inner,
                        *layout.field_offsets.get(field.index)?,
                        output,
                        depth,
                        fuel,
                    )?;
                }
                Some(())
            }
            _ => None,
        }
    }
}

/// Build once per function. Backends choose the readonly section encoding,
/// but share proof of literal provenance and target object bytes.
#[must_use]
pub fn static_initializers(
    function: &AmirFunc,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    pool: &AmirLiteralPool,
    layout: DataLayout,
    little_endian: bool,
) -> FxHashMap<InstrId, StaticInitializer> {
    let mut serializer = Serializer {
        interner,
        provider,
        pool,
        engine: LayoutEngine::from_data_layout(layout),
        little: little_endian,
        definitions: FxHashMap::default(),
        uses: FxHashMap::default(),
    };
    for id in function.stmts.iter_ids() {
        let Some(statement) = function.stmts.get(id) else {
            continue;
        };
        if let AmirStmt::Assign { lhs, rhs } = statement {
            serializer
                .definitions
                .entry(*lhs)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(rhs));
        }
        for_each_stmt_operand(statement, |operand| {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand {
                *serializer.uses.entry(*temp).or_default() += 1;
            }
        });
    }
    for block in &function.blocks {
        for_each_terminator_operand(&block.terminator, |operand| {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand {
                *serializer.uses.entry(*temp).or_default() += 1;
            }
        });
    }
    let mut result = FxHashMap::default();
    let mut remaining = 65536usize;
    let mut byte_budget = 1024 * 1024usize;
    for id in function.stmts.iter_ids() {
        let Some(AmirStmt::Assign { lhs, rhs }) = function.stmts.get(id) else {
            continue;
        };
        if !matches!(
            rhs,
            AmirRvalue::Array { .. }
                | AmirRvalue::Tuple { .. }
                | AmirRvalue::StructLiteral { .. }
                | AmirRvalue::EnumConstruct { .. }
        ) {
            continue;
        }
        let Some(temp) = function.temps.get(lhs.as_usize()) else {
            continue;
        };
        if provider.is_copy_type(temp.ty) != Some(true) {
            continue;
        }
        let Ok(object_layout) =
            serializer
                .engine
                .layout_of_type(&interner.resolve(temp.ty), interner, provider)
        else {
            continue;
        };
        let Ok(size) = usize::try_from(object_layout.size) else {
            continue;
        };
        if size < 32 || size > byte_budget || remaining == 0 {
            continue;
        }
        let mut bytes = vec![0; size];
        if serializer
            .rvalue(rhs, temp.ty, &mut bytes, 0, &mut remaining)
            .is_some()
        {
            byte_budget -= size;
            result.insert(
                id,
                StaticInitializer {
                    bytes,
                    alignment: object_layout.align.max(1),
                },
            );
        }
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arandu_typeck::TypeInfo;

    fn serializer<'a>(
        info: &'a TypeInfo,
        pool: &'a AmirLiteralPool,
        little: bool,
    ) -> Serializer<'a> {
        Serializer {
            interner: &info.type_interner,
            provider: info,
            pool,
            engine: LayoutEngine::from_data_layout(DataLayout::ptr_width(4)),
            little,
            definitions: FxHashMap::default(),
            uses: FxHashMap::default(),
        }
    }

    #[test]
    fn scalar_bytes_obey_target_endian_signed_width_and_frozen_ieee_bits() {
        let info = TypeInfo::default();
        let mut pool = AmirLiteralPool::default();
        let negative = AmirConstant::Pool(pool.intern_int("-1"));
        let char_value = AmirConstant::Pool(pool.intern_char("é"));
        let float_ty = FloatType::new(Primitive::F32, DataLayout::ptr_width(4)).unwrap();
        let nan = AmirConstant::Pool(
            pool.intern_float_bits(ConstFloat::new(float_ty, 0x7fc01234).unwrap()),
        );
        for little in [true, false] {
            let serializer = serializer(&info, &pool, little);
            let mut bytes = [0; 4];
            serializer
                .scalar(
                    &negative,
                    info.type_interner.intern(ArType::Primitive(Primitive::I32)),
                    &mut bytes,
                )
                .unwrap();
            assert_eq!(bytes, [255; 4]);
            serializer
                .scalar(
                    &char_value,
                    info.type_interner
                        .intern(ArType::Primitive(Primitive::Char)),
                    &mut bytes,
                )
                .unwrap();
            assert_eq!(
                bytes,
                if little {
                    233u32.to_le_bytes()
                } else {
                    233u32.to_be_bytes()
                }
            );
            serializer
                .scalar(
                    &nan,
                    info.type_interner.intern(ArType::Primitive(Primitive::F32)),
                    &mut bytes,
                )
                .unwrap();
            assert_eq!(
                bytes,
                if little {
                    0x7fc01234u32.to_le_bytes()
                } else {
                    0x7fc01234u32.to_be_bytes()
                }
            );
        }
    }

    #[test]
    fn tuple_serialization_zeroes_padding_and_nested_array_bytes_are_inline() {
        let info = TypeInfo::default();
        let mut pool = AmirLiteralPool::default();
        let narrow = info.type_interner.intern(ArType::Primitive(Primitive::U8));
        let wide = info.type_interner.intern(ArType::Primitive(Primitive::U64));
        let array = info.type_interner.intern(ArType::Array(4, narrow));
        let tuple = info.type_interner.intern(ArType::Tuple(
            info.type_interner.push_type_args(&[array, wide]),
        ));
        let literal = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("7")));
        let array_value = AmirRvalue::Array {
            items: vec![literal; 4],
        };
        let temp = TempId::from_usize(1);
        let tuple_value = AmirRvalue::Tuple {
            items: vec![AmirOperand::Move(temp), literal],
        };
        let mut serializer = serializer(&info, &pool, true);
        serializer.definitions.insert(temp, Some(&array_value));
        serializer.uses.insert(temp, 1);
        let layout = serializer
            .engine
            .layout_of_type(
                &info.type_interner.resolve(tuple),
                &info.type_interner,
                &info,
            )
            .unwrap();
        let mut bytes = vec![0; usize::try_from(layout.size).unwrap()];
        serializer
            .rvalue(&tuple_value, tuple, &mut bytes, 0, &mut 100)
            .unwrap();
        assert_eq!(&bytes[..4], &[7; 4]);
        assert_eq!(&bytes[4..8], &[0; 4]);
        assert_eq!(&bytes[8..], &7u64.to_le_bytes());
    }

    #[test]
    fn mutable_aliases_bad_shapes_and_exhausted_work_never_become_static_data() {
        let info = TypeInfo::default();
        let mut pool = AmirLiteralPool::default();
        let byte = info.type_interner.intern(ArType::Primitive(Primitive::U8));
        let array = info.type_interner.intern(ArType::Array(4, byte));
        let literal = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("7")));
        let value = AmirRvalue::Array {
            items: vec![literal; 4],
        };
        let temp = TempId::from_usize(1);
        let mut serializer = serializer(&info, &pool, true);
        serializer.definitions.insert(temp, Some(&value));
        serializer.uses.insert(temp, 2);
        assert!(
            serializer
                .operand(&AmirOperand::Copy(temp), array, &mut [0; 4], 0, &mut 100)
                .is_none()
        );
        assert!(
            serializer
                .rvalue(&value, array, &mut [0; 3], 0, &mut 100)
                .is_none()
        );
        assert!(
            serializer
                .rvalue(&value, array, &mut [0; 4], 0, &mut 0)
                .is_none()
        );
        assert!(
            serializer
                .scalar(&AmirConstant::Nil, byte, &mut [0])
                .is_none()
        );
    }
    #[test]
    fn enum_serialization_checks_tags_payloads_and_target_byte_order() {
        let info = TypeInfo::default();
        let mut pool = AmirLiteralPool::default();
        let integer = info.type_interner.intern(ArType::Primitive(Primitive::I32));
        let result = info.type_interner.intern(ArType::Result(integer, integer));
        let option = info.type_interner.intern(ArType::Option(integer));
        let payload = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("42")));
        for little in [true, false] {
            let serializer = serializer(&info, &pool, little);
            let mut bytes = [0; 8];
            serializer
                .rvalue(
                    &AmirRvalue::EnumConstruct {
                        variant_tag: 1,
                        payload: Some(payload),
                    },
                    result,
                    &mut bytes,
                    0,
                    &mut 100,
                )
                .unwrap();
            let tag = if little {
                1_u32.to_le_bytes()
            } else {
                1_u32.to_be_bytes()
            };
            let value = if little {
                42_i32.to_le_bytes()
            } else {
                42_i32.to_be_bytes()
            };
            assert_eq!(&bytes[..4], &tag);
            assert_eq!(&bytes[4..], &value);
            assert!(
                serializer
                    .rvalue(
                        &AmirRvalue::EnumConstruct {
                            variant_tag: 2,
                            payload: Some(payload)
                        },
                        result,
                        &mut bytes,
                        0,
                        &mut 100
                    )
                    .is_none()
            );
            assert!(
                serializer
                    .rvalue(
                        &AmirRvalue::EnumConstruct {
                            variant_tag: 0,
                            payload: None
                        },
                        result,
                        &mut bytes,
                        0,
                        &mut 100
                    )
                    .is_none()
            );
            assert!(
                serializer
                    .rvalue(
                        &AmirRvalue::EnumConstruct {
                            variant_tag: 0,
                            payload: Some(payload)
                        },
                        option,
                        &mut bytes,
                        0,
                        &mut 100
                    )
                    .is_none()
            );
            bytes.fill(0);
            serializer
                .rvalue(
                    &AmirRvalue::EnumConstruct {
                        variant_tag: 0,
                        payload: None,
                    },
                    option,
                    &mut bytes,
                    0,
                    &mut 100,
                )
                .unwrap();
            assert_eq!(bytes, [0; 8]);
        }
    }
}
