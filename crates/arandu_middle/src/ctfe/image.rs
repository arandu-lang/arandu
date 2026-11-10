//! Initial images of writable static storage (`static mut`): a frozen
//! constant laid out in target bytes. Shared by every backend so the data
//! section and the code that addresses it agree on one layout.

use crate::layout::{
    DataLayout, LayoutEngine, StructLayoutProvider, TagEncoding,
    instantiated_enum_variant_payload_type, instantiated_field_type,
};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};

/// Target bytes and alignment of one static object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticImage {
    pub bytes: Vec<u8>,
    pub alignment: u64,
}

/// Target bytes of a frozen constant laid out as `ty`: the initial image of
/// writable static storage (`static mut`). Fails closed (`None`) on a value
/// with no relocation-free image, such as a string, a byte view or a niche or
/// pointer-tagged enum. Padding is zero.
#[must_use]
pub fn const_value_bytes(
    value: &super::ConstValue,
    ty: TypeId,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    layout: DataLayout,
    little_endian: bool,
) -> Option<StaticImage> {
    let encoder = ConstEncoder {
        interner,
        provider,
        engine: LayoutEngine::from_data_layout(layout),
        little: little_endian,
    };
    let owner = encoder
        .engine
        .layout_of_type(&interner.resolve(ty), interner, provider)
        .ok()?;
    let mut bytes = vec![0; usize::try_from(owner.size).ok()?];
    encoder.value(value, ty, &mut bytes, 0)?;
    Some(StaticImage {
        bytes,
        alignment: owner.align,
    })
}

struct ConstEncoder<'a> {
    interner: &'a TypeInterner,
    provider: &'a dyn StructLayoutProvider,
    engine: LayoutEngine,
    little: bool,
}

impl ConstEncoder<'_> {
    fn write_uint(&self, value: u128, output: &mut [u8]) -> Option<()> {
        if !matches!(output.len(), 1 | 2 | 4 | 8 | 16) {
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

    fn at<'b>(&self, ty: TypeId, offset: u64, output: &'b mut [u8]) -> Option<&'b mut [u8]> {
        let size = self
            .engine
            .layout_of_type(&self.interner.resolve(ty), self.interner, self.provider)
            .ok()?
            .size;
        let start = usize::try_from(offset).ok()?;
        output.get_mut(start..start.checked_add(usize::try_from(size).ok()?)?)
    }

    fn value(
        &self,
        value: &super::ConstValue,
        ty: TypeId,
        output: &mut [u8],
        depth: usize,
    ) -> Option<()> {
        use super::ConstValue;
        if depth > 64 {
            return None;
        }
        let owner = self.interner.resolve(ty);
        match (value, &owner) {
            (ConstValue::Void, _) => Some(()),
            (ConstValue::Bool(value), ArType::Primitive(Primitive::Bool)) => {
                self.write_uint(u128::from(*value), output)
            }
            (ConstValue::Integer(value), ArType::Primitive(_)) => {
                self.write_uint(u128::from_le_bytes(value.value().to_le_bytes()), output)
            }
            (ConstValue::Float(value), ArType::Primitive(primitive)) if primitive.is_float() => {
                self.write_uint(u128::from(value.bits()), output)
            }
            (ConstValue::Aggregate(aggregate), _) => {
                let layout = self
                    .engine
                    .layout_of_type(&owner, self.interner, self.provider)
                    .ok()?;
                if let Some(variant) = aggregate.variant() {
                    return self.variant(
                        &owner,
                        &layout,
                        variant.tag,
                        aggregate.values(),
                        output,
                        depth,
                    );
                }
                let values = aggregate.values();
                match &owner {
                    ArType::Array(count, inner)
                        if usize::try_from(*count).ok()? == values.len() =>
                    {
                        let stride = self
                            .engine
                            .layout_of_type(
                                &self.interner.resolve(*inner),
                                self.interner,
                                self.provider,
                            )
                            .ok()?
                            .size;
                        for (index, item) in values.iter().enumerate() {
                            let offset = u64::try_from(index).ok()?.checked_mul(stride)?;
                            let slot = self.at(*inner, offset, output)?;
                            self.value(item, *inner, slot, depth + 1)?;
                        }
                        Some(())
                    }
                    ArType::Tuple(args) => {
                        let args = self.interner.type_args(*args);
                        if args.len() != values.len() {
                            return None;
                        }
                        for (index, (item, &inner)) in values.iter().zip(args.iter()).enumerate() {
                            let slot = self.at(inner, *layout.field_offsets.get(index)?, output)?;
                            self.value(item, inner, slot, depth + 1)?;
                        }
                        Some(())
                    }
                    ArType::Named(_, _) => {
                        let declared = self
                            .provider
                            .get_struct_fields_for_type(&owner, self.interner)?;
                        if declared.len() != values.len() {
                            return None;
                        }
                        for field in declared.iter() {
                            let inner = instantiated_field_type(
                                &owner,
                                field.name.as_str(),
                                self.interner,
                                self.provider,
                            )?;
                            let slot =
                                self.at(inner, *layout.field_offsets.get(field.index)?, output)?;
                            self.value(values.get(field.index)?, inner, slot, depth + 1)?;
                        }
                        Some(())
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn variant(
        &self,
        owner: &ArType,
        layout: &crate::layout::TypeLayout,
        tag: usize,
        values: &[super::ConstValue],
        output: &mut [u8],
        depth: usize,
    ) -> Option<()> {
        let payload_ty = match owner {
            ArType::Option(inner) => (tag == 1).then_some(*inner),
            ArType::Result(ok, error) => Some(if tag == 0 { *ok } else { *error }),
            ArType::Named(_, _) => {
                let variant = self
                    .provider
                    .get_enum_variants_for_type(owner, self.interner)?
                    .get(tag)?
                    .clone();
                if variant.payload_ty.is_some() {
                    Some(instantiated_enum_variant_payload_type(
                        owner,
                        tag,
                        self.interner,
                        self.provider,
                    )?)
                } else {
                    None
                }
            }
            _ => return None,
        };
        // Niche and pointer-tagged layouts need a proof of the payload's bit
        // pattern; only an explicit tag is encoded.
        let TagEncoding::Direct {
            tag_size,
            payload_offset,
        } = layout.tag_encoding?
        else {
            return None;
        };
        let size = usize::try_from(tag_size).ok()?;
        self.write_uint(u128::try_from(tag).ok()?, output.get_mut(..size)?)?;
        match (values, payload_ty) {
            ([], None) => Some(()),
            ([payload], Some(inner)) => {
                let slot = self.at(inner, payload_offset, output)?;
                self.value(payload, inner, slot, depth + 1)
            }
            _ => None,
        }
    }
}
