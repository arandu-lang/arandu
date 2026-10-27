use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use crate::SymbolId;
use arandu_middle::layout::StructFields;
use arandu_middle::types::ReturnBorrowSummary;
use arandu_parser::ast_pool::ExprId;

use super::types::{self, ArType, TypeId, TypeInterner};

#[cfg(test)]
use super::types::Primitive;
#[cfg(test)]
use arandu_middle::layout::StructFieldInfo;
#[cfg(test)]
use arandu_middle::types::BorrowKind;

mod merge;
mod query;
mod translate;

pub use merge::ItemPlacement;
pub use translate::{import_thread_captures, translate_type};

#[derive(Debug, Clone, PartialEq)]
pub enum EnumPayloadShape {
    Unit,
    /// Payload element types as interned ids (no owned `ArType` trees).
    Tuple(Vec<TypeId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceConstraint {
    pub iface_sym: SymbolId,
    pub type_args: SmallVec<[TypeId; 2]>,
}

/// One owned local moved into a synthesized `ThreadJob`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCaptureField {
    pub name: smol_str::SmolStr,
    pub symbol: SymbolId,
    pub source: SymbolId,
    pub ty: TypeId,
    pub span: arandu_middle::Span,
}

/// `move ||` with no parameters, lowered as a capture struct plus `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCaptureRecipe {
    pub lambda_span: arandu_middle::Span,
    pub struct_symbol: SymbolId,
    pub run_symbol: SymbolId,
    pub self_param: SymbolId,
    pub arg_param: SymbolId,
    pub fields: Vec<ThreadCaptureField>,
    pub return_type: TypeId,
    /// Backend name. The source method is `run`; this spelling has no dots.
    pub host_name: smol_str::SmolStr,
}

impl ThreadCaptureRecipe {
    #[must_use]
    pub fn struct_symbol_name(&self) -> String {
        format!(
            "__ThreadCapture_{}_{}",
            self.lambda_span.start, self.lambda_span.end
        )
    }
}

/// One concrete declaration contract in this interner's domain. Templates
/// retain their own metadata; distinct structural instance keys never overwrite it.
#[derive(Debug, Clone)]
pub struct ConcreteHeader {
    pub signature: TypeId,
    pub fields: Option<Arc<StructFields>>,
    pub variants: Vec<(SymbolId, usize, EnumPayloadShape)>,
}

/// Shared metadata maps use `Arc` so `merge_from` (item body typeck fold) is O(1)
/// per entry instead of deep-cloning every field map / generic param list.
#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub target_identity: arandu_middle::db::TargetIdentity,
    pub target_pointer_width: u8,
    pub target_layout: arandu_middle::DataLayout,
    pub type_interner: TypeInterner,
    pub expr_types: Vec<Option<TypeId>>,
    /// Source-local obligations recorded only when checking explicit staging.
    pub ctfe_roots: Vec<ExprId>,
    /// Frozen roots by full source span, not revision-local arena IDs. Item
    /// memos may retain an older AST after a sibling edit; file composition and
    /// imported contexts must not confuse those pools. No VM handles escape.
    pub ctfe_values: FxHashMap<
        arandu_middle::Span,
        (
            arandu_middle::ctfe::ConstValue,
            arandu_middle::layout::DataLayout,
        ),
    >,
    /// Exportable values keyed by source identity, without initializer spans.
    pub ctfe_global_values: FxHashMap<SymbolId, arandu_middle::ctfe::ConstValue>,
    pub decl_types: FxHashMap<SymbolId, TypeId>,
    pub deferred_headers: FxHashSet<SymbolId>,
    pub concrete_headers: FxHashMap<arandu_middle::types::FunctionInstance, Arc<ConcreteHeader>>,
    /// Pure discovery output, consumed by the query continuation before publication.
    pub header_requests: FxHashMap<arandu_middle::types::FunctionInstance, arandu_middle::Span>,
    /// Canonical flow-derived borrow interfaces published across item/module boundaries.
    pub return_borrow_summaries: FxHashMap<SymbolId, ReturnBorrowSummary>,
    /// Struct field table: declaration order + name index (type/symbol/index folded in).
    pub struct_fields: FxHashMap<SymbolId, Arc<StructFields>>,
    /// Field symbols declared with `private` visibility.
    pub private_fields: FxHashSet<SymbolId>,
    /// Canonical interior-mutable owners must never gain structural Copy.
    pub interior_mutable_types: FxHashSet<SymbolId>,
    pub enum_variants: FxHashMap<SymbolId, (SymbolId, EnumPayloadShape)>,
    /// Pre-computed discriminant tag for each enum variant symbol.
    pub enum_variant_tags: FxHashMap<SymbolId, usize>,
    /// Receiver type symbol → the unique method carrying `@Destructor`.
    ///
    /// The association is semantic metadata, not a naming convention: a
    /// method named `destroy` has no special meaning without the annotation.
    pub destructors: FxHashMap<SymbolId, SymbolId>,
    /// Concrete receiver `TypeId` → monomorphic destructor function.
    pub destructor_instances: FxHashMap<TypeId, SymbolId>,
    /// Ordered type-parameter symbols for generic decls (func, struct, …).
    pub generic_params: FxHashMap<SymbolId, Arc<Vec<SymbolId>>>,
    /// T2.1: type-parameter symbol → default type (`A = GlobalAllocator`).
    pub generic_defaults: FxHashMap<SymbolId, TypeId>,
    /// Type-parameter symbol → interface constraints required (`T: Display`, `I: Iterator<Item>`).
    pub param_constraints: FxHashMap<SymbolId, Arc<Vec<InterfaceConstraint>>>,
    /// Interface symbol → method signatures (nominal, Go-style structural check).
    pub interfaces: FxHashMap<SymbolId, types::InterfaceInfo>,
    /// Cache of instantiated variant/method signatures to avoid redundant substitution.
    pub variant_instantiations: FxHashMap<(SymbolId, Vec<TypeId>), (Vec<TypeId>, TypeId)>,
    /// Declared and inferred effects per function symbol.
    pub function_effects: FxHashMap<SymbolId, arandu_middle::EffectFlags>,
    /// Functions whose API contract requires callers to establish unsafe
    /// preconditions in an explicit `unsafe` block (`@Unsafe`).
    pub unsafe_functions: FxHashSet<SymbolId>,
    /// Struct symbols that explicitly declare `@Repr("C")` / `#[repr(C)]`.
    pub struct_repr_c: rustc_hash::FxHashSet<SymbolId>,
    /// Struct field default value expressions (`(struct_symbol, field_name) -> default_expr`).
    pub struct_field_defaults: FxHashMap<(SymbolId, smol_str::SmolStr), ExprId>,
    /// Thread-closure capture jobs keyed by the lambda span. Empty unless a
    /// `move ||` was accepted as a `ThreadJob`.
    pub thread_captures: FxHashMap<arandu_middle::Span, ThreadCaptureRecipe>,
}

impl Default for TypeInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeInfo {
    pub const MAX_BORROW_PATH_DEPTH: usize = arandu_middle::types::MAX_BORROW_PATH_DEPTH;
    pub const MAX_BORROW_PATHS: usize = arandu_middle::types::MAX_BORROW_PATHS;

    #[must_use]
    pub fn new() -> Self {
        Self::with_interner(TypeInterner::new())
    }

    #[must_use]
    pub fn with_interner(type_interner: TypeInterner) -> Self {
        Self {
            target_identity: arandu_middle::db::TargetIdentity::default(),
            target_pointer_width: 0,
            target_layout: arandu_middle::DataLayout::ptr_width(8),
            type_interner,
            expr_types: Vec::new(),
            ctfe_roots: Vec::new(),
            ctfe_values: FxHashMap::default(),
            ctfe_global_values: FxHashMap::default(),
            decl_types: FxHashMap::default(),
            deferred_headers: FxHashSet::default(),
            concrete_headers: FxHashMap::default(),
            header_requests: FxHashMap::default(),
            return_borrow_summaries: FxHashMap::default(),
            struct_fields: FxHashMap::default(),
            private_fields: FxHashSet::default(),
            interior_mutable_types: FxHashSet::default(),
            enum_variants: FxHashMap::default(),
            enum_variant_tags: FxHashMap::default(),
            destructors: FxHashMap::default(),
            destructor_instances: FxHashMap::default(),
            generic_params: FxHashMap::default(),
            generic_defaults: FxHashMap::default(),
            param_constraints: FxHashMap::default(),
            interfaces: FxHashMap::default(),
            variant_instantiations: FxHashMap::default(),
            function_effects: FxHashMap::default(),
            unsafe_functions: FxHashSet::default(),
            struct_repr_c: rustc_hash::FxHashSet::default(),
            struct_field_defaults: FxHashMap::default(),
            thread_captures: FxHashMap::default(),
        }
    }

    #[must_use]
    pub fn thread_capture_at(&self, span: arandu_middle::Span) -> Option<&ThreadCaptureRecipe> {
        self.thread_captures.get(&span)
    }

    #[must_use]
    pub fn thread_capture_for_run(&self, symbol: SymbolId) -> Option<&ThreadCaptureRecipe> {
        self.thread_captures
            .values()
            .find(|recipe| recipe.run_symbol == symbol)
    }

    /// Re-register span-derived capture symbols on a signature table.
    ///
    /// Item body checks already hold them. File composition keeps the
    /// signature table, so this runs only when a recipe was recorded.
    pub fn materialize_thread_captures(&self, symbols: &mut crate::SymbolTable) {
        if self.thread_captures.is_empty() {
            return;
        }
        let scope = symbols.global_scope();
        for recipe in self.thread_captures.values() {
            if recipe.struct_symbol.file_id != symbols.file_id {
                continue;
            }
            Self::ensure_capture_symbol(
                symbols,
                recipe.struct_symbol,
                &recipe.struct_symbol_name(),
                arandu_middle::SymbolKind::Struct,
                recipe.lambda_span,
                scope,
            );
            Self::ensure_capture_symbol(
                symbols,
                recipe.run_symbol,
                &format!("{}.run", recipe.struct_symbol_name()),
                arandu_middle::SymbolKind::AssociatedFunc,
                recipe.lambda_span,
                scope,
            );
            Self::ensure_capture_symbol(
                symbols,
                recipe.self_param,
                "self",
                arandu_middle::SymbolKind::Param,
                recipe.lambda_span,
                scope,
            );
            Self::ensure_capture_symbol(
                symbols,
                recipe.arg_param,
                "argument",
                arandu_middle::SymbolKind::Param,
                recipe.lambda_span,
                scope,
            );
            for field in &recipe.fields {
                Self::ensure_capture_symbol(
                    symbols,
                    field.symbol,
                    &field.name,
                    arandu_middle::SymbolKind::Field,
                    field.span,
                    scope,
                );
            }
            symbols
                .associated_members
                .insert((recipe.struct_symbol, "run".into()), recipe.run_symbol);
            symbols
                .host_function_names
                .insert(recipe.run_symbol, recipe.host_name.clone());
        }
    }

    fn ensure_capture_symbol(
        symbols: &mut crate::SymbolTable,
        id: SymbolId,
        name: &str,
        kind: arandu_middle::SymbolKind,
        span: arandu_middle::Span,
        scope: crate::ScopeId,
    ) {
        if symbols.try_get(id).is_some() {
            return;
        }
        symbols.register_synthetic(arandu_middle::Symbol {
            id,
            name: name.into(),
            kind,
            span,
            scope,
            visibility: arandu_middle::Visibility::Public,
            lang_item: None,
        });
    }

    #[must_use]
    pub fn header_key(
        &self,
        owner: SymbolId,
        args: &[TypeId],
    ) -> Option<arandu_middle::types::FunctionInstance> {
        use arandu_middle::types::{FunctionInstance, TypeShape};
        let arguments = args
            .iter()
            .map(|&id| TypeShape::from_id(id, &self.type_interner))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if !arguments
            .iter()
            .all(|argument| self.is_closed_shape(argument))
        {
            return None;
        }
        Some(FunctionInstance {
            definition: owner,
            arguments,
        })
    }

    #[must_use]
    pub fn is_closed_shape(&self, shape: &arandu_middle::types::TypeShape) -> bool {
        if !shape.is_concrete() {
            return false;
        }
        let mut open = false;
        let bounded = shape.for_each_symbol(|symbol| {
            open |= self
                .generic_params
                .values()
                .any(|params| params.contains(&symbol));
        });
        bounded.is_ok() && !open
    }
}

pub use arandu_middle::types::BorrowShapeError;

impl TypeInfo {
    pub fn record_expr_type(&mut self, expr: ExprId, id: TypeId) {
        let idx = expr.as_usize();
        if self.expr_types.len() <= idx {
            self.expr_types.resize(idx + 1, None);
        }
        self.expr_types[idx] = Some(id);
    }

    pub fn record_decl_type(&mut self, symbol: SymbolId, id: TypeId) {
        self.decl_types.insert(symbol, id);
    }

    #[must_use]
    pub fn expr_type(&self, expr: ExprId) -> Option<ArType> {
        self.expr_types
            .get(expr.as_usize())
            .and_then(|id| id.as_ref().map(|id| self.type_interner.resolve(*id)))
    }

    #[must_use]
    pub fn expr_type_id(&self, expr: ExprId) -> Option<TypeId> {
        self.expr_types.get(expr.as_usize()).copied().flatten()
    }

    #[must_use]
    pub fn decl_type(&self, symbol: SymbolId) -> Option<ArType> {
        self.decl_types
            .get(&symbol)
            .map(|id| self.type_interner.resolve(*id))
    }

    #[must_use]
    pub fn decl_type_id(&self, symbol: SymbolId) -> Option<TypeId> {
        self.decl_types.get(&symbol).copied()
    }

    #[must_use]
    pub fn resolve_type_id(&self, id: TypeId) -> ArType {
        self.type_interner.resolve(id)
    }
}

impl arandu_middle::layout::StructLayoutProvider for TypeInfo {
    fn get_struct_fields(
        &self,
        struct_id: SymbolId,
    ) -> Option<&arandu_middle::layout::StructFields> {
        self.struct_fields.get(&struct_id).map(|a| a.as_ref())
    }

    fn get_struct_fields_for_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
    ) -> Option<&StructFields> {
        let ArType::Named(symbol, args) = ty else {
            return None;
        };
        if !self.deferred_headers.contains(symbol) {
            return self.get_struct_fields(*symbol);
        }
        let args = interner.type_args(*args);
        let key = arandu_middle::types::FunctionInstance {
            definition: *symbol,
            arguments: args
                .iter()
                .map(|&id| arandu_middle::types::TypeShape::from_id(id, interner))
                .collect::<Result<Vec<_>, _>>()
                .ok()?,
        };
        self.concrete_headers
            .get(&key)
            .and_then(|header| header.fields.as_deref())
            .or_else(|| self.get_struct_fields(*symbol))
    }

    fn get_enum_variants_for_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
    ) -> Option<Vec<arandu_middle::layout::EnumPayloadShape>> {
        let ArType::Named(symbol, args) = ty else {
            return None;
        };
        if !self.deferred_headers.contains(symbol) {
            return self.get_enum_variants(*symbol);
        }
        let key = arandu_middle::types::FunctionInstance {
            definition: *symbol,
            arguments: interner
                .type_args(*args)
                .iter()
                .map(|&id| arandu_middle::types::TypeShape::from_id(id, interner))
                .collect::<Result<Vec<_>, _>>()
                .ok()?,
        };
        if let Some(header) = self.concrete_headers.get(&key)
            && !header.variants.is_empty()
        {
            return Some(
                header
                    .variants
                    .iter()
                    .map(|(_, _, payload)| arandu_middle::layout::EnumPayloadShape {
                        payload_ty: match payload {
                            EnumPayloadShape::Unit => None,
                            EnumPayloadShape::Tuple(items) if items.len() == 1 => Some(items[0]),
                            EnumPayloadShape::Tuple(items) if items.is_empty() => None,
                            EnumPayloadShape::Tuple(items) => {
                                Some(self.type_interner.intern(ArType::Tuple(
                                    self.type_interner.push_type_args(items),
                                )))
                            }
                        },
                    })
                    .collect(),
            );
        }
        self.get_enum_variants(*symbol)
    }

    fn get_generic_params(&self, struct_id: SymbolId) -> Option<&[SymbolId]> {
        self.generic_params.get(&struct_id).map(|v| v.as_slice())
    }

    fn get_enum_variant_symbol(&self, enum_id: SymbolId, tag: usize) -> Option<SymbolId> {
        self.enum_variants
            .iter()
            .filter(|(symbol, (owner, _))| {
                *owner == enum_id && self.enum_variant_tags.get(symbol) == Some(&tag)
            })
            .map(|(symbol, _)| *symbol)
            .min_by_key(|symbol| (symbol.file_id, symbol.local_id.0))
    }

    fn get_enum_variants(
        &self,
        enum_id: SymbolId,
    ) -> Option<Vec<arandu_middle::layout::EnumPayloadShape>> {
        let mut variant_list: Vec<(usize, &EnumPayloadShape)> = self
            .enum_variants
            .iter()
            .filter(|(_var_symbol, (parent_enum_id, _shape))| *parent_enum_id == enum_id)
            .map(|(var_symbol, (_parent, shape))| {
                let tag = self.enum_variant_tags.get(var_symbol).copied().unwrap_or(0);
                (tag, shape)
            })
            .collect();

        if variant_list.is_empty() {
            return None;
        }

        variant_list.sort_by_key(|(tag, _shape)| *tag);
        variant_list.dedup_by_key(|(tag, _shape)| *tag);

        let mut mapped_variants = Vec::new();
        for (_tag, shape) in variant_list {
            let payload_ty = match shape {
                EnumPayloadShape::Unit => None,
                EnumPayloadShape::Tuple(tids) => {
                    if tids.is_empty() {
                        None
                    } else if tids.len() == 1 {
                        Some(tids[0])
                    } else {
                        // Multi-payload: retain a concrete tuple even for unselected variants.
                        let range = self.type_interner.push_type_args(tids);
                        Some(self.type_interner.intern(ArType::Tuple(range)))
                    }
                }
            };
            mapped_variants.push(arandu_middle::layout::EnumPayloadShape { payload_ty });
        }

        Some(mapped_variants)
    }

    fn is_copy_type(&self, ty: TypeId) -> Option<bool> {
        Some(self.is_copy(ty))
    }

    fn destructor_for_type(&self, ty: TypeId) -> Option<SymbolId> {
        self.destructor_instances.get(&ty).copied()
    }

    fn is_repr_c(&self, struct_id: SymbolId) -> bool {
        self.struct_repr_c.contains(&struct_id)
    }
}

impl arandu_middle::type_context::AmirTypeContext for TypeInfo {
    fn type_interner(&self) -> &TypeInterner {
        &self.type_interner
    }

    fn enum_variant_payloads(
        &self,
        owner: SymbolId,
        args: &[TypeId],
    ) -> Vec<arandu_middle::type_context::EnumVariantPayload<'_>> {
        self.enum_variants
            .iter()
            .filter(|(_, (parent, _))| *parent == owner)
            .filter_map(|(&variant, _)| {
                let payload = match self.variant_payload_for(variant, args)? {
                    EnumPayloadShape::Unit => None,
                    EnumPayloadShape::Tuple(types) => Some(types.as_slice()),
                };
                Some(arandu_middle::type_context::EnumVariantPayload {
                    variant,
                    tag: self.enum_variant_tags.get(&variant).copied(),
                    payload,
                })
            })
            .collect()
    }

    fn borrow_paths(
        &self,
        ty: TypeId,
    ) -> Result<
        Vec<(
            arandu_middle::types::BorrowPath,
            arandu_middle::types::BorrowKind,
        )>,
        BorrowShapeError,
    > {
        TypeInfo::borrow_paths(self, ty)
    }

    fn enum_variant_tag(&self, variant: SymbolId) -> Option<usize> {
        self.enum_variant_tags.get(&variant).copied()
    }

    fn return_borrow_summaries(&self) -> &FxHashMap<SymbolId, ReturnBorrowSummary> {
        &self.return_borrow_summaries
    }

    fn contains_explicit_reference(&self, ty: TypeId) -> bool {
        TypeInfo::contains_explicit_reference(self, ty)
    }

    fn is_destructor_instance(&self, function: SymbolId) -> bool {
        self.destructor_instances
            .values()
            .any(|symbol| *symbol == function)
    }
}

impl arandu_middle::type_context::CodegenTypeContext for TypeInfo {
    fn target_identity(&self) -> &arandu_middle::db::TargetIdentity {
        &self.target_identity
    }

    fn target_layout(&self) -> arandu_middle::DataLayout {
        self.target_layout
    }

    fn decl_type_id(&self, symbol: SymbolId) -> Option<TypeId> {
        self.decl_types.get(&symbol).copied()
    }

    fn function_effects(&self, function: SymbolId) -> arandu_middle::EffectFlags {
        self.function_effects
            .get(&function)
            .copied()
            .unwrap_or_default()
    }

    fn destructor_method(&self, receiver: SymbolId) -> Option<SymbolId> {
        self.destructors.get(&receiver).copied()
    }

    fn fields_for(&self, owner: SymbolId, args: &[TypeId]) -> Option<&StructFields> {
        TypeInfo::fields_for(self, owner, args)
    }
}

#[cfg(test)]
mod borrow_shape_tests {
    use super::*;
    use arandu_middle::types::{BorrowPath, BorrowPathSegment};

    #[test]
    fn structural_borrow_paths_cover_carriers_and_terminate_recursive_types() {
        let mut info = TypeInfo::new();
        let int = info.type_interner.intern(ArType::Primitive(Primitive::Int));
        let shared = info.type_interner.intern(ArType::Ref(int));
        let exclusive = info.type_interner.intern(ArType::RefMut(int));
        let option_shared = info.type_interner.intern(ArType::Option(shared));

        let record = SymbolId::new(0, 10);
        let record_type =
            info.type_interner
                .intern(ArType::named(record, &[], &info.type_interner));
        info.struct_fields.insert(
            record,
            Arc::new(StructFields::from_entries([
                StructFieldInfo {
                    name: "next".into(),
                    symbol: Some(SymbolId::new(0, 1)),
                    ty: record_type,
                    index: 0,
                },
                StructFieldInfo {
                    name: "read".into(),
                    symbol: Some(SymbolId::new(0, 2)),
                    ty: option_shared,
                    index: 1,
                },
                StructFieldInfo {
                    name: "write".into(),
                    symbol: Some(SymbolId::new(0, 3)),
                    ty: exclusive,
                    index: 2,
                },
            ])),
        );

        let paths = info.borrow_paths(record_type).expect("bounded shape");
        assert!(info.contains_explicit_reference(record_type));
        assert_eq!(
            paths,
            vec![
                (
                    BorrowPath(
                        vec![
                            BorrowPathSegment::Field("read".into()),
                            BorrowPathSegment::OptionSome,
                        ]
                        .into()
                    ),
                    BorrowKind::Shared,
                ),
                (
                    BorrowPath(vec![BorrowPathSegment::Field("write".into())].into()),
                    BorrowKind::Exclusive,
                ),
            ]
        );

        let choice = SymbolId::new(0, 20);
        let variant = SymbolId::new(0, 21);
        info.enum_variants
            .insert(variant, (choice, EnumPayloadShape::Tuple(vec![shared])));
        info.enum_variant_tags.insert(variant, 7);
        let choice_type =
            info.type_interner
                .intern(ArType::named(choice, &[], &info.type_interner));
        assert_eq!(
            info.borrow_paths(choice_type).expect("enum shape"),
            vec![(
                BorrowPath(
                    vec![BorrowPathSegment::Variant(7), BorrowPathSegment::Payload(0),].into()
                ),
                BorrowKind::Shared,
            )]
        );
    }

    #[test]
    fn slice_is_a_copyable_shared_borrow_at_its_root() {
        let info = TypeInfo::new();
        let int = info.type_interner.intern(ArType::Primitive(Primitive::Int));
        let slice = info.type_interner.intern(ArType::Slice(int));

        assert_eq!(
            info.borrow_paths(slice).expect("slice shape"),
            vec![(BorrowPath::root(), BorrowKind::Shared)]
        );
        assert!(info.is_copy(slice));
        assert!(!info.contains_explicit_reference(slice));
    }

    #[test]
    fn mut_slice_is_a_move_only_exclusive_borrow_at_its_root() {
        let info = TypeInfo::new();
        let int = info.type_interner.intern(ArType::Primitive(Primitive::Int));
        let mut_slice = info.type_interner.intern(ArType::MutSlice(int));

        assert_eq!(
            info.borrow_paths(mut_slice).expect("mut slice shape"),
            vec![(BorrowPath::root(), BorrowKind::Exclusive)]
        );
        assert!(!info.is_copy(mut_slice));
        assert!(!info.contains_explicit_reference(mut_slice));
    }

    #[test]
    fn plain_str_does_not_claim_an_unconditional_borrow_origin() {
        let info = TypeInfo::new();
        let str_view = info.type_interner.intern(ArType::Primitive(Primitive::Str));

        assert!(info.borrow_paths(str_view).expect("str shape").is_empty());
        assert!(info.is_copy(str_view));
    }

    #[test]
    fn structural_borrow_paths_fail_closed_at_the_published_depth_bound() {
        let info = TypeInfo::new();
        let int = info.type_interner.intern(ArType::Primitive(Primitive::Int));
        let mut nested = info.type_interner.intern(ArType::Ref(int));
        for _ in 0..=TypeInfo::MAX_BORROW_PATH_DEPTH {
            nested = info.type_interner.intern(ArType::Option(nested));
        }

        assert_eq!(
            info.borrow_paths(nested),
            Err(BorrowShapeError::DepthExceeded)
        );
    }
}

#[cfg(test)]
mod type_context_tests {
    use super::*;
    use arandu_middle::type_context::{AmirTypeContext, EnumVariantPayload};

    #[test]
    fn type_context_reports_variants_tags_and_borrow_freedom() {
        let mut info = TypeInfo::new();
        let int = info.type_interner.intern(ArType::Primitive(Primitive::Int));
        let shared = info.type_interner.intern(ArType::Ref(int));
        let owner = SymbolId::new(0, 1);
        let unit = SymbolId::new(0, 2);
        let tuple = SymbolId::new(0, 3);
        let foreign = SymbolId::new(0, 4);
        info.enum_variants
            .insert(unit, (owner, EnumPayloadShape::Unit));
        info.enum_variants
            .insert(tuple, (owner, EnumPayloadShape::Tuple(vec![int])));
        info.enum_variants
            .insert(foreign, (SymbolId::new(0, 9), EnumPayloadShape::Unit));
        info.enum_variant_tags.insert(unit, 0);
        info.enum_variant_tags.insert(tuple, 1);

        let mut variants = info.enum_variant_payloads(owner, &[]);
        variants.sort_by_key(|variant| variant.tag);
        assert_eq!(
            variants,
            [
                EnumVariantPayload {
                    variant: unit,
                    tag: Some(0),
                    payload: None,
                },
                EnumVariantPayload {
                    variant: tuple,
                    tag: Some(1),
                    payload: Some(&[int][..]),
                },
            ]
        );
        assert_eq!(info.enum_variant_tag(foreign), None);
        assert!(info.is_borrow_free(int));
        assert!(!info.is_borrow_free(shared));
    }
}
