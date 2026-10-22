use super::{
    LayoutError, LayoutOperation, LayoutWalk, StructLayoutProvider, TagEncoding, TypeLayout,
    substitute,
};
use crate::SymbolId;
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use rustc_hash::FxHashMap;

impl super::LayoutEngine {
    pub(super) fn layout_of_type_inner(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
        walk: &mut LayoutWalk,
        depth: usize,
    ) -> Result<TypeLayout, LayoutError> {
        walk.visit(depth)?;
        Ok(match ty {
            ArType::Primitive(p) => match p {
                Primitive::I8 | Primitive::U8 | Primitive::Byte | Primitive::Bool => {
                    TypeLayout::simple(1, 1)
                }
                Primitive::I16 | Primitive::U16 => TypeLayout::simple(2, 2),
                Primitive::I32 | Primitive::U32 | Primitive::F32 => TypeLayout::simple(4, 4),
                Primitive::Char => TypeLayout::simple(4, 4),
                Primitive::Float => {
                    let f = self.data_layout.float;
                    TypeLayout::simple(f.size, f.abi_align)
                }
                Primitive::I64 | Primitive::U64 => {
                    let t = self.data_layout.i64;
                    TypeLayout::simple(t.size, t.abi_align)
                }
                Primitive::F64 => {
                    let t = self.data_layout.f64;
                    TypeLayout::simple(t.size, t.abi_align)
                }
                Primitive::Int | Primitive::Uint => TypeLayout::simple(4, 4),
                Primitive::ISize | Primitive::USize => {
                    let p = self.data_layout.pointer;
                    TypeLayout::simple(p.size, p.abi_align)
                }
                Primitive::Str => self.fat_pointer_layout(),
                Primitive::Any => {
                    let p = self.data_layout.pointer;
                    TypeLayout::simple(p.size, p.abi_align)
                }
            },
            ArType::Vector(_) => TypeLayout::simple(16, 16),
            ArType::IntLiteral => TypeLayout::simple(4, 4),
            ArType::FloatLiteral => {
                let f = self.data_layout.float;
                TypeLayout::simple(f.size, f.abi_align)
            }
            // `Err` is a non-null message handle (pointer to a NUL-terminated
            // UTF-8 buffer allocated by `err.new`). Not a ZST — payload of
            // `Result<T, Err>` must be distinguishable from nil.
            ArType::Err => {
                let p = self.data_layout.pointer;
                TypeLayout::simple(p.size, p.abi_align)
            }
            // Error has a recovery-only zero layout for existing IDE callers;
            // executable lowering rejects erroneous type-check results first.
            ArType::Void | ArType::Error => TypeLayout::simple(0, 1),
            ArType::Ref(inner) | ArType::RefMut(inner)
                if interner.with_type(*inner, |inner| {
                    matches!(inner, ArType::Slice(_) | ArType::MutSlice(_))
                }) =>
            {
                // References to dynamically-sized slices carry both data and
                // length. Borrow kind is erased after ownership validation.
                self.fat_pointer_layout()
            }
            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) => {
                // References to sized values and raw pointers are one word.
                let p = self.data_layout.pointer;
                TypeLayout::simple(p.size, p.abi_align)
            }
            // F2.3: GenRef = {u32 index, u32 generation} — always 8 bytes.
            ArType::GenRef => TypeLayout {
                size: 8,
                align: 4,
                field_offsets: vec![0, 4],
                tag_encoding: None,
            },
            ArType::Nullable(_) => {
                // Nullable is always a null-or-pointer handle (box for scalars;
                // heap object pointer for Named/etc.). Never stores the payload
                // inline — avoids `int? = 0` colliding with `nil`.
                let p = self.data_layout.pointer;
                TypeLayout::simple(p.size, p.abi_align)
            }
            ArType::Slice(_) | ArType::MutSlice(_) => self.fat_pointer_layout(),
            ArType::Array(len, inner) => {
                let inner_layout = self.layout_of_type_inner(
                    &interner.resolve(*inner),
                    interner,
                    provider,
                    walk,
                    depth + 1,
                )?;
                TypeLayout {
                    size: self.checked_mul(
                        inner_layout.size,
                        *len,
                        LayoutOperation::ArrayRepeat,
                    )?,
                    align: inner_layout.align,
                    field_offsets: Vec::new(),
                    tag_encoding: None,
                }
            }
            ArType::ConstArray(_, _)
            | ArType::FrozenConst(_)
            | ArType::Const(_)
            | ArType::ConstParam(_) => {
                return Err(LayoutError::UnresolvedConst);
            }
            ArType::Tuple(tys) => {
                let ty_ids = interner.type_args(*tys).to_vec();
                let mut current_offset = 0;
                let mut max_align = 1;
                let mut field_offsets = Vec::with_capacity(ty_ids.len());

                for &ty_id in &ty_ids {
                    let layout = self.layout_of_type_inner(
                        &interner.resolve(ty_id),
                        interner,
                        provider,
                        walk,
                        depth + 1,
                    )?;
                    max_align = max_align.max(layout.align);
                    current_offset =
                        self.align_up(current_offset, layout.align, LayoutOperation::FieldOffset)?;
                    field_offsets.push(current_offset);
                    current_offset = self.checked_add(
                        current_offset,
                        layout.size,
                        LayoutOperation::FieldOffset,
                    )?;
                }

                let total_size =
                    self.align_up(current_offset, max_align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align: max_align,
                    field_offsets,
                    tag_encoding: None,
                }
            }
            ArType::Named(symbol_id, generic_args) => {
                if let Some(fields_def) = provider.get_struct_fields_for_type(ty, interner) {
                    let generic_params = provider.get_generic_params(*symbol_id).unwrap_or(&[]);
                    let arg_ids = interner.type_args(*generic_args);
                    let subst: FxHashMap<SymbolId, TypeId> = generic_params
                        .iter()
                        .copied()
                        .zip(arg_ids.iter().copied())
                        .collect();

                    struct FieldItem {
                        orig_index: usize,
                        layout: TypeLayout,
                    }

                    let mut items = Vec::with_capacity(fields_def.len());
                    for f in fields_def.iter() {
                        let ty = interner.resolve(f.ty);
                        let substituted = substitute(&ty, &subst, interner);
                        let layout = self.layout_of_type_inner(
                            &substituted,
                            interner,
                            provider,
                            walk,
                            depth + 1,
                        )?;
                        items.push(FieldItem {
                            orig_index: f.index,
                            layout,
                        });
                    }

                    if provider.is_repr_c(*symbol_id) {
                        items.sort_by_key(|item| item.orig_index);
                    } else {
                        // A4.0: Sort by descending alignment, then descending size,
                        // with original declaration index as a deterministic tie-breaker.
                        items.sort_by_key(|item| {
                            (
                                std::cmp::Reverse(item.layout.align),
                                std::cmp::Reverse(item.layout.size),
                                item.orig_index,
                            )
                        });
                    }

                    let mut current_offset = 0;
                    let mut max_align = 1;
                    let mut field_offsets = vec![0u64; items.len()];

                    for item in items {
                        max_align = max_align.max(item.layout.align);
                        current_offset = self.align_up(
                            current_offset,
                            item.layout.align,
                            LayoutOperation::FieldOffset,
                        )?;
                        if item.orig_index < field_offsets.len() {
                            field_offsets[item.orig_index] = current_offset;
                        }
                        current_offset = self.checked_add(
                            current_offset,
                            item.layout.size,
                            LayoutOperation::FieldOffset,
                        )?;
                    }

                    let total_size = self.align_up(
                        current_offset,
                        max_align,
                        LayoutOperation::AggregatePadding,
                    )?;

                    TypeLayout {
                        size: total_size,
                        align: max_align,
                        field_offsets,
                        tag_encoding: None,
                    }
                } else if let Some(mut variants) = provider.get_enum_variants_for_type(ty, interner)
                {
                    // Enum payload declarations are stored using the enum's
                    // generic parameters, just like generic struct fields.
                    // Substitute the concrete arguments before computing the
                    // layout of `Enum<T>` so `Enum<int>` includes its payload.
                    let generic_params = provider.get_generic_params(*symbol_id).unwrap_or(&[]);
                    let arg_ids = interner.type_args(*generic_args);
                    if !generic_params.is_empty() && !arg_ids.is_empty() {
                        let subst: FxHashMap<SymbolId, TypeId> = generic_params
                            .iter()
                            .copied()
                            .zip(arg_ids.iter().copied())
                            .collect();
                        for variant in &mut variants {
                            let Some(payload_id) = variant.payload_ty else {
                                continue;
                            };
                            let payload_ty = interner.resolve(payload_id);
                            let substituted = substitute(&payload_ty, &subst, interner);
                            variant.payload_ty = Some(interner.intern(substituted));
                        }
                    }
                    let num_variants = variants.len();
                    let max_tag_bits: u32 = if self.pointer_width() >= 8 { 3 } else { 2 };
                    let mut all_payloads_eligible = true;
                    let mut min_payload_align = u64::MAX;
                    let mut non_unit_count = 0;

                    for variant in &variants {
                        if let Some(payload_ty_id) = variant.payload_ty {
                            non_unit_count += 1;
                            let payload_ty = interner.resolve(payload_ty_id);
                            // Pointee alignment is an optional tagging proof,
                            // not a prerequisite for a pointer payload's size.
                            // Recursive enums through references may not admit
                            // that proof; retain the conservative direct tag.
                            if let Some(align) = self
                                .ref_pointee_align_inner(
                                    &payload_ty,
                                    interner,
                                    provider,
                                    walk,
                                    depth + 1,
                                )
                                .ok()
                                .flatten()
                            {
                                if align < min_payload_align {
                                    min_payload_align = align;
                                }
                            } else {
                                all_payloads_eligible = false;
                                break;
                            }
                        }
                    }

                    let k = if all_payloads_eligible && non_unit_count > 0 && min_payload_align >= 2
                    {
                        let tz = min_payload_align.trailing_zeros();
                        tz.min(max_tag_bits)
                    } else {
                        0
                    };

                    if k >= 1 && num_variants <= (1usize << k) {
                        let tag_bits = k as u8;
                        let tag_mask = (1u64 << k) - 1;
                        let size = self.pointer_width();
                        let align = self.pointer_width();
                        TypeLayout {
                            size,
                            align,
                            field_offsets: vec![0],
                            tag_encoding: Some(TagEncoding::PointerTag {
                                tag_bits,
                                tag_mask,
                                pointer_offset: 0,
                            }),
                        }
                    } else {
                        let tag_size = self.pointer_width();
                        let mut max_payload_size = 0;
                        let mut max_payload_align = 1;
                        for variant in variants {
                            if let Some(payload_ty_id) = variant.payload_ty {
                                let payload_layout = self.layout_of_type_inner(
                                    &interner.resolve(payload_ty_id),
                                    interner,
                                    provider,
                                    walk,
                                    depth + 1,
                                )?;
                                if payload_layout.size > max_payload_size {
                                    max_payload_size = payload_layout.size;
                                }
                                if payload_layout.align > max_payload_align {
                                    max_payload_align = payload_layout.align;
                                }
                            }
                        }
                        let max_align = max_payload_align.max(tag_size);
                        let payload_offset = self.align_up(
                            tag_size,
                            max_payload_align,
                            LayoutOperation::EnumPayload,
                        )?;
                        let payload_end = self.checked_add(
                            payload_offset,
                            max_payload_size,
                            LayoutOperation::EnumPayload,
                        )?;
                        let size = self.align_up(
                            payload_end,
                            max_align,
                            LayoutOperation::AggregatePadding,
                        )?;
                        TypeLayout {
                            size,
                            align: max_align,
                            field_offsets: vec![0, payload_offset],
                            tag_encoding: Some(TagEncoding::Direct {
                                tag_size,
                                payload_offset,
                            }),
                        }
                    }
                } else {
                    return Err(LayoutError::UnknownType);
                }
            }

            ArType::Func(_, _) => TypeLayout::simple(self.pointer_width(), self.pointer_width()),
            ArType::Result(ok, err) => {
                let ok_layout = self.layout_of_type_inner(
                    &interner.resolve(*ok),
                    interner,
                    provider,
                    walk,
                    depth + 1,
                )?;
                let err_layout = self.layout_of_type_inner(
                    &interner.resolve(*err),
                    interner,
                    provider,
                    walk,
                    depth + 1,
                )?;
                let max_payload_align = ok_layout.align.max(err_layout.align);
                let max_align = max_payload_align.max(self.pointer_width());
                let tag_offset = 0;
                let payload_offset = self.align_up(
                    self.pointer_width(),
                    max_payload_align,
                    LayoutOperation::ResultPayload,
                )?;
                let max_payload_size = ok_layout.size.max(err_layout.size);
                let payload_end = self.checked_add(
                    payload_offset,
                    max_payload_size,
                    LayoutOperation::ResultPayload,
                )?;
                let total_size =
                    self.align_up(payload_end, max_align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align: max_align,
                    field_offsets: vec![tag_offset, payload_offset],
                    tag_encoding: Some(TagEncoding::Direct {
                        tag_size: self.pointer_width(),
                        payload_offset,
                    }),
                }
            }
            ArType::Option(inner) | ArType::Poll(inner) => {
                let inner_ty = interner.resolve(*inner);
                let inner_layout =
                    self.layout_of_type_inner(&inner_ty, interner, provider, walk, depth + 1)?;
                if matches!(ty, ArType::Option(_))
                    && self.has_null_niche_inner(&inner_ty, interner, provider, walk, depth + 1)?
                {
                    TypeLayout {
                        size: inner_layout.size,
                        align: inner_layout.align,
                        field_offsets: vec![0, 0],
                        tag_encoding: Some(TagEncoding::Niche {
                            niche_offset: 0,
                            niche_size: self.pointer_width(),
                            niche_value: 0,
                            untagged_variant: 1,
                            tagged_variant: 0,
                        }),
                    }
                } else {
                    let max_align = inner_layout.align.max(self.pointer_width());
                    let tag_offset = 0;
                    let payload_offset = self.align_up(
                        self.pointer_width(),
                        inner_layout.align,
                        LayoutOperation::OptionPayload,
                    )?;
                    let payload_end = self.checked_add(
                        payload_offset,
                        inner_layout.size,
                        LayoutOperation::OptionPayload,
                    )?;
                    let total_size =
                        self.align_up(payload_end, max_align, LayoutOperation::AggregatePadding)?;

                    TypeLayout {
                        size: total_size,
                        align: max_align,
                        field_offsets: vec![tag_offset, payload_offset],
                        tag_encoding: Some(TagEncoding::Direct {
                            tag_size: self.pointer_width(),
                            payload_offset,
                        }),
                    }
                }
            }
            ArType::Coroutine(_) => TypeLayout::simple(self.pointer_width(), self.pointer_width()),
            ArType::Range(inner) => {
                let inner_layout = self.layout_of_type_inner(
                    &interner.resolve(*inner),
                    interner,
                    provider,
                    walk,
                    depth + 1,
                )?;
                let align = inner_layout.align;
                let start_offset = 0;
                let end_offset =
                    self.align_up(inner_layout.size, align, LayoutOperation::RangePair)?;
                let end =
                    self.checked_add(end_offset, inner_layout.size, LayoutOperation::RangePair)?;
                let total_size = self.align_up(end, align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align,
                    field_offsets: vec![start_offset, end_offset],
                    tag_encoding: None,
                }
            }
        })
    }

    fn checked_add(
        &self,
        lhs: u64,
        rhs: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        let value = lhs
            .checked_add(rhs)
            .ok_or_else(|| LayoutError::SizeOverflow {
                operation,
                limit: self.data_layout.object_size_bound(),
            })?;
        self.check_object_size(value, operation)
    }

    fn checked_mul(
        &self,
        lhs: u64,
        rhs: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        let value = lhs
            .checked_mul(rhs)
            .ok_or_else(|| LayoutError::SizeOverflow {
                operation,
                limit: self.data_layout.object_size_bound(),
            })?;
        self.check_object_size(value, operation)
    }

    fn align_up(
        &self,
        value: u64,
        align: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        if align == 0 || !align.is_power_of_two() {
            return Err(LayoutError::InvalidAlignment { align });
        }
        let mask = align - 1;
        let padded = value
            .checked_add(mask)
            .ok_or_else(|| LayoutError::SizeOverflow {
                operation,
                limit: self.data_layout.object_size_bound(),
            })?
            & !mask;
        self.check_object_size(padded, operation)
    }

    fn check_object_size(
        &self,
        value: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        let limit = self.data_layout.object_size_bound();
        if value >= limit {
            Err(LayoutError::SizeOverflow { operation, limit })
        } else {
            Ok(value)
        }
    }
}
