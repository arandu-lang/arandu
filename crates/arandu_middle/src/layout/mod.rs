pub mod abi;
mod data_layout;
pub mod export_abi;

pub use abi::{
    AbiConfigError, AbiScalar, AbiSlot, ArgAbi, DirectAbi, TargetAbi, TargetAbiClassifier,
};
pub use data_layout::{DataLayout, DataLayoutError, SizeAlign};
pub use export_abi::{
    AbiLane, AbiPart, ExportAbi, ExportAbiInfo, ParamAbi, ReturnAbi, classify as classify_export,
    is_export_aggregate,
};

use crate::SymbolId;
use crate::index_vec::IdIndex;
use crate::types::{ArType, TypeId, TypeInterner};
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

    /// Builds a range when both ends fit in `u32` and their sum does not wrap.
    #[must_use]
    pub const fn try_new(start: usize, len: usize) -> Option<Self> {
        if start > u32::MAX as usize || len > u32::MAX as usize {
            return None;
        }
        let start = start as u32;
        let len = len as u32;
        if start.checked_add(len).is_none() {
            return None;
        }
        Some(Self { start, len })
    }

    /// Builds a range.
    ///
    /// # Panics
    ///
    /// Panics if `start` or `len` does not fit in `u32`, or if `start + len`
    /// overflows. Callers that can observe a malformed range use [`Self::try_new`].
    #[must_use]
    #[allow(clippy::panic)] // Infallible constructor; malformed ranges use try_new.
    pub const fn new(start: usize, len: usize) -> Self {
        match Self::try_new(start, len) {
            Some(range) => range,
            None => panic!("DenseRange exceeds u32"),
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
    UnresolvedConst,
    /// Nominal metadata is missing; it is not a zero-sized type.
    UnknownType,
    /// Shared structural limit, including recursive by-value definitions.
    StructuralLimit,
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
            Self::UnresolvedConst => {
                f.write_str("const generic reached layout before monomorphization")
            }
            Self::UnknownType => f.write_str("type is unresolved or has no layout metadata"),
            Self::StructuralLimit => {
                f.write_str("type is recursive by value or exceeds the structural layout limit")
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

    /// Concrete headers override the template metadata without changing its identity.
    fn get_struct_fields_for_type(
        &self,
        ty: &ArType,
        _interner: &TypeInterner,
    ) -> Option<&StructFields> {
        let ArType::Named(symbol, _) = ty else {
            return None;
        };
        self.get_struct_fields(*symbol)
    }

    fn get_enum_variants_for_type(
        &self,
        ty: &ArType,
        _interner: &TypeInterner,
    ) -> Option<Vec<EnumPayloadShape>> {
        let ArType::Named(symbol, _) = ty else {
            return None;
        };
        self.get_enum_variants(*symbol)
    }

    /// Source identity for a nominal variant. Layout-only providers can omit it.
    fn get_enum_variant_symbol(&self, _enum_id: SymbolId, _tag: usize) -> Option<SymbolId> {
        None
    }

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
/// Language `float` is always IEEE f64 (see [`DataLayout`]); `int` and `uint`
/// are fixed 32-bit. i686 uses [`DataLayout::i686_sysv`] for i64/f64
/// abi_align=4.
#[derive(Debug, Clone)]
pub struct LayoutEngine {
    pub data_layout: DataLayout,
}

/// One bounded traversal shared by layout and niche queries. Use the existing
/// structural type limits, rather than resetting the budget in recursive calls.
struct LayoutWalk {
    remaining: usize,
}

impl LayoutWalk {
    fn new() -> Self {
        Self {
            remaining: crate::types::TypeShape::MAX_NODES,
        }
    }

    fn visit(&mut self, depth: usize) -> Result<(), LayoutError> {
        if depth >= crate::types::TypeShape::MAX_DEPTH || self.remaining == 0 {
            return Err(LayoutError::StructuralLimit);
        }
        self.remaining -= 1;
        Ok(())
    }
}

mod engine;
mod type_layout;

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
        ArType::MutSlice(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::MutSlice(new_inner)
        }
        ArType::Array(len, inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Array(*len, new_inner)
        }
        ArType::ConstArray(param, inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.intern(substituted_inner);
            match subst.get(param).map(|&id| interner.resolve(id)) {
                Some(ArType::Const(length)) => ArType::Array(length, new_inner),
                _ => ArType::ConstArray(*param, new_inner),
            }
        }
        ArType::ConstParam(param) => subst
            .get(param)
            .map_or_else(|| ty.clone(), |&id| interner.resolve(id)),
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
    let field = provider
        .get_struct_fields_for_type(owner, interner)?
        .get(field_name)?
        .ty;
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

/// Resolve an enum variant's payload type through the concrete generic arguments
/// on `owner`. Returns `None` for unit variants, unavailable metadata, or arity mismatch.
#[must_use]
pub fn instantiated_enum_variant_payload_type(
    owner: &ArType,
    variant_tag: usize,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> Option<TypeId> {
    let ArType::Named(symbol, arguments) = owner else {
        return None;
    };
    let variants = provider.get_enum_variants_for_type(owner, interner)?;
    let payload = variants.get(variant_tag)?.payload_ty?;
    let parameters = provider.get_generic_params(*symbol).unwrap_or(&[]);
    let arguments = interner.type_args(*arguments);
    if parameters.is_empty() || arguments.is_empty() {
        return Some(payload);
    }
    if parameters.len() != arguments.len() {
        return None;
    }
    let substitution = crate::types::build_subst_ids(parameters, &arguments, interner);
    Some(crate::types::substitute_type_id(
        payload,
        &substitution,
        interner,
    ))
}

#[cfg(test)]
mod tests;
