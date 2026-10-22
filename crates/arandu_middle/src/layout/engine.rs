use super::{
    DataLayout, GenPayloadLayout, LayoutError, LayoutOperation, LayoutWalk, SOO_MAX_INLINE_BYTES,
    SooLayout, StructLayoutProvider, TypeLayout, substitute,
};
use crate::SymbolId;
use crate::types::{ArType, TypeId, TypeInterner};
use rustc_hash::FxHashMap;

impl super::LayoutEngine {
    /// Sugar for [`DataLayout::ptr_width`] (standard LP64/ILP32-style rules).
    #[must_use]
    pub fn new(pointer_width: u64) -> Self {
        Self::from_data_layout(DataLayout::ptr_width(pointer_width))
    }

    #[must_use]
    pub fn from_data_layout(data_layout: DataLayout) -> Self {
        Self { data_layout }
    }

    /// Host process layout (Cranelift JIT / host C parity).
    #[must_use]
    pub fn host() -> Self {
        Self::from_data_layout(DataLayout::host())
    }

    #[must_use]
    pub fn pointer_width(&self) -> u64 {
        self.data_layout.pointer_width()
    }

    /// Fat pointer ABI for `str` and `[]T`: `{ ptr, len }` where `len` is
    /// target `usize` (same width as a pointer). Offsets: ptr@0, len@W.
    /// 64-bit → size 16; 32-bit → size 8 (matches `arandu-abi-layout`).
    #[must_use]
    pub fn fat_pointer_layout(&self) -> TypeLayout {
        let w = self.pointer_width();
        let align = self.data_layout.pointer_align();
        TypeLayout {
            size: w * 2,
            align,
            field_offsets: vec![0, w],
            tag_encoding: None,
        }
    }

    /// Checks whether `ty` has a null niche (i.e. invalid bit-pattern 0x0 of pointer width at offset 0).
    /// Safe references (`ref T`, `mut ref T`), function pointers (`Func`), and error handles (`Err`)
    /// are never null. A struct whose first physical field has a null niche also provides a null niche.
    #[must_use]
    pub fn has_null_niche(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> bool {
        self.has_null_niche_inner(ty, interner, provider, &mut LayoutWalk::new(), 0)
            .unwrap_or(false)
    }

    pub(super) fn has_null_niche_inner(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
        walk: &mut LayoutWalk,
        depth: usize,
    ) -> Result<bool, LayoutError> {
        walk.visit(depth)?;
        Ok(match ty {
            ArType::Ref(_) | ArType::RefMut(_) | ArType::Func(_, _) | ArType::Err => true,
            ArType::Named(sym, args) => {
                if let Some(fields_def) = provider.get_struct_fields_for_type(ty, interner)
                    && let Ok(layout) =
                        self.layout_of_type_inner(ty, interner, provider, walk, depth + 1)
                {
                    for f in fields_def.iter() {
                        if layout.field_offsets.get(f.index) == Some(&0) {
                            let generic_params = provider.get_generic_params(*sym).unwrap_or(&[]);
                            let arg_ids = interner.type_args(*args);
                            let subst: FxHashMap<SymbolId, TypeId> = generic_params
                                .iter()
                                .copied()
                                .zip(arg_ids.iter().copied())
                                .collect();
                            let field_ty = interner.resolve(f.ty);
                            let substituted = substitute(&field_ty, &subst, interner);
                            return self.has_null_niche_inner(
                                &substituted,
                                interner,
                                provider,
                                walk,
                                depth + 1,
                            );
                        }
                    }
                }
                false
            }
            _ => false,
        })
    }

    /// Returns the alignment of the pointed-to object if `ty` is a reference or pointer,
    /// or a single-field struct wrapping one.
    pub fn ref_pointee_align(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Option<u64> {
        self.ref_pointee_align_inner(ty, interner, provider, &mut LayoutWalk::new(), 0)
            .ok()
            .flatten()
    }

    pub(super) fn ref_pointee_align_inner(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
        walk: &mut LayoutWalk,
        depth: usize,
    ) -> Result<Option<u64>, LayoutError> {
        walk.visit(depth)?;
        Ok(match ty {
            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                let inner_ty = interner.resolve(*inner);
                Some(
                    self.layout_of_type_inner(&inner_ty, interner, provider, walk, depth + 1)?
                        .align,
                )
            }
            ArType::Named(sym, args) => {
                if let Some(fields_def) = provider.get_struct_fields_for_type(ty, interner)
                    && fields_def.len() == 1
                {
                    let field = &fields_def.fields[0];
                    let generic_params = provider.get_generic_params(*sym).unwrap_or(&[]);
                    let arg_ids = interner.type_args(*args);
                    let subst: FxHashMap<SymbolId, TypeId> = generic_params
                        .iter()
                        .copied()
                        .zip(arg_ids.iter().copied())
                        .collect();
                    let field_ty = interner.resolve(field.ty);
                    let substituted = substitute(&field_ty, &subst, interner);
                    return self.ref_pointee_align_inner(
                        &substituted,
                        interner,
                        provider,
                        walk,
                        depth + 1,
                    );
                } else {
                    None
                }
            }
            _ => None,
        })
    }

    /// Returns the [`SooLayout`] for an element type `ty`, calculating how many
    /// elements fit inline within [`SOO_MAX_INLINE_BYTES`] (24 bytes).
    pub fn soo_layout_for_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<SooLayout, LayoutError> {
        let layout = self.layout_of_type(ty, interner, provider)?;
        let size = layout.size;
        let is_inline_eligible = size <= SOO_MAX_INLINE_BYTES;
        let inline_capacity = SOO_MAX_INLINE_BYTES
            .checked_div(size)
            .map_or(usize::MAX, |cap| cap as usize);
        Ok(SooLayout::new(inline_capacity, is_inline_eligible))
    }

    /// Checks whether `ty` is eligible for Small Object Optimization (size <= 24 bytes).
    #[must_use]
    pub fn is_soo_eligible(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> bool {
        self.layout_of_type(ty, interner, provider)
            .map(|l| l.size <= SOO_MAX_INLINE_BYTES)
            .unwrap_or(false)
    }

    /// Byte offset of the `len` field in a fat pointer (`str` / slice).
    #[must_use]
    pub fn fat_ptr_len_offset(&self) -> u64 {
        self.pointer_width()
    }

    /// Byte size of the `len` field (`usize` of the target).
    #[must_use]
    pub fn fat_ptr_len_size(&self) -> u64 {
        self.pointer_width()
    }

    /// Compute the memory layout of any canonical `TypeId`.
    pub fn layout_of(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<TypeLayout, LayoutError> {
        self.layout_of_type(&interner.resolve(type_id), interner, provider)
    }

    /// Derive the checked physical contract consumed by GenRef runtime slots.
    pub fn gen_payload_layout(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<GenPayloadLayout, LayoutError> {
        let layout = self.layout_of(type_id, interner, provider)?;
        let limit = self.data_layout.object_size_bound();
        if layout.size >= limit {
            return Err(LayoutError::SizeOverflow {
                operation: LayoutOperation::GenPayload,
                limit,
            });
        }
        if layout.align == 0 || !layout.align.is_power_of_two() {
            return Err(LayoutError::InvalidAlignment {
                align: layout.align,
            });
        }
        Ok(GenPayloadLayout {
            size: layout.size,
            align: layout.align,
        })
    }

    /// Compute the memory layout of a structural `ArType`.
    #[tracing::instrument(
        level = "trace",
        target = "arandu_middle::layout",
        skip(self, interner, provider)
    )]
    pub fn layout_of_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<TypeLayout, LayoutError> {
        self.layout_of_type_inner(ty, interner, provider, &mut LayoutWalk::new(), 0)
    }
}
