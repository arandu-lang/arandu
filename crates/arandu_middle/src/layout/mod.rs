pub mod abi;
mod data_layout;

pub use abi::{AbiScalar, AbiSlot, ArgAbi, DirectAbi, TargetAbi, TargetAbiClassifier};
pub use data_layout::{DataLayout, SizeAlign};

use crate::SymbolId;
use crate::index_vec::IdIndex;
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use smol_str::SmolStr;

/// A compact contiguous range into a dense backing table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DenseRange {
    pub start: u32,
    pub len: u32,
}

impl DenseRange {
    #[must_use]
    pub const fn empty() -> Self {
        Self { start: 0, len: 0 }
    }

    #[must_use]
    pub const fn new(start: usize, len: usize) -> Self {
        Self {
            start: start as u32,
            len: len as u32,
        }
    }

    #[must_use]
    pub const fn start_usize(self) -> usize {
        self.start as usize
    }

    #[must_use]
    pub const fn len_usize(self) -> usize {
        self.len as usize
    }

    #[must_use]
    pub const fn end_usize(self) -> usize {
        self.start as usize + self.len as usize
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn as_range(self) -> std::ops::Range<usize> {
        self.start_usize()..self.end_usize()
    }

    #[must_use]
    pub fn iter_ids<I: IdIndex>(self) -> DenseRangeIds<I> {
        DenseRangeIds {
            next: self.start_usize(),
            end: self.end_usize(),
            _marker: std::marker::PhantomData,
        }
    }
}

pub struct DenseRangeIds<I: IdIndex> {
    next: usize,
    end: usize,
    _marker: std::marker::PhantomData<I>,
}

impl<I: IdIndex> Iterator for DenseRangeIds<I> {
    type Item = I;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let id = I::from_usize(self.next);
        self.next += 1;
        Some(id)
    }
}

/// Discriminant tag encoding strategy for enums and sum types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagEncoding {
    /// Explicit discriminant tag stored at `tag_offset` with size `tag_size`.
    Direct { tag_size: u64, payload_offset: u64 },
    /// Niche optimization: discriminant tag is elided by using an invalid bit-pattern
    /// (e.g. null pointer 0x0) of the payload at `niche_offset`.
    Niche {
        niche_offset: u64,
        niche_size: u64,
        niche_value: u64,
        untagged_variant: usize, // e.g. 1 for Option::Some
        tagged_variant: usize,   // e.g. 0 for Option::None
    },
    /// Pointer tagging optimization: discriminant tag is encoded in the lowest `tag_bits`
    /// of an aligned pointer/reference payload.
    PointerTag {
        tag_bits: u8,
        tag_mask: u64,
        pointer_offset: u64,
    },
}

/// Maximum inline byte capacity for Small Object Optimization (SOO).
///
/// Matches the 24-byte footprint (3 words on 64-bit) of `Vec` `{ data, len, capacity }`.
pub const SOO_MAX_INLINE_BYTES: u64 = 24;

/// Small Object Optimization (SOO) layout metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SooLayout {
    /// Maximum inline bytes available in the container (24 bytes).
    pub max_inline_bytes: u64,
    /// Number of elements of the given type that fit inline.
    pub inline_capacity: usize,
    /// Whether the type is eligible to be stored inline (size <= 24 bytes).
    pub is_inline_eligible: bool,
}

impl SooLayout {
    #[must_use]
    pub const fn new(inline_capacity: usize, is_inline_eligible: bool) -> Self {
        Self {
            max_inline_bytes: SOO_MAX_INLINE_BYTES,
            inline_capacity,
            is_inline_eligible,
        }
    }
}

/// Physical memory layout metadata for a resolved type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeLayout {
    pub size: u64,               // Total size in bytes (including trailing padding)
    pub align: u64,              // Alignment required in bytes (power of 2)
    pub field_offsets: Vec<u64>, // Field offsets (populated for structs, tuples, etc.)
    pub tag_encoding: Option<TagEncoding>,
}

impl TypeLayout {
    #[must_use]
    pub fn simple(size: u64, align: u64) -> Self {
        Self {
            size,
            align,
            field_offsets: Vec::new(),
            tag_encoding: None,
        }
    }
}

/// Target-derived runtime contract for a promoted GenRef payload.
///
/// Drop glue is attached during backend lowering; size and alignment always
/// originate in [`LayoutEngine`] rather than the compiler host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GenPayloadLayout {
    pub size: u64,
    pub align: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutOperation {
    ArrayRepeat,
    FieldOffset,
    AggregatePadding,
    EnumPayload,
    ResultPayload,
    OptionPayload,
    RangePair,
    GenPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    SizeOverflow {
        operation: LayoutOperation,
        limit: u64,
    },
    InvalidAlignment {
        align: u64,
    },
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeOverflow { operation, limit } => write!(
                f,
                "type layout overflow during {operation:?}; target object size must be below {limit} bytes"
            ),
            Self::InvalidAlignment { align } => {
                write!(f, "invalid type alignment {align}; expected a power of two")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumPayloadShape {
    pub payload_ty: Option<TypeId>,
}

/// One struct field: dense metadata folding name, symbol, type and index into
/// a single ordered entry. Replaces three parallel name-keyed maps
/// (`struct_fields` / `struct_field_symbols` / `struct_field_indices`).
///
/// `symbol` is optional because resolution may not have produced a definition
/// symbol for a malformed field; the type-based maps historically stayed
/// populated in that case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructFieldInfo {
    pub name: SmolStr,
    pub symbol: Option<SymbolId>,
    pub ty: TypeId,
    pub index: usize,
}

/// Ordered field table for one struct: `fields` in declaration order plus a
/// `by_name` index for O(1) lookups. Shared across typeck shards via `Arc`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructFields {
    pub fields: SmallVec<[StructFieldInfo; 6]>,
    by_name: FxHashMap<SmolStr, usize>,
}

impl StructFields {
    /// Empty field table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fields: SmallVec::new(),
            by_name: FxHashMap::default(),
        }
    }

    /// Build a field table from declaration-order entries, indexing by name.
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = StructFieldInfo>) -> Self {
        let mut table = Self::new();
        for entry in entries {
            table.by_name.insert(entry.name.clone(), table.fields.len());
            table.fields.push(entry);
        }
        table
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Look up a field by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&StructFieldInfo> {
        self.by_name.get(name).map(|&idx| &self.fields[idx])
    }

    /// Iterate fields in declaration order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &StructFieldInfo> + '_ {
        self.fields.iter()
    }
}

/// Decoupled metadata provider to resolve struct fields and generic parameters.
pub trait StructLayoutProvider {
    fn get_struct_fields(&self, struct_id: SymbolId) -> Option<&StructFields>;
    fn get_generic_params(&self, struct_id: SymbolId) -> Option<&[SymbolId]>;
    fn get_enum_variants(&self, enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>>;

    /// Structural Copy proof supplied by typeck when available. Layout-only
    /// test providers may return `None`; backends must then stay conservative.
    fn is_copy_type(&self, _ty: TypeId) -> Option<bool> {
        None
    }

    /// Explicit cleanup method associated with a nominal type by the
    /// language-level `@Destructor` contract. Backends must never infer this
    /// association from a function name or from the presence of pointers.
    fn destructor_for_type(&self, _ty: TypeId) -> Option<SymbolId> {
        None
    }

    /// Whether the struct is marked with `#[repr(C)]` / `@Repr("C")` and must
    /// preserve declaration field order for ABI compatibility.
    fn is_repr_c(&self, _struct_id: SymbolId) -> bool {
        false
    }
}

/// The physical memory layout engine.
///
/// All target-dependent sizes/alignments come from [`DataLayout`]. Prefer
/// [`LayoutEngine::from_data_layout`] / [`LayoutEngine::host`];
/// [`LayoutEngine::new`] remains as sugar for [`DataLayout::ptr_width`].
///
/// Language `float` is always IEEE f64 (see [`DataLayout`]); platform `int`
/// follows pointer width. i686 uses [`DataLayout::i686_sysv`] for i64/f64
/// abi_align=4.
#[derive(Debug, Clone)]
pub struct LayoutEngine {
    pub data_layout: DataLayout,
}

impl LayoutEngine {
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
        match ty {
            ArType::Ref(_) | ArType::RefMut(_) | ArType::Func(_, _) | ArType::Err => true,
            ArType::Named(sym, args) => {
                if let Some(fields_def) = provider.get_struct_fields(*sym)
                    && let Ok(layout) = self.layout_of_type(ty, interner, provider)
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
                            return self.has_null_niche(&substituted, interner, provider);
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Returns the alignment of the pointed-to object if `ty` is a reference or pointer,
    /// or a single-field struct wrapping one.
    pub fn ref_pointee_align(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Option<u64> {
        match ty {
            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                let inner_ty = interner.resolve(*inner);
                self.layout_of_type(&inner_ty, interner, provider)
                    .ok()
                    .map(|l| l.align)
            }
            ArType::Named(sym, args) => {
                if let Some(fields_def) = provider.get_struct_fields(*sym)
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
                    self.ref_pointee_align(&substituted, interner, provider)
                } else {
                    None
                }
            }
            _ => None,
        }
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
                Primitive::Int | Primitive::Uint => {
                    let p = self.data_layout.pointer;
                    TypeLayout::simple(p.size, p.abi_align)
                }
                Primitive::Str => self.fat_pointer_layout(),
                Primitive::Any => {
                    let p = self.data_layout.pointer;
                    TypeLayout::simple(p.size, p.abi_align)
                }
            },
            ArType::IntLiteral => {
                let p = self.data_layout.pointer;
                TypeLayout::simple(p.size, p.abi_align)
            }
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
            ArType::Void | ArType::Error => TypeLayout::simple(0, 1),
            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) => {
                // Safe refs and raw pointers are single machine pointers (fat types later).
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
            ArType::Slice(_) => self.fat_pointer_layout(),
            ArType::Array(len, inner) => {
                let inner_layout = self.layout_of(*inner, interner, provider)?;
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
            ArType::Tuple(tys) => {
                let ty_ids = interner.type_args(*tys).to_vec();
                let mut current_offset = 0;
                let mut max_align = 1;
                let mut field_offsets = Vec::with_capacity(ty_ids.len());

                for &ty_id in &ty_ids {
                    let layout = self.layout_of(ty_id, interner, provider)?;
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
                if let Some(fields_def) = provider.get_struct_fields(*symbol_id) {
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
                        let layout = self.layout_of_type(&substituted, interner, provider)?;
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
                } else if let Some(variants) = provider.get_enum_variants(*symbol_id) {
                    let num_variants = variants.len();
                    let max_tag_bits: u32 = if self.pointer_width() >= 8 { 3 } else { 2 };
                    let mut all_payloads_eligible = true;
                    let mut min_payload_align = u64::MAX;
                    let mut non_unit_count = 0;

                    for variant in &variants {
                        if let Some(payload_ty_id) = variant.payload_ty {
                            non_unit_count += 1;
                            let payload_ty = interner.resolve(payload_ty_id);
                            if let Some(align) =
                                self.ref_pointee_align(&payload_ty, interner, provider)
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
                                let payload_layout =
                                    self.layout_of(payload_ty_id, interner, provider)?;
                                if payload_layout.size > max_payload_size {
                                    max_payload_size = payload_layout.size;
                                }
                                if payload_layout.align > max_payload_align {
                                    max_payload_align = payload_layout.align;
                                }
                            }
                        }
                        let max_align = max_payload_align.max(tag_size);
                        let payload_end = self.checked_add(
                            tag_size,
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
                            field_offsets: vec![0, tag_size],
                            tag_encoding: Some(TagEncoding::Direct {
                                tag_size,
                                payload_offset: tag_size,
                            }),
                        }
                    }
                } else {
                    TypeLayout::simple(0, 1)
                }
            }

            ArType::Func(_, _) => TypeLayout::simple(self.pointer_width(), self.pointer_width()),
            ArType::Result(ok, err) => {
                let ok_layout = self.layout_of(*ok, interner, provider)?;
                let err_layout = self.layout_of(*err, interner, provider)?;
                let max_align = ok_layout
                    .align
                    .max(err_layout.align)
                    .max(self.pointer_width());
                let tag_offset = 0;
                let payload_offset = self.pointer_width();
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
                let inner_layout = self.layout_of(*inner, interner, provider)?;
                if matches!(ty, ArType::Option(_))
                    && self.has_null_niche(&inner_ty, interner, provider)
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
                    let payload_offset = self.pointer_width();
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
                let inner_layout = self.layout_of(*inner, interner, provider)?;
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

fn substitute(ty: &ArType, subst: &FxHashMap<SymbolId, TypeId>, interner: &TypeInterner) -> ArType {
    match ty {
        ArType::Named(id, args) => {
            if let Some(&concrete_id) = subst.get(id) {
                interner.resolve(concrete_id)
            } else {
                let arg_ids = interner.type_args(*args).to_vec();
                let new_args: Vec<TypeId> = arg_ids
                    .iter()
                    .map(|&arg_id| {
                        let arg_ty = interner.resolve(arg_id);
                        let substituted_arg = substitute(&arg_ty, subst, interner);
                        interner.lookup(&substituted_arg).unwrap_or(arg_id)
                    })
                    .collect();
                let range = interner.push_type_args(&new_args);
                ArType::Named(*id, range)
            }
        }
        ArType::Func(params, ret) => {
            let param_ids = interner.type_args(*params).to_vec();
            let new_params: Vec<TypeId> = param_ids
                .iter()
                .map(|&param_id| {
                    let param_ty = interner.resolve(param_id);
                    let substituted_param = substitute(&param_ty, subst, interner);
                    interner.lookup(&substituted_param).unwrap_or(param_id)
                })
                .collect();
            let ret_ty = interner.resolve(*ret);
            let substituted_ret = substitute(&ret_ty, subst, interner);
            let new_ret = interner.lookup(&substituted_ret).unwrap_or(*ret);
            let range = interner.push_type_args(&new_params);
            ArType::Func(range, new_ret)
        }
        ArType::Nullable(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Nullable(new_inner)
        }
        ArType::Slice(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Slice(new_inner)
        }
        ArType::Array(len, inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Array(*len, new_inner)
        }
        ArType::Ptr(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Ptr(new_inner)
        }
        ArType::Ref(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Ref(new_inner)
        }
        ArType::RefMut(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::RefMut(new_inner)
        }
        ArType::Tuple(tys) => {
            let ty_ids = interner.type_args(*tys).to_vec();
            let new_tys: Vec<TypeId> = ty_ids
                .iter()
                .map(|&ty_id| {
                    let item_ty = interner.resolve(ty_id);
                    let substituted_item = substitute(&item_ty, subst, interner);
                    interner.lookup(&substituted_item).unwrap_or(ty_id)
                })
                .collect();
            let range = interner.push_type_args(&new_tys);
            ArType::Tuple(range)
        }
        ArType::Result(ok, err) => {
            let ok_ty = interner.resolve(*ok);
            let substituted_ok = substitute(&ok_ty, subst, interner);
            let new_ok = interner.lookup(&substituted_ok).unwrap_or(*ok);

            let err_ty = interner.resolve(*err);
            let substituted_err = substitute(&err_ty, subst, interner);
            let new_err = interner.lookup(&substituted_err).unwrap_or(*err);

            ArType::Result(new_ok, new_err)
        }
        ArType::Option(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Option(new_inner)
        }
        ArType::Coroutine(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Coroutine(new_inner)
        }
        ArType::Poll(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Poll(new_inner)
        }
        ArType::Range(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Range(new_inner)
        }
        other => other.clone(),
    }
}

/// Resolve a declared field through the concrete arguments used by layout.
/// Returns `None` for unavailable metadata or an inconsistent generic arity.
#[must_use]
pub fn instantiated_field_type(
    owner: &ArType,
    field_name: &str,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> Option<TypeId> {
    let ArType::Named(symbol, arguments) = owner else {
        return None;
    };
    let field = provider.get_struct_fields(*symbol)?.get(field_name)?.ty;
    let parameters = provider.get_generic_params(*symbol).unwrap_or(&[]);
    let arguments = interner.type_args(*arguments);
    if parameters.len() != arguments.len() {
        return None;
    }
    if parameters.is_empty() {
        return Some(field);
    }
    let substitution = crate::types::build_subst_ids(parameters, &arguments, interner);
    Some(crate::types::substitute_type_id(
        field,
        &substitution,
        interner,
    ))
}

#[cfg(test)]
mod tests;
