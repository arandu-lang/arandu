use std::sync::Arc;

use smallvec::SmallVec;

use crate::SymbolId;
use arandu_middle::layout::{StructFieldInfo, StructFields};
use arandu_parser::ast_pool::ExprId;

use super::super::types;
use super::translate::{reintern_type, translate_type};
use super::{
    Adjustment, ClosureCaptureField, ClosureParam, ClosureRecipe, ConcreteHeader, EnumPayloadShape,
    InterfaceConstraint, OwningCode, TypeInfo,
};

/// Where an item-local typing shard sits in its file (RFC 0042): its
/// expression IDs start at `exprs` in the file arena and its spans in
/// `file_id` at `offset`. The default places a shard already in file
/// coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemPlacement {
    pub file_id: u32,
    pub exprs: u32,
    pub offset: u32,
    /// Other items whose AST the shard's program appends after its own: the
    /// field defaults it lowers.
    pub segments: Arc<[PlacedSegment]>,
}

/// One appended item of a local program and where it sits in its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedSegment {
    pub file_id: u32,
    /// Spans of the segment start at `local_offset` in the local program
    /// and cover `len` bytes.
    pub local_offset: u32,
    pub len: u32,
    /// Its expressions start at `local_exprs` and are `exprs_len` long.
    pub local_exprs: u32,
    pub exprs_len: u32,
    /// Start of the item in its file.
    pub offset: u32,
    /// Its first expression in the arena of the placement's file; `None` for
    /// an item of another file, whose expressions have no slot there.
    pub exprs: Option<u32>,
}

impl ItemPlacement {
    /// The `Span::new(0, 0, 0)` placeholder of synthetic nodes is not a
    /// position and is never moved, as in `Program::append_item`.
    #[must_use]
    pub fn span(&self, span: arandu_middle::Span) -> arandu_middle::Span {
        if span == arandu_middle::Span::new(0, 0, 0) {
            return span;
        }
        for segment in self.segments.iter() {
            if span.file_id == segment.file_id
                && segment.local_offset <= span.start
                && span.end <= segment.local_offset + segment.len
            {
                return arandu_middle::Span::new(
                    span.file_id,
                    span.start - segment.local_offset + segment.offset,
                    span.end - segment.local_offset + segment.offset,
                );
            }
        }
        if span.file_id != self.file_id {
            return span;
        }
        arandu_middle::Span::new(
            span.file_id,
            span.start + self.offset,
            span.end + self.offset,
        )
    }

    #[must_use]
    pub fn expr(&self, expr: ExprId) -> ExprId {
        ExprId::new(expr.as_usize() + self.exprs as usize)
    }

    /// File arena slot of a local expression; `None` when it belongs to an
    /// item of another file.
    #[must_use]
    pub fn slot(&self, local: usize) -> Option<usize> {
        for segment in self.segments.iter() {
            let start = segment.local_exprs as usize;
            if (start..start + segment.exprs_len as usize).contains(&local) {
                return segment.exprs.map(|base| base as usize + local - start);
            }
        }
        Some(self.exprs as usize + local)
    }
}

impl TypeInfo {
    pub fn merge_from(&mut self, other: &TypeInfo) {
        self.merge_context(
            other,
            |symbol| symbol,
            false,
            true,
            &ItemPlacement::default(),
        );
    }

    /// Moves the expression tables and spans of an item-local shard to `at`,
    /// in place: the inverse of making them local.
    pub fn place_at(&mut self, at: &ItemPlacement) {
        if at.segments.is_empty() {
            let mut expr_types = vec![None; at.exprs as usize];
            expr_types.append(&mut self.expr_types);
            self.expr_types = expr_types;
        } else {
            let mut expr_types = vec![None; at.exprs as usize];
            for (local, slot) in std::mem::take(&mut self.expr_types).into_iter().enumerate() {
                let (Some(ty), Some(index)) = (slot, at.slot(local)) else {
                    continue;
                };
                if expr_types.len() <= index {
                    expr_types.resize(index + 1, None);
                }
                expr_types[index].get_or_insert(ty);
            }
            self.expr_types = expr_types;
        }
        for root in &mut self.ctfe_roots {
            *root = at.expr(*root);
        }
        self.ctfe_values = std::mem::take(&mut self.ctfe_values)
            .into_iter()
            .map(|(span, value)| (at.span(span), value))
            .collect();
        for span in self.header_requests.values_mut() {
            *span = at.span(*span);
        }
        // A default of another file has no slot in this one.
        self.struct_field_defaults = std::mem::take(&mut self.struct_field_defaults)
            .into_iter()
            .filter_map(|(key, expr)| Some((key, ExprId::new(at.slot(expr.as_usize())?))))
            .collect();
    }

    /// Merge an item shard recorded in item-local coordinates, placing its
    /// expression tables and spans at `at`. Thread captures are never item
    /// local: their recipes and synthetic names carry file positions.
    pub fn merge_item_from(&mut self, item: &TypeInfo, at: &ItemPlacement) {
        self.merge_context(item, |symbol| symbol, false, true, at);
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
        self.merge_codegen_context_at(other, remap, include_expressions, &ItemPlacement::default());
    }

    /// [`Self::merge_codegen_context`] of a unit context in item-local
    /// coordinates, placed at `at` as it is merged: the same result as
    /// [`Self::place_at`] followed by the merge, without copying the context.
    pub fn merge_codegen_context_at(
        &mut self,
        other: &TypeInfo,
        remap: impl Fn(SymbolId) -> SymbolId,
        include_expressions: bool,
        at: &ItemPlacement,
    ) {
        self.merge_context(other, remap, true, include_expressions, at);
    }

    fn merge_context(
        &mut self,
        other: &TypeInfo,
        remap: impl Fn(SymbolId) -> SymbolId,
        shared_headers: bool,
        include_expressions: bool,
        at: &ItemPlacement,
    ) {
        self.merge_symbol_contracts(other, &remap, at);
        self.merge_concrete_headers(other, &remap, shared_headers);
        self.merge_closures(other, &remap);
        for (span, adjustment) in &other.adjustments {
            let kind = match &adjustment.kind {
                super::AdjustmentKind::Call {
                    function,
                    type_args,
                } => super::AdjustmentKind::Call {
                    function: remap(*function),
                    type_args: type_args
                        .iter()
                        .map(|&ty| reintern_type(self, other, ty))
                        .collect(),
                },
                super::AdjustmentKind::Unsize => super::AdjustmentKind::Unsize,
                super::AdjustmentKind::TextView => super::AdjustmentKind::TextView,
            };
            let adjusted = Adjustment {
                kind,
                from: reintern_type(self, other, adjustment.from),
                to: reintern_type(self, other, adjustment.to),
            };
            self.adjustments.insert(*span, adjusted);
        }
        for (span, temporary) in &other.closure_temporaries {
            let temporary = super::ClosureTemporary {
                local: remap(temporary.local),
                ty: reintern_type(self, other, temporary.ty),
            };
            self.closure_temporaries.insert(*span, temporary);
        }
        // Fast path: empty body shards / empty import stubs.
        if context_shard_is_empty(other) {
            return;
        }
        self.merge_nominal_tables(other, &remap, shared_headers, at);
        self.merge_expression_tables(other, include_expressions, at);
    }

    fn merge_symbol_contracts<F>(&mut self, other: &TypeInfo, remap: &F, at: &ItemPlacement)
    where
        F: Fn(SymbolId) -> SymbolId,
    {
        // Symbol safety contracts are independent of expression-type shards.
        self.unsafe_functions
            .extend(other.unsafe_functions.iter().copied().map(&remap));
        self.exported_functions
            .extend(other.exported_functions.iter().copied().map(&remap));
        for (&symbol, &effects) in &other.function_effects {
            self.function_effects.insert(remap(symbol), effects);
        }

        self.deferred_headers
            .extend(other.deferred_headers.iter().copied().map(&remap));
        self.header_requests.extend(
            other
                .header_requests
                .iter()
                .map(|(key, &span)| (key.clone(), at.span(span))),
        );
    }

    fn merge_concrete_headers<F>(&mut self, other: &TypeInfo, remap: &F, shared_headers: bool)
    where
        F: Fn(SymbolId) -> SymbolId,
    {
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
    }

    pub(super) fn merge_closures<F>(&mut self, other: &TypeInfo, remap: &F)
    where
        F: Fn(SymbolId) -> SymbolId,
    {
        for (span, recipe) in &other.closures {
            let fields = recipe
                .fields
                .iter()
                .map(|field| ClosureCaptureField {
                    name: field.name.clone(),
                    symbol: remap(field.symbol),
                    source: remap(field.source),
                    ty: reintern_type(self, other, field.ty),
                    field_ty: reintern_type(self, other, field.field_ty),
                    span: field.span,
                    mode: field.mode,
                })
                .collect();
            let params = recipe
                .params
                .iter()
                .map(|param| ClosureParam {
                    symbol: remap(param.symbol),
                    ty: reintern_type(self, other, param.ty),
                })
                .collect();
            let return_type = reintern_type(self, other, recipe.return_type);
            let owning = recipe.owning.as_ref().map(|code| OwningCode {
                symbol: remap(code.symbol),
                env_param: remap(code.env_param),
                params: code.params.iter().copied().map(remap).collect(),
                host_name: code.host_name.clone(),
                closure_ty: reintern_type(self, other, code.closure_ty),
            });
            self.closures.insert(
                *span,
                ClosureRecipe {
                    lambda_span: recipe.lambda_span,
                    struct_symbol: remap(recipe.struct_symbol),
                    struct_name: recipe.struct_name.clone(),
                    method_symbol: remap(recipe.method_symbol),
                    method_name: recipe.method_name.clone(),
                    self_param: remap(recipe.self_param),
                    params,
                    fields,
                    return_type,
                    host_name: recipe.host_name.clone(),
                    adapts: recipe.adapts.map(remap),
                    owning,
                },
            );
        }
    }

    fn merge_nominal_tables<F>(
        &mut self,
        other: &TypeInfo,
        remap: &F,
        shared_headers: bool,
        at: &ItemPlacement,
    ) where
        F: Fn(SymbolId) -> SymbolId,
    {
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
        for (symbol, &bound) in &other.callback_bounds {
            if shared_headers && self.callback_bounds.contains_key(symbol) {
                continue;
            }
            let ty = other.type_interner.resolve(bound);
            let translated = translate_type(&ty, &other.type_interner, &mut self.type_interner);
            self.callback_bounds
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
        self.struct_unions.extend(&other.struct_unions);
        for ((sym, name), &expr_id) in &other.struct_field_defaults {
            // A default of another file has no slot in this one.
            if let Some(slot) = at.slot(expr_id.as_usize()) {
                self.struct_field_defaults
                    .insert((remap(*sym), name.clone()), ExprId::new(slot));
            }
        }
        self.interior_mutable_types
            .extend(other.interior_mutable_types.iter().copied().map(&remap));
    }

    fn merge_expression_tables(
        &mut self,
        other: &TypeInfo,
        include_expressions: bool,
        at: &ItemPlacement,
    ) {
        // Expr types (body typeck shards): re-intern TypeIds into `self`.
        if include_expressions {
            for &expr in &other.ctfe_roots {
                let expr = at.expr(expr);
                if !self.ctfe_roots.contains(&expr) {
                    self.ctfe_roots.push(expr);
                }
            }
            self.ctfe_values.extend(
                other
                    .ctfe_values
                    .iter()
                    .map(|(&span, value)| (at.span(span), value.clone())),
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
        if at.segments.is_empty() {
            let end = at.exprs as usize + other.expr_types.len();
            if end > self.expr_types.len() {
                self.expr_types.resize(end, None);
            }
        }
        for (idx, slot) in other.expr_types.iter().enumerate() {
            let Some(other_id) = slot else {
                continue;
            };
            let Some(idx) = at.slot(idx) else {
                continue;
            };
            if self.expr_types.len() <= idx {
                self.expr_types.resize(idx + 1, None);
            }
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
}

fn context_shard_is_empty(other: &TypeInfo) -> bool {
    other.decl_types.is_empty()
        && other.return_borrow_summaries.is_empty()
        && other.struct_fields.is_empty()
        && other.private_fields.is_empty()
        && other.interior_mutable_types.is_empty()
        && other.enum_variants.is_empty()
        && other.enum_variant_tags.is_empty()
        && other.destructors.is_empty()
        && other.destructor_instances.is_empty()
        && other.generic_params.is_empty()
        && other.unsafe_functions.is_empty()
        && other.exported_functions.is_empty()
        && other.generic_defaults.is_empty()
        && other.callback_bounds.is_empty()
        && other.param_constraints.is_empty()
        && other.interfaces.is_empty()
        && other.struct_repr_c.is_empty()
        && other.struct_unions.is_empty()
        && other.expr_types.iter().all(|s| s.is_none())
}
