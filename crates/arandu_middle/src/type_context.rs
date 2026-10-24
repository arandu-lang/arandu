//! Read-only type facts that consumers of type checking read.
//!
//! Typeck owns the implementation. AMIR passes depend on [`AmirTypeContext`];
//! backends depend on [`CodegenTypeContext`], which extends it. Neither ever
//! reaches the front-end phases or their internal representations.

use rustc_hash::FxHashMap;

use crate::db::TargetIdentity;
use crate::effects::EffectFlags;
use crate::layout::{DataLayout, StructFields, StructLayoutProvider};
use crate::symbol_table::SymbolId;
use crate::types::{
    ArType, BorrowKind, BorrowPath, BorrowShapeError, ReturnBorrowSummary, TypeId, TypeInterner,
};

/// One variant of a nominal enum instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumVariantPayload<'a> {
    pub variant: SymbolId,
    /// Discriminant assigned by typeck; `None` when the variant has no tag.
    pub tag: Option<usize>,
    /// `None` for a unit variant, otherwise the tuple payload types.
    pub payload: Option<&'a [TypeId]>,
}

/// Facts the AMIR analyses and transforms need about checked types.
pub trait AmirTypeContext: StructLayoutProvider {
    fn type_interner(&self) -> &TypeInterner;

    /// Variants of the enum `owner` instantiated with `args`, in unspecified order.
    fn enum_variant_payloads(
        &self,
        owner: SymbolId,
        args: &[TypeId],
    ) -> Vec<EnumVariantPayload<'_>>;

    /// Every safe-borrow leaf of `ty` in canonical order, or why the bounded
    /// contract cannot be published.
    fn borrow_paths(&self, ty: TypeId) -> Result<Vec<(BorrowPath, BorrowKind)>, BorrowShapeError>;

    fn enum_variant_tag(&self, variant: SymbolId) -> Option<usize>;

    /// Return-borrow contracts already published for signatures, keyed by function.
    fn return_borrow_summaries(&self) -> &FxHashMap<SymbolId, ReturnBorrowSummary>;

    /// Whether `ty` structurally contains an explicit `ref`/`mut ref`.
    fn contains_explicit_reference(&self, ty: TypeId) -> bool;

    /// Whether `function` is the `@Destructor` instance of some type.
    fn is_destructor_instance(&self, function: SymbolId) -> bool;

    fn resolve_type_id(&self, id: TypeId) -> ArType {
        self.type_interner().resolve(id)
    }

    /// True only when the structural shape of `ty` is bounded and contains no
    /// borrowed leaf, so its bytes may outlive the borrows of one invocation.
    fn is_borrow_free(&self, ty: TypeId) -> bool {
        self.borrow_paths(ty).is_ok_and(|paths| paths.is_empty())
    }
}

/// Facts a codegen backend additionally needs about the checked program.
pub trait CodegenTypeContext: AmirTypeContext {
    fn target_identity(&self) -> &TargetIdentity;

    fn target_layout(&self) -> DataLayout;

    fn decl_type_id(&self, symbol: SymbolId) -> Option<TypeId>;

    fn function_effects(&self, function: SymbolId) -> EffectFlags;

    /// The `@Destructor` method declared on a nominal receiver type.
    fn destructor_method(&self, receiver: SymbolId) -> Option<SymbolId>;

    /// Field table of `owner`, preferring the concrete header for `args`.
    fn fields_for(&self, owner: SymbolId, args: &[TypeId]) -> Option<&StructFields>;

    fn decl_type(&self, symbol: SymbolId) -> Option<ArType> {
        self.decl_type_id(symbol).map(|id| self.resolve_type_id(id))
    }
}
