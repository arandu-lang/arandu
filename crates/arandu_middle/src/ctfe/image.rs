//! Data images of static storage: a frozen constant laid out in target
//! bytes, plus the objects its pointers reach. Shared by every backend so the
//! data section and the code that addresses it agree on one layout.

use crate::layout::{
    DataLayout, LayoutEngine, StructLayoutProvider, TagEncoding,
    instantiated_enum_variant_payload_type, instantiated_field_type,
};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};

/// A pointer-sized word at `offset` of an object that holds the address of
/// `objects[target]` of the same [`StaticImage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StaticRelocation {
    pub offset: u64,
    pub target: usize,
}

/// One read-only object a static image points to: the bytes of a `str`, or
/// the elements of a slice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StaticObject {
    pub bytes: Vec<u8>,
    pub alignment: u64,
    pub relocations: Vec<StaticRelocation>,
}

/// Target bytes and alignment of one static object, and the objects its
/// pointers reach. Every pointer word is zero in `bytes`; a backend writes
/// the target's address there (a relocation of its object format).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StaticImage {
    pub bytes: Vec<u8>,
    pub alignment: u64,
    pub relocations: Vec<StaticRelocation>,
    pub objects: Vec<StaticObject>,
}

/// Most objects one image may point to.
const MAX_OBJECTS: usize = 4096;

/// Target bytes of a frozen constant laid out as `ty`. Strings, byte views
/// and slices become objects of their own reached through relocations.
/// Fails closed (`None`) on a value with no image, such as a niche or
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
        pointer: layout.pointer.size,
    };
    let owner = encoder
        .engine
        .layout_of_type(&interner.resolve(ty), interner, provider)
        .ok()?;
    let mut bytes = vec![0; usize::try_from(owner.size).ok()?];
    let mut objects = Vec::new();
    let mut relocations = Vec::new();
    encoder.value(
        value,
        ty,
        &mut Target {
            bytes: &mut bytes,
            base: 0,
            relocations: &mut relocations,
        },
        &mut objects,
        0,
    )?;
    Some(StaticImage {
        bytes,
        alignment: owner.align,
        relocations,
        objects,
    })
}

/// Where a value is written: a window of the current object's bytes that
/// starts `base` bytes into it, and that object's relocations.
struct Target<'b> {
    bytes: &'b mut [u8],
    base: u64,
    relocations: &'b mut Vec<StaticRelocation>,
}

impl Target<'_> {
    fn window(&mut self, offset: u64, size: u64) -> Option<Target<'_>> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(usize::try_from(size).ok()?)?;
        Some(Target {
            bytes: self.bytes.get_mut(start..end)?,
            base: self.base.checked_add(offset)?,
            relocations: self.relocations,
        })
    }
}

struct ConstEncoder<'a> {
    interner: &'a TypeInterner,
    provider: &'a dyn StructLayoutProvider,
    engine: LayoutEngine,
    little: bool,
    pointer: u64,
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

    fn size_of(&self, ty: TypeId) -> Option<u64> {
        Some(
            self.engine
                .layout_of_type(&self.interner.resolve(ty), self.interner, self.provider)
                .ok()?
                .size,
        )
    }

    /// A fat pointer `{data, len}` to a new object holding `bytes`.
    fn fat_pointer(
        &self,
        object: StaticObject,
        len: u64,
        output: &mut Target<'_>,
        objects: &mut Vec<StaticObject>,
    ) -> Option<()> {
        if objects.len() >= MAX_OBJECTS {
            return None;
        }
        let pointer = self.pointer;
        if output.bytes.len() != usize::try_from(pointer.checked_mul(2)?).ok()? {
            return None;
        }
        output.relocations.push(StaticRelocation {
            offset: output.base,
            target: objects.len(),
        });
        objects.push(object);
        let len_slot = output.bytes.get_mut(usize::try_from(pointer).ok()?..)?;
        self.write_uint(u128::from(len), len_slot)
    }

    fn value(
        &self,
        value: &super::ConstValue,
        ty: TypeId,
        output: &mut Target<'_>,
        objects: &mut Vec<StaticObject>,
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
                self.write_uint(u128::from(*value), output.bytes)
            }
            (ConstValue::Integer(value), ArType::Primitive(_)) => self.write_uint(
                u128::from_le_bytes(value.value().to_le_bytes()),
                output.bytes,
            ),
            (ConstValue::Float(value), ArType::Primitive(primitive)) if primitive.is_float() => {
                self.write_uint(u128::from(value.bits()), output.bytes)
            }
            // A `str` and a byte view point at their bytes.
            (ConstValue::String(text), ArType::Primitive(Primitive::Str)) => {
                let bytes = text.as_str().as_bytes().to_vec();
                let len = u64::try_from(bytes.len()).ok()?;
                let object = StaticObject {
                    bytes,
                    alignment: 1,
                    relocations: Vec::new(),
                };
                self.fat_pointer(object, len, output, objects)
            }
            (ConstValue::Bytes(view), ArType::Slice(inner))
                if matches!(
                    self.interner.resolve(*inner),
                    ArType::Primitive(Primitive::U8 | Primitive::Byte)
                ) =>
            {
                let bytes = view.as_bytes().to_vec();
                let len = u64::try_from(bytes.len()).ok()?;
                let object = StaticObject {
                    bytes,
                    alignment: 1,
                    relocations: Vec::new(),
                };
                self.fat_pointer(object, len, output, objects)
            }
            // `c"..."` as a `ptr[u8]`: the pointer word holds the address of
            // its bytes, NUL included, as C's `static char *p = "...";`.
            (ConstValue::Bytes(view), ArType::Ptr(inner))
                if matches!(
                    self.interner.resolve(*inner),
                    ArType::Primitive(Primitive::U8 | Primitive::I8 | Primitive::Byte)
                ) =>
            {
                if objects.len() >= MAX_OBJECTS
                    || output.bytes.len() != usize::try_from(self.pointer).ok()?
                {
                    return None;
                }
                output.relocations.push(StaticRelocation {
                    offset: output.base,
                    target: objects.len(),
                });
                objects.push(StaticObject {
                    bytes: view.as_bytes().to_vec(),
                    alignment: 1,
                    relocations: Vec::new(),
                });
                Some(())
            }
            // A slice of other values points at an object holding them laid
            // out as an array.
            (ConstValue::Aggregate(aggregate), ArType::Slice(inner))
                if aggregate.variant().is_none() =>
            {
                let element = self
                    .engine
                    .layout_of_type(&self.interner.resolve(*inner), self.interner, self.provider)
                    .ok()?;
                let values = aggregate.values();
                let count = u64::try_from(values.len()).ok()?;
                let mut bytes = vec![0; usize::try_from(element.size.checked_mul(count)?).ok()?];
                let mut relocations = Vec::new();
                {
                    let mut array = Target {
                        bytes: &mut bytes,
                        base: 0,
                        relocations: &mut relocations,
                    };
                    for (index, item) in values.iter().enumerate() {
                        let offset = u64::try_from(index).ok()?.checked_mul(element.size)?;
                        let mut slot = array.window(offset, element.size)?;
                        self.value(item, *inner, &mut slot, objects, depth + 1)?;
                    }
                }
                let object = StaticObject {
                    bytes,
                    alignment: element.align.max(1),
                    relocations,
                };
                self.fat_pointer(object, count, output, objects)
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
                        objects,
                        depth,
                    );
                }
                let values = aggregate.values();
                match &owner {
                    ArType::Array(count, inner)
                        if usize::try_from(*count).ok()? == values.len() =>
                    {
                        let stride = self.size_of(*inner)?;
                        for (index, item) in values.iter().enumerate() {
                            let offset = u64::try_from(index).ok()?.checked_mul(stride)?;
                            let mut slot = output.window(offset, stride)?;
                            self.value(item, *inner, &mut slot, objects, depth + 1)?;
                        }
                        Some(())
                    }
                    ArType::Tuple(args) => {
                        let args = self.interner.type_args(*args);
                        if args.len() != values.len() {
                            return None;
                        }
                        for (index, (item, &inner)) in values.iter().zip(args.iter()).enumerate() {
                            let mut slot = output
                                .window(*layout.field_offsets.get(index)?, self.size_of(inner)?)?;
                            self.value(item, inner, &mut slot, objects, depth + 1)?;
                        }
                        Some(())
                    }
                    ArType::Named(symbol, _) => {
                        let declared = self
                            .provider
                            .get_struct_fields_for_type(&owner, self.interner)?;
                        if self.provider.is_union(*symbol) {
                            // A union holds one member at offset zero: the
                            // single value given, written as the first member
                            // it fits, or the one non-void member value.
                            let candidates: Vec<(&super::ConstValue, &str)> = match values {
                                [] => return Some(()),
                                [item] => declared
                                    .iter()
                                    .map(|field| (item, field.name.as_str()))
                                    .collect(),
                                _ if values.len() == declared.len() => declared
                                    .iter()
                                    .zip(values)
                                    .filter(|(_, item)| !matches!(item, ConstValue::Void))
                                    .map(|(field, item)| (item, field.name.as_str()))
                                    .collect(),
                                _ => return None,
                            };
                            let all_void = candidates.is_empty();
                            for (item, field) in candidates {
                                if let Some(inner) = instantiated_field_type(
                                    &owner,
                                    field,
                                    self.interner,
                                    self.provider,
                                ) && let Some(size) = self.size_of(inner)
                                    && let Some(mut slot) = output.window(0, size)
                                    && self
                                        .value(item, inner, &mut slot, objects, depth + 1)
                                        .is_some()
                                {
                                    return Some(());
                                }
                            }
                            return all_void.then_some(());
                        }
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
                            let mut slot = output.window(
                                *layout.field_offsets.get(field.index)?,
                                self.size_of(inner)?,
                            )?;
                            self.value(
                                values.get(field.index)?,
                                inner,
                                &mut slot,
                                objects,
                                depth + 1,
                            )?;
                        }
                        Some(())
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn variant(
        &self,
        owner: &ArType,
        layout: &crate::layout::TypeLayout,
        tag: usize,
        values: &[super::ConstValue],
        output: &mut Target<'_>,
        objects: &mut Vec<StaticObject>,
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
        self.write_uint(u128::try_from(tag).ok()?, output.bytes.get_mut(..size)?)?;
        match (values, payload_ty) {
            ([], None) => Some(()),
            ([payload], Some(inner)) => {
                let mut slot = output.window(payload_offset, self.size_of(inner)?)?;
                self.value(payload, inner, &mut slot, objects, depth + 1)
            }
            _ => None,
        }
    }
}
