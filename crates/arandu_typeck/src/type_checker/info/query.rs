use std::sync::Arc;

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::SymbolId;
use arandu_middle::layout::StructFields;
use arandu_middle::types::{BorrowKind, BorrowPath, BorrowPathSegment};

use super::super::types::{ArType, Primitive, TypeId, build_subst_ids, substitute_type};
use super::{BorrowShapeError, EnumPayloadShape, TypeInfo};

impl TypeInfo {
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
                | ArType::MutSlice(inner)
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
                | ArType::Vector(_)
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
                // `[]T` and `mut []T` are views with a separate owner-lifetime contract,
                // not an embedded explicit reference for this diagnostic.
                ArType::Slice(_) | ArType::MutSlice(_) => false,
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
            ArType::MutSlice(_) => {
                // An exclusive mutable slice view of its backing storage.
                // Carries exclusive borrow semantics across dataflow.
                output.push((BorrowPath(prefix.clone()), BorrowKind::Exclusive));
            }
            ArType::Primitive(_)
            | ArType::Vector(_)
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
            ArType::Vector(_)
            | ArType::IntLiteral
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
            | ArType::MutSlice(_)
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
        if self.destructors.contains_key(&sym) || self.interior_mutable_types.contains(&sym) {
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
