use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use crate::SymbolId;
use arandu_middle::layout::{StructFieldInfo, StructFields};
use arandu_middle::types::{BorrowKind, BorrowPath, BorrowPathSegment, ReturnBorrowSummary};
use arandu_parser::ast_pool::ExprId;

use super::types::{
    self, ArType, Primitive, TypeId, TypeInterner, build_subst_ids, substitute_type,
};

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
}

impl Default for TypeInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeInfo {
    /// Maximum nesting published in a borrow interface. This bound is part of
    /// the compiler contract, not a recursion guard chosen by the host stack.
    pub const MAX_BORROW_PATH_DEPTH: usize = 32;
    /// Maximum borrowed leaves published for one type.
    pub const MAX_BORROW_PATHS: usize = 256;

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
        }
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

    pub fn demand_header(
        &mut self,
        owner: SymbolId,
        args: &[TypeId],
        span: arandu_middle::Span,
    ) -> Option<TypeId> {
        if !self.deferred_headers.contains(&owner) {
            return None;
        }
        let key = self.header_key(owner, args)?;
        if let Some(contract) = self.concrete_headers.get(&key) {
            return Some(contract.signature);
        }
        self.header_requests.entry(key).or_insert(span);
        None
    }

    /// Pure discovery from an already inferred type. Query orchestration fills
    /// these demands before publishing the final body or isolated root.
    pub fn demand_type(&mut self, ty: TypeId, span: arandu_middle::Span) {
        if self.deferred_headers.is_empty() {
            return;
        }
        let mut pending = vec![ty];
        let mut remaining = arandu_middle::types::TypeShape::MAX_NODES;
        while let Some(ty) = pending.pop() {
            if remaining == 0 {
                break;
            }
            remaining -= 1;
            match self.type_interner.resolve(ty) {
                ArType::Named(owner, args) => {
                    let args = self.type_interner.type_args(args);
                    self.demand_header(owner, &args, span);
                    pending.extend(args);
                }
                ArType::Func(args, result) => {
                    pending.extend(self.type_interner.type_args(args));
                    pending.push(result);
                }
                ArType::Tuple(args) => pending.extend(self.type_interner.type_args(args)),
                ArType::Array(_, inner)
                | ArType::ConstArray(_, inner)
                | ArType::Slice(inner)
                | ArType::Ref(inner)
                | ArType::RefMut(inner)
                | ArType::Ptr(inner)
                | ArType::Option(inner)
                | ArType::Nullable(inner)
                | ArType::Poll(inner)
                | ArType::Coroutine(inner)
                | ArType::Range(inner) => pending.push(inner),
                ArType::Result(ok, error) => {
                    pending.push(ok);
                    pending.push(error);
                }
                ArType::Primitive(_)
                | ArType::Const(_)
                | ArType::FrozenConst(_)
                | ArType::ConstParam(_)
                | ArType::GenRef
                | ArType::Err
                | ArType::Void
                | ArType::Error
                | ArType::IntLiteral
                | ArType::FloatLiteral => {}
            }
        }
    }

    #[must_use]
    pub fn fields_for(&self, owner: SymbolId, args: &[TypeId]) -> Option<&StructFields> {
        self.fields_arc_for(owner, args).map(Arc::as_ref)
    }

    #[must_use]
    pub fn field_type_for(&self, owner: SymbolId, args: &[TypeId], name: &str) -> Option<TypeId> {
        let field = self.fields_for(owner, args)?.get(name)?;
        if let Some(EnumPayloadShape::Tuple(items)) = self.variant_payload_for(owner, args) {
            return items.get(field.index).copied();
        }
        Some(field.ty)
    }

    #[must_use]
    pub fn fields_arc_for(&self, owner: SymbolId, args: &[TypeId]) -> Option<&Arc<StructFields>> {
        if !self.deferred_headers.contains(&owner) {
            return self.struct_fields.get(&owner);
        }
        if let Some(key) = self.header_key(owner, args)
            && let Some(header) = self.concrete_headers.get(&key)
        {
            return header.fields.as_ref();
        }
        self.struct_fields.get(&owner)
    }

    /// Payloads belong to the nominal instance, never to its shared template.
    #[must_use]
    pub fn variant_payload_for(
        &self,
        variant: SymbolId,
        args: &[TypeId],
    ) -> Option<&EnumPayloadShape> {
        let (owner, template) = self.enum_variants.get(&variant)?;
        if self.deferred_headers.contains(owner)
            && let Some(key) = self.header_key(*owner, args)
            && let Some(header) = self.concrete_headers.get(&key)
        {
            return header
                .variants
                .iter()
                .find(|(symbol, _, _)| *symbol == variant)
                .map(|(_, _, payload)| payload);
        }
        Some(template)
    }

    #[must_use]
    pub fn variant_type_for(&self, variant: SymbolId, args: &[TypeId]) -> Option<TypeId> {
        let (owner, _) = self.enum_variants.get(&variant)?;
        let key = self.header_key(*owner, args)?;
        let header = self.concrete_headers.get(&key)?;
        let payload = header
            .variants
            .iter()
            .find(|(symbol, _, _)| *symbol == variant)?;
        let parameters = match &payload.2 {
            EnumPayloadShape::Unit => return Some(header.signature),
            EnumPayloadShape::Tuple(items) => items.as_slice(),
        };
        Some(self.type_interner.intern(ArType::func(
            parameters,
            header.signature,
            &self.type_interner,
        )))
    }

    pub fn record_enum_variant_tag(&mut self, variant: SymbolId, tag: usize) {
        self.enum_variant_tags.insert(variant, tag);
    }

    /// The bare variant name (`Color.Red` → `Red`) for an enum variant symbol.
    #[must_use]
    pub fn enum_variant_name(
        &self,
        symbols: &crate::SymbolTable,
        variant: SymbolId,
    ) -> Option<String> {
        symbols
            .try_get(variant)
            .map(|s| s.name.rsplit('.').next().unwrap_or("").to_string())
    }

    /// Find the canonical variant symbol for an enum by its bare name.
    /// Returns `(canonical_variant_symbol, enum_symbol, tag)` when found.
    #[must_use]
    pub fn enum_variant_by_name(
        &self,
        symbols: &crate::SymbolTable,
        enum_sym: SymbolId,
        name: &str,
    ) -> Option<(SymbolId, SymbolId, usize)> {
        for (&v_sym, &(parent, _)) in &self.enum_variants {
            if parent == enum_sym
                && symbols
                    .try_get(v_sym)
                    .is_some_and(|s| s.name.rsplit('.').next().unwrap_or("") == name)
                && let Some(&tag) = self.enum_variant_tags.get(&v_sym)
            {
                return Some((v_sym, enum_sym, tag));
            }
        }
        None
    }

    /// Whether values of this type may be used after "move" (copy semantics).
    ///
    /// # Rules (Minimal / big-tech POD)
    /// - Scalars, AMIR `GenRef`, bare `ptr` and shared `ref`: same as
    ///   [`ArType::is_copy_v01`]. Exclusive `mut ref` is move-only.
    /// - **Named structs**: auto-copy iff **all fields are POD components**
    ///   (no raw pointers / refs / `str` / owned aggregates). Empty structs are copy.
    /// - Tuples / arrays / `Option` / `Result`: copy iff all payload components are POD.
    ///
    /// Bare pointers stay copy (cheap handles); wrapping them in a struct (`Vec`)
    /// does **not** make the struct copy — that would double-free ownership.
    #[must_use]
    pub fn is_copy(&self, id: TypeId) -> bool {
        let mut visiting = FxHashMap::default();
        self.is_copy_rec(id, &mut visiting)
    }

    /// Enumerate every safe-borrow leaf contained by `id` in canonical order.
    ///
    /// Recursive nominal types terminate through `visiting`. Exceeding a
    /// public bound is reported to the caller so it can fail closed instead of
    /// silently truncating a safety contract.
    pub fn borrow_paths(
        &self,
        id: TypeId,
    ) -> Result<Vec<(BorrowPath, BorrowKind)>, BorrowShapeError> {
        let mut output = Vec::new();
        let mut visiting = Vec::new();
        self.collect_borrow_paths(id, &mut SmallVec::new(), &mut visiting, &mut output)?;
        output.sort();
        output.dedup();
        Ok(output)
    }

    /// Whether a type structurally contains an explicit `ref`/`mut ref`.
    /// Slice views are deliberately excluded: callers that need to reason
    /// about their owner lifetime use the borrow-path contract instead.
    #[must_use]
    pub fn contains_explicit_reference(&self, id: TypeId) -> bool {
        fn visit(info: &TypeInfo, id: TypeId, visiting: &mut Vec<TypeId>) -> bool {
            if visiting.contains(&id) {
                return false;
            }
            visiting.push(id);
            let found = match info.type_interner.resolve(id) {
                ArType::Ref(_) | ArType::RefMut(_) => true,
                ArType::Tuple(items) => info
                    .type_interner
                    .type_args(items)
                    .into_iter()
                    .any(|item| visit(info, item, visiting)),
                ArType::Option(inner)
                | ArType::Nullable(inner)
                | ArType::Array(_, inner)
                | ArType::ConstArray(_, inner)
                | ArType::Poll(inner)
                | ArType::Coroutine(inner)
                | ArType::Range(inner) => visit(info, inner, visiting),
                ArType::Result(ok, err) => visit(info, ok, visiting) || visit(info, err, visiting),
                ArType::Named(symbol, arguments) => {
                    let args = info.type_interner.type_args(arguments);
                    let substitution = info.generic_params.get(&symbol).and_then(|parameters| {
                        (parameters.len() == args.len() && !parameters.is_empty())
                            .then(|| build_subst_ids(parameters, &args, &info.type_interner))
                    });
                    let fields_contain_ref = info.fields_for(symbol, &args).is_some_and(|fields| {
                        fields.fields.iter().any(|field| {
                            let field_ty = substitution.as_ref().map_or(field.ty, |substitution| {
                                let field = info.type_interner.resolve(field.ty);
                                info.type_interner.intern(substitute_type(
                                    &field,
                                    substitution,
                                    &info.type_interner,
                                ))
                            });
                            visit(info, field_ty, visiting)
                        })
                    });
                    fields_contain_ref
                        || info.enum_variants.iter().any(|(variant, (owner, _))| {
                            if *owner != symbol {
                                return false;
                            }
                            let Some(payload) = info.variant_payload_for(*variant, &args) else {
                                return false;
                            };
                            let substitution = substitution.as_ref();
                            match payload {
                                EnumPayloadShape::Tuple(items) => items.iter().any(|item| {
                                    let item_ty = substitution.map_or(*item, |substitution| {
                                        let item = info.type_interner.resolve(*item);
                                        info.type_interner.intern(substitute_type(
                                            &item,
                                            substitution,
                                            &info.type_interner,
                                        ))
                                    });
                                    visit(info, item_ty, visiting)
                                }),
                                EnumPayloadShape::Unit => false,
                            }
                        })
                }
                // `[]T` is a view with a separate owner-lifetime contract,
                // not an embedded explicit reference for this diagnostic.
                ArType::Slice(_) => false,
                _ => false,
            };
            visiting.pop();
            found
        }

        visit(self, id, &mut Vec::new())
    }

    fn collect_borrow_paths(
        &self,
        id: TypeId,
        prefix: &mut SmallVec<[BorrowPathSegment; 4]>,
        visiting: &mut Vec<TypeId>,
        output: &mut Vec<(BorrowPath, BorrowKind)>,
    ) -> Result<(), BorrowShapeError> {
        if prefix.len() > Self::MAX_BORROW_PATH_DEPTH {
            return Err(BorrowShapeError::DepthExceeded);
        }
        if output.len() >= Self::MAX_BORROW_PATHS {
            return Err(BorrowShapeError::PathCountExceeded);
        }
        if visiting.contains(&id) {
            return Ok(());
        }
        visiting.push(id);
        let ty = self.type_interner.resolve(id);
        match ty {
            ArType::Ref(_) => output.push((BorrowPath(prefix.clone()), BorrowKind::Shared)),
            ArType::RefMut(_) => {
                output.push((BorrowPath(prefix.clone()), BorrowKind::Exclusive));
            }
            ArType::Tuple(items) => {
                let items = self.type_interner.type_args(items);
                for (index, item) in items.into_iter().enumerate() {
                    let Ok(index) = u32::try_from(index) else {
                        return Err(BorrowShapeError::PathCountExceeded);
                    };
                    prefix.push(BorrowPathSegment::Tuple(index));
                    self.collect_borrow_paths(item, prefix, visiting, output)?;
                    prefix.pop();
                }
            }
            ArType::Option(inner) => {
                prefix.push(BorrowPathSegment::OptionSome);
                self.collect_borrow_paths(inner, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Result(ok, err) => {
                prefix.push(BorrowPathSegment::ResultOk);
                self.collect_borrow_paths(ok, prefix, visiting, output)?;
                prefix.pop();
                prefix.push(BorrowPathSegment::ResultErr);
                self.collect_borrow_paths(err, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Named(symbol, arguments) => {
                if let Some(fields) =
                    self.fields_for(symbol, &self.type_interner.type_args(arguments))
                {
                    let args = self.type_interner.type_args(arguments);
                    let substitution = self.generic_params.get(&symbol).and_then(|parameters| {
                        (parameters.len() == args.len() && !parameters.is_empty())
                            .then(|| build_subst_ids(parameters, &args, &self.type_interner))
                    });
                    let mut field_refs = fields.fields.iter().collect::<Vec<_>>();
                    field_refs.sort_by_key(|f| f.name.as_str());
                    for f in field_refs {
                        let field_id = if let Some(substitution) = &substitution {
                            let field = self.type_interner.resolve(f.ty);
                            self.type_interner.intern(substitute_type(
                                &field,
                                substitution,
                                &self.type_interner,
                            ))
                        } else {
                            f.ty
                        };
                        prefix.push(BorrowPathSegment::Field(f.name.as_str().into()));
                        self.collect_borrow_paths(field_id, prefix, visiting, output)?;
                        prefix.pop();
                    }
                }

                let mut variants = self
                    .enum_variants
                    .iter()
                    .filter(|(_, (owner, _))| *owner == symbol)
                    .filter_map(|(variant, _)| {
                        Some((
                            self.enum_variant_tags.get(variant).copied().unwrap_or(0),
                            self.variant_payload_for(
                                *variant,
                                &self.type_interner.type_args(arguments),
                            )?,
                        ))
                    })
                    .collect::<Vec<_>>();
                variants.sort_by_key(|(tag, _)| *tag);
                for (tag, payload) in variants {
                    let Ok(tag) = u32::try_from(tag) else {
                        return Err(BorrowShapeError::PathCountExceeded);
                    };
                    if let EnumPayloadShape::Tuple(items) = payload {
                        for (index, &item) in items.iter().enumerate() {
                            let Ok(index) = u32::try_from(index) else {
                                return Err(BorrowShapeError::PathCountExceeded);
                            };
                            prefix.push(BorrowPathSegment::Variant(tag));
                            prefix.push(BorrowPathSegment::Payload(index));
                            self.collect_borrow_paths(item, prefix, visiting, output)?;
                            prefix.pop();
                            prefix.pop();
                        }
                    }
                }
            }
            ArType::Array(_, inner) | ArType::ConstArray(_, inner) => {
                prefix.push(BorrowPathSegment::ArrayElement);
                self.collect_borrow_paths(inner, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Nullable(inner) => {
                prefix.push(BorrowPathSegment::NullableValue);
                self.collect_borrow_paths(inner, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Coroutine(inner) => {
                prefix.push(BorrowPathSegment::CoroutinePayload);
                self.collect_borrow_paths(inner, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Poll(inner) => {
                prefix.push(BorrowPathSegment::PollReady);
                self.collect_borrow_paths(inner, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Range(inner) => {
                prefix.push(BorrowPathSegment::RangeElement);
                self.collect_borrow_paths(inner, prefix, visiting, output)?;
                prefix.pop();
            }
            ArType::Slice(_) => {
                // A slice is itself a shared, non-owning view of its backing
                // storage. Its ptr+len runtime representation carries no
                // provenance, so the root dependency must survive in the
                // compile-time structural contract.
                output.push((BorrowPath(prefix.clone()), BorrowKind::Shared));
            }
            ArType::Primitive(_)
            | ArType::FrozenConst(_)
            | ArType::Const(_)
            | ArType::ConstParam(_)
            | ArType::Func(_, _)
            | ArType::Ptr(_)
            | ArType::GenRef
            | ArType::Err
            | ArType::Void
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Error => {}
        }
        visiting.pop();
        Ok(())
    }

    /// `visiting`: TypeId → currently on the stack (cycle → not copy).
    fn is_copy_rec(&self, id: TypeId, visiting: &mut FxHashMap<TypeId, bool>) -> bool {
        if visiting.get(&id).copied().unwrap_or(false) {
            return false;
        }
        visiting.insert(id, true);
        let ty = self.type_interner.resolve(id);
        let result = match &ty {
            ArType::Named(sym, args) => {
                let args = self.type_interner.type_args(*args);
                self.is_named_struct_pod_copy(*sym, &args, visiting)
            }
            ArType::Tuple(elems) => {
                let elems = self.type_interner.type_args(*elems);
                elems.iter().all(|&e| self.is_pod_component(e, visiting))
            }
            ArType::Array(_, elem) | ArType::ConstArray(_, elem) => {
                self.is_pod_component(*elem, visiting)
            }
            ArType::Option(inner) => self.is_pod_component(*inner, visiting),
            ArType::Result(ok, err) => {
                self.is_pod_component(*ok, visiting) && self.is_pod_component(*err, visiting)
            }
            other => other.is_copy_v01(),
        };
        visiting.insert(id, false);
        result
    }

    /// POD component: bitwise-copyable value without dual ownership of heap.
    fn is_pod_component(&self, id: TypeId, visiting: &mut FxHashMap<TypeId, bool>) -> bool {
        if visiting.get(&id).copied().unwrap_or(false) {
            return false;
        }
        let ty = self.type_interner.resolve(id);
        match &ty {
            ArType::Primitive(p) => {
                p.is_numeric()
                    || matches!(
                        p,
                        Primitive::Bool | Primitive::Char | Primitive::Byte | Primitive::Str
                    )
            }
            ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::GenRef
            | ArType::FrozenConst(_)
            | ArType::Const(_) => true,
            ArType::Named(sym, args) => {
                let args = self.type_interner.type_args(*args);
                visiting.insert(id, true);
                let result = self.is_named_struct_pod_copy(*sym, &args, visiting);
                visiting.insert(id, false);
                result
            }
            ArType::Tuple(elems) => {
                let elems = self.type_interner.type_args(*elems);
                elems.iter().all(|&e| self.is_pod_component(e, visiting))
            }
            ArType::Array(_, elem) | ArType::ConstArray(_, elem) => {
                self.is_pod_component(*elem, visiting)
            }
            ArType::Option(inner) => self.is_pod_component(*inner, visiting),
            ArType::Result(ok, err) => {
                self.is_pod_component(*ok, visiting) && self.is_pod_component(*err, visiting)
            }
            // Shared borrows remain copyable inside structural carriers. An
            // exclusive borrow is move-only, and raw pointers nested in an
            // owner aggregate remain non-POD to avoid accidental double free.
            ArType::Ref(_) | ArType::Slice(_) => true,
            ArType::Ptr(_)
            | ArType::ConstParam(_)
            | ArType::RefMut(_)
            | ArType::Nullable(_)
            | ArType::Func(_, _)
            | ArType::Coroutine(_)
            | ArType::Poll(_)
            | ArType::Range(_)
            | ArType::Err
            | ArType::Void
            | ArType::Error => false,
        }
    }

    fn is_named_struct_pod_copy(
        &self,
        sym: SymbolId,
        args: &[TypeId],
        visiting: &mut FxHashMap<TypeId, bool>,
    ) -> bool {
        // A destructor is an ownership obligation even for empty/scalar storage.
        // Copying such a value would duplicate that obligation.
        if self.destructors.contains_key(&sym) {
            return false;
        }
        let mut variants: Vec<_> = self
            .enum_variants
            .iter()
            .filter(|(_, (owner, _))| *owner == sym)
            .collect();
        if !variants.is_empty() {
            variants.sort_by_key(|(symbol, _)| {
                (
                    self.enum_variant_tags.get(symbol).copied(),
                    symbol.file_id,
                    symbol.local_id.0,
                )
            });
            let parameters = self
                .generic_params
                .get(&sym)
                .map_or(&[][..], |p| p.as_slice());
            if parameters.len() != args.len() {
                return false;
            }
            let substitution = build_subst_ids(parameters, args, &self.type_interner);
            return variants.iter().all(|(variant, _)| {
                match self.variant_payload_for(**variant, args) {
                    None => false,
                    Some(payload) => match payload {
                        EnumPayloadShape::Unit => true,
                        EnumPayloadShape::Tuple(fields) => fields.iter().all(|ty| {
                            let ty = arandu_middle::types::substitute_type_id(
                                *ty,
                                &substitution,
                                &self.type_interner,
                            );
                            self.is_pod_component(ty, visiting)
                        }),
                    },
                }
            });
        }
        let Some(fields) = self.fields_for(sym, args) else {
            return false;
        };
        if fields.is_empty() {
            return true;
        }
        let params = self.generic_params.get(&sym);
        let use_subst = params.is_some_and(|p| !p.is_empty()) && !args.is_empty();
        if use_subst {
            let Some(params) = params else {
                return false;
            };
            if params.len() != args.len() {
                return false;
            }
            let subst = build_subst_ids(params, args, &self.type_interner);
            for f in fields.iter() {
                let field_ty = self.type_interner.resolve(f.ty);
                let inst = substitute_type(&field_ty, &subst, &self.type_interner);
                let inst_id = self.type_interner.intern(inst);
                if !self.is_pod_component(inst_id, visiting) {
                    return false;
                }
            }
            return true;
        }
        fields.iter().all(|f| self.is_pod_component(f.ty, visiting))
    }
}

/// Deterministic reason why a structural borrow contract cannot be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowShapeError {
    DepthExceeded,
    PathCountExceeded,
}

/// Re-intern `ty` from `from` into `to`, recursively translating nested TypeIds.
///
/// Used when merging type info across files / HIR module linking so TypeIds from
/// one interner become valid in another.
pub fn translate_type(ty: &ArType, from: &TypeInterner, to: &mut TypeInterner) -> ArType {
    match ty {
        ArType::Primitive(p) => ArType::Primitive(*p),
        ArType::Named(id, args) => {
            let old_args = from.type_args(*args);
            let new_args = old_args
                .iter()
                .map(|&arg_id| {
                    let resolved = from.resolve(arg_id);
                    let translated = translate_type(&resolved, from, to);
                    to.intern(translated)
                })
                .collect::<Vec<_>>();
            ArType::named(*id, &new_args, to)
        }
        ArType::Func(params, ret) => {
            let old_params = from.type_args(*params);
            let new_params = old_params
                .iter()
                .map(|&param_id| {
                    let resolved = from.resolve(param_id);
                    let translated = translate_type(&resolved, from, to);
                    to.intern(translated)
                })
                .collect::<Vec<_>>();
            let resolved_ret = from.resolve(*ret);
            let translated_ret = translate_type(&resolved_ret, from, to);
            let new_ret = to.intern(translated_ret);
            ArType::func(&new_params, new_ret, to)
        }
        ArType::Nullable(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Nullable(new_inner)
        }
        ArType::Slice(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Slice(new_inner)
        }
        ArType::Array(n, inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Array(*n, new_inner)
        }
        ArType::ConstArray(param, inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::ConstArray(*param, new_inner)
        }
        ArType::Const(value) => ArType::Const(*value),
        ArType::FrozenConst(value) => ArType::FrozenConst(Arc::clone(value)),
        ArType::ConstParam(param) => ArType::ConstParam(*param),
        ArType::Ptr(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Ptr(new_inner)
        }
        ArType::Ref(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Ref(new_inner)
        }
        ArType::RefMut(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::RefMut(new_inner)
        }
        ArType::GenRef => ArType::GenRef,
        ArType::Tuple(items) => {
            let old_items = from.type_args(*items);
            let new_items = old_items
                .iter()
                .map(|&item_id| {
                    let resolved = from.resolve(item_id);
                    let translated = translate_type(&resolved, from, to);
                    to.intern(translated)
                })
                .collect::<Vec<_>>();
            ArType::tuple(&new_items, to)
        }
        ArType::Result(ok, err) => {
            let resolved_ok = from.resolve(*ok);
            let translated_ok = translate_type(&resolved_ok, from, to);
            let new_ok = to.intern(translated_ok);

            let resolved_err = from.resolve(*err);
            let translated_err = translate_type(&resolved_err, from, to);
            let new_err = to.intern(translated_err);

            ArType::Result(new_ok, new_err)
        }
        ArType::Option(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Option(new_inner)
        }
        ArType::Coroutine(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Coroutine(new_inner)
        }
        ArType::Poll(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Poll(new_inner)
        }
        ArType::Range(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Range(new_inner)
        }
        ArType::Err => ArType::Err,
        ArType::Void => ArType::Void,
        ArType::Error => ArType::Error,
        ArType::IntLiteral => ArType::IntLiteral,
        ArType::FloatLiteral => ArType::FloatLiteral,
    }
}

impl TypeInfo {
    pub fn merge_from(&mut self, other: &TypeInfo) {
        self.merge_context(other, |symbol| symbol, false, true);
    }

    /// Compose units from one revision without cloning their interners or
    /// translating identical source-owned headers for every body. Only
    /// functions/parameters/locals may be rebased; nominal metadata is shared
    /// by source identity. Not suitable for merging metadata across revisions.
    pub fn merge_codegen_context(
        &mut self,
        other: &TypeInfo,
        remap: impl Fn(SymbolId) -> SymbolId,
        include_expressions: bool,
    ) {
        self.merge_context(other, remap, true, include_expressions);
    }

    fn merge_context(
        &mut self,
        other: &TypeInfo,
        remap: impl Fn(SymbolId) -> SymbolId,
        shared_headers: bool,
        include_expressions: bool,
    ) {
        // Symbol safety contracts are independent of expression-type shards.
        self.unsafe_functions
            .extend(other.unsafe_functions.iter().copied().map(&remap));
        for (&symbol, &effects) in &other.function_effects {
            self.function_effects.insert(remap(symbol), effects);
        }

        self.deferred_headers
            .extend(other.deferred_headers.iter().copied().map(&remap));
        self.header_requests.extend(
            other
                .header_requests
                .iter()
                .map(|(key, &span)| (key.clone(), span)),
        );
        for (key, header) in &other.concrete_headers {
            if shared_headers && self.concrete_headers.contains_key(key) {
                continue;
            }
            let translate = |id| {
                let ty = other.type_interner.resolve(id);
                let ty = translate_type(&ty, &other.type_interner, &mut self.type_interner);
                self.type_interner.intern(ty)
            };
            let mut translate = translate;
            let signature = translate(header.signature);
            let fields = header.fields.as_ref().map(|fields| {
                Arc::new(StructFields::from_entries(fields.iter().map(|field| {
                    StructFieldInfo {
                        name: field.name.clone(),
                        symbol: field.symbol.map(&remap),
                        index: field.index,
                        ty: translate(field.ty),
                    }
                })))
            });
            let variants = header
                .variants
                .iter()
                .map(|(symbol, tag, payload)| {
                    (
                        remap(*symbol),
                        *tag,
                        match payload {
                            EnumPayloadShape::Unit => EnumPayloadShape::Unit,
                            EnumPayloadShape::Tuple(items) => EnumPayloadShape::Tuple(
                                items.iter().map(|&id| translate(id)).collect(),
                            ),
                        },
                    )
                })
                .collect();
            let mut key = key.clone();
            key.definition = remap(key.definition);
            self.concrete_headers.insert(
                key,
                Arc::new(ConcreteHeader {
                    signature,
                    fields,
                    variants,
                }),
            );
        }

        // Fast path: empty body shards / empty import stubs.
        if other.decl_types.is_empty()
            && other.return_borrow_summaries.is_empty()
            && other.struct_fields.is_empty()
            && other.private_fields.is_empty()
            && other.enum_variants.is_empty()
            && other.enum_variant_tags.is_empty()
            && other.destructors.is_empty()
            && other.destructor_instances.is_empty()
            && other.generic_params.is_empty()
            && other.unsafe_functions.is_empty()
            && other.generic_defaults.is_empty()
            && other.param_constraints.is_empty()
            && other.interfaces.is_empty()
            && other.struct_repr_c.is_empty()
            && other.expr_types.iter().all(|s| s.is_none())
        {
            return;
        }

        for (&symbol, &other_type_id) in &other.decl_types {
            let mapped = remap(symbol);
            if shared_headers && mapped == symbol && self.decl_types.contains_key(&mapped) {
                continue;
            }
            let other_type = other.type_interner.resolve(other_type_id);
            let translated =
                translate_type(&other_type, &other.type_interner, &mut self.type_interner);
            let id = self.type_interner.intern(translated);
            self.record_decl_type(mapped, id);
        }
        self.return_borrow_summaries.extend(
            other
                .return_borrow_summaries
                .iter()
                .map(|(&symbol, summary)| (remap(symbol), summary.clone())),
        );
        for (symbol, fields) in &other.struct_fields {
            if shared_headers && self.struct_fields.contains_key(symbol) {
                continue;
            }
            let translated: StructFields =
                StructFields::from_entries(fields.iter().map(|f| StructFieldInfo {
                    name: f.name.clone(),
                    symbol: f.symbol,
                    ty: {
                        let ty = other.type_interner.resolve(f.ty);
                        let translated =
                            translate_type(&ty, &other.type_interner, &mut self.type_interner);
                        self.type_interner.intern(translated)
                    },
                    index: f.index,
                }));
            self.struct_fields.insert(*symbol, Arc::new(translated));
        }
        self.private_fields
            .extend(other.private_fields.iter().copied());
        for (symbol, (enum_id, shape)) in &other.enum_variants {
            if shared_headers && self.enum_variants.contains_key(symbol) {
                continue;
            }
            let translated_shape = match shape {
                EnumPayloadShape::Unit => EnumPayloadShape::Unit,
                EnumPayloadShape::Tuple(tids) => {
                    let mut new_tids = Vec::with_capacity(tids.len());
                    for &tid in tids {
                        let ty = other.type_interner.resolve(tid);
                        let translated =
                            translate_type(&ty, &other.type_interner, &mut self.type_interner);
                        new_tids.push(self.type_interner.intern(translated));
                    }
                    EnumPayloadShape::Tuple(new_tids)
                }
            };
            self.enum_variants
                .insert(*symbol, (*enum_id, translated_shape));
        }
        for (&symbol, &tag) in &other.enum_variant_tags {
            self.enum_variant_tags.insert(symbol, tag);
        }
        for (&type_symbol, &destructor) in &other.destructors {
            self.destructors.insert(type_symbol, destructor);
        }
        for (&other_type, &destructor) in &other.destructor_instances {
            let ty = other.type_interner.resolve(other_type);
            let translated = translate_type(&ty, &other.type_interner, &mut self.type_interner);
            let concrete = self.type_interner.intern(translated);
            self.destructor_instances
                .insert(concrete, remap(destructor));
        }
        for (symbol, params) in &other.generic_params {
            self.generic_params.insert(*symbol, Arc::clone(params));
        }
        for (symbol, &def_tid) in &other.generic_defaults {
            if shared_headers && self.generic_defaults.contains_key(symbol) {
                continue;
            }
            let ty = other.type_interner.resolve(def_tid);
            let translated = translate_type(&ty, &other.type_interner, &mut self.type_interner);
            self.generic_defaults
                .insert(*symbol, self.type_interner.intern(translated));
        }
        for (symbol, constraints) in &other.param_constraints {
            if shared_headers && self.param_constraints.contains_key(symbol) {
                continue;
            }
            let mut translated_constraints = Vec::with_capacity(constraints.len());
            for c in constraints.iter() {
                let mut new_args = SmallVec::with_capacity(c.type_args.len());
                for &arg in &c.type_args {
                    let ty = other.type_interner.resolve(arg);
                    let translated =
                        translate_type(&ty, &other.type_interner, &mut self.type_interner);
                    new_args.push(self.type_interner.intern(translated));
                }
                translated_constraints.push(InterfaceConstraint {
                    iface_sym: c.iface_sym,
                    type_args: new_args,
                });
            }
            self.param_constraints
                .insert(*symbol, Arc::new(translated_constraints));
        }
        for (symbol, interface_info) in &other.interfaces {
            if shared_headers && self.interfaces.contains_key(symbol) {
                continue;
            }
            let mut translated_methods = Vec::new();
            for m in &interface_info.methods {
                let ty = other.type_interner.resolve(m.sig_id);
                let translated = translate_type(&ty, &other.type_interner, &mut self.type_interner);
                translated_methods.push(types::InterfaceMethod {
                    name: m.name.clone(),
                    sig_id: self.type_interner.intern(translated),
                    generic_params: m.generic_params.clone(),
                });
            }
            self.interfaces.insert(
                *symbol,
                types::InterfaceInfo {
                    self_param: interface_info.self_param,
                    sealed: interface_info.sealed,
                    methods: translated_methods,
                },
            );
        }
        self.struct_repr_c.extend(&other.struct_repr_c);

        // Expr types (body typeck shards): re-intern TypeIds into `self`.
        if include_expressions {
            for &expr in &other.ctfe_roots {
                if !self.ctfe_roots.contains(&expr) {
                    self.ctfe_roots.push(expr);
                }
            }
            self.ctfe_values.extend(
                other
                    .ctfe_values
                    .iter()
                    .map(|(&span, value)| (span, value.clone())),
            );
        }
        self.ctfe_global_values.extend(
            other
                .ctfe_global_values
                .iter()
                .map(|(&symbol, value)| (symbol, value.clone())),
        );
        // Signature-only TypeInfos leave this empty — skip the O(n) scan.
        if !include_expressions || other.expr_types.iter().all(|s| s.is_none()) {
            return;
        }
        if other.expr_types.len() > self.expr_types.len() {
            self.expr_types.resize(other.expr_types.len(), None);
        }
        for (idx, slot) in other.expr_types.iter().enumerate() {
            let Some(other_id) = slot else {
                continue;
            };
            if self.expr_types[idx].is_some() {
                continue; // keep first writer (signatures / earlier merge)
            }
            let other_ty = other.type_interner.resolve(*other_id);
            let translated =
                translate_type(&other_ty, &other.type_interner, &mut self.type_interner);
            let id = self.type_interner.intern(translated);
            self.expr_types[idx] = Some(id);
        }
    }

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
