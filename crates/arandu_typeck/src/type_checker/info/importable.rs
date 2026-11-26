//! What an importer reads of a module's signatures (INC.3a).
//!
//! A module's signature tables also hold the facts it merged from its own
//! imports, transitively. An importer can only name the module's symbols and
//! reach other symbols through the types those symbols mention, so it merges
//! the entries of that closure only: a signature change two imports away
//! that the module's surface never mentions leaves the importer's tables, and
//! everything typed against them, equal.

use rustc_hash::FxHashSet;

use super::{EnumPayloadShape, TypeInfo};
use crate::SymbolId;
use arandu_middle::SymbolTable;
use arandu_middle::types::{TypeId, TypeShape};

impl TypeInfo {
    /// The entries of the symbols reachable from `roots`, the symbols of
    /// `file_id` in `symbols` and the lang items; `None` when a type cannot
    /// be walked, in which case the importer merges everything.
    #[must_use]
    pub fn importable_view(
        &self,
        symbols: &SymbolTable,
        file_id: u32,
        roots: impl IntoIterator<Item = SymbolId>,
    ) -> Option<Self> {
        let keep = self.reachable(symbols, file_id, roots)?;
        Some(self.view_of(|symbol| keep.contains(symbol)))
    }

    /// These tables with only the entries of the symbols `kept` accepts.
    #[must_use]
    pub fn view_of(&self, kept: impl Fn(&SymbolId) -> bool + Copy) -> Self {
        let mut view = self.clone();
        view.ctfe_global_values.retain(|symbol, _| kept(symbol));
        view.decl_types.retain(|symbol, _| kept(symbol));
        view.deferred_headers.retain(kept);
        view.concrete_headers.retain(|key, _| kept(&key.definition));
        view.header_requests.retain(|key, _| kept(&key.definition));
        view.return_borrow_summaries
            .retain(|symbol, _| kept(symbol));
        view.struct_fields.retain(|symbol, _| kept(symbol));
        view.private_fields.retain(kept);
        view.interior_mutable_types.retain(kept);
        view.non_copy_types.retain(kept);
        view.enum_variants.retain(|symbol, _| kept(symbol));
        view.enum_variant_tags.retain(|symbol, _| kept(symbol));
        view.destructors.retain(|symbol, _| kept(symbol));
        view.destructor_instances
            .retain(|_, destructor| kept(destructor));
        view.generic_params.retain(|symbol, _| kept(symbol));
        view.generic_defaults.retain(|symbol, _| kept(symbol));
        view.callback_bounds.retain(|symbol, _| kept(symbol));
        view.param_constraints.retain(|symbol, _| kept(symbol));
        view.interfaces.retain(|symbol, _| kept(symbol));
        view.variant_instantiations
            .retain(|(symbol, _), _| kept(symbol));
        view.function_effects.retain(|symbol, _| kept(symbol));
        view.unsafe_functions.retain(kept);
        view.exported_functions.retain(kept);
        view.cold_functions.retain(kept);
        view.variadic_functions.retain(kept);
        view.struct_repr_c.retain(kept);
        view.struct_unions.retain(kept);
        view.func_param_defaults.retain(|(sym, _), _| kept(sym));
        view.derived_methods.retain(|sym, _| kept(sym));
        view
    }

    /// The symbols an importer reaches: `roots`, the symbols of `file_id`
    /// and the lang items, closed over the symbols their entries' types,
    /// fields, variants, generics, destructors, members and implementations
    /// name. `None` when a type cannot be walked.
    #[must_use]
    pub fn reachable(
        &self,
        symbols: &SymbolTable,
        file_id: u32,
        roots: impl IntoIterator<Item = SymbolId>,
    ) -> Option<FxHashSet<SymbolId>> {
        self.close_over(symbols, Some(file_id), roots)
    }

    /// [`Self::reachable`] from `roots` and the lang items only: what one
    /// item reads, without the rest of its file.
    #[must_use]
    pub fn reachable_from(
        &self,
        symbols: &SymbolTable,
        roots: impl IntoIterator<Item = SymbolId>,
    ) -> Option<FxHashSet<SymbolId>> {
        self.close_over(symbols, None, roots)
    }

    fn close_over(
        &self,
        symbols: &SymbolTable,
        file_id: Option<u32>,
        roots: impl IntoIterator<Item = SymbolId>,
    ) -> Option<FxHashSet<SymbolId>> {
        let mut variants = rustc_hash::FxHashMap::<SymbolId, Vec<SymbolId>>::default();
        for (&variant, &(parent, _)) in &self.enum_variants {
            variants.entry(parent).or_default().push(variant);
        }
        let mut members = rustc_hash::FxHashMap::<SymbolId, Vec<SymbolId>>::default();
        for ((owner, _), &member) in symbols.associated_members.iter() {
            members.entry(*owner).or_default().push(member);
        }
        let mut headers = rustc_hash::FxHashMap::<SymbolId, Vec<TypeId>>::default();
        for (key, header) in &self.concrete_headers {
            let types = headers.entry(key.definition).or_default();
            types.push(header.signature);
            if let Some(fields) = &header.fields {
                types.extend(fields.iter().map(|field| field.ty));
            }
        }
        let mut implementations = rustc_hash::FxHashMap::<SymbolId, Vec<SymbolId>>::default();
        for &(ty, interface) in &symbols.interface_implementations {
            implementations.entry(ty).or_default().push(interface);
            implementations.entry(interface).or_default().push(ty);
        }

        let mut pending = roots.into_iter().collect::<Vec<_>>();
        pending.extend(
            symbols
                .iter()
                .filter(|symbol| Some(symbol.id.file_id) == file_id || symbol.lang_item.is_some())
                .map(|symbol| symbol.id),
        );
        // Entries keyed by the module's own symbols, even those its table
        // does not list.
        pending.extend(
            self.decl_types
                .keys()
                .chain(self.struct_fields.keys())
                .chain(self.enum_variants.keys())
                .chain(self.interfaces.keys())
                .chain(self.generic_params.keys())
                .copied()
                .filter(|symbol| Some(symbol.file_id) == file_id),
        );
        let mut keep = FxHashSet::default();
        while let Some(symbol) = pending.pop() {
            if !keep.insert(symbol) {
                continue;
            }
            let mut types = Vec::<TypeId>::new();
            if let Some(&ty) = self.decl_types.get(&symbol) {
                types.push(ty);
            }
            if let Some(fields) = self.struct_fields.get(&symbol) {
                for field in fields.iter() {
                    pending.extend(field.symbol);
                    types.push(field.ty);
                }
            }
            if let Some((parent, payload)) = self.enum_variants.get(&symbol) {
                pending.push(*parent);
                if let EnumPayloadShape::Tuple(items) = payload {
                    types.extend(items.iter().copied());
                }
            }
            pending.extend(variants.get(&symbol).into_iter().flatten().copied());
            pending.extend(members.get(&symbol).into_iter().flatten().copied());
            pending.extend(implementations.get(&symbol).into_iter().flatten().copied());
            pending.extend(self.destructors.get(&symbol).copied());
            if let Some(params) = self.generic_params.get(&symbol) {
                pending.extend(params.iter().copied());
            }
            if let Some(constraints) = self.param_constraints.get(&symbol) {
                for constraint in constraints.iter() {
                    pending.push(constraint.iface_sym);
                    types.extend(constraint.type_args.iter().copied());
                }
            }
            types.extend(self.generic_defaults.get(&symbol).copied());
            types.extend(self.callback_bounds.get(&symbol).copied());
            if let Some(interface) = self.interfaces.get(&symbol) {
                pending.extend(interface.self_param);
                for method in &interface.methods {
                    types.push(method.sig_id);
                    pending.extend(method.generic_params.iter().copied());
                }
            }
            if let Some(recipe) = self.derived_methods.get(&symbol) {
                pending.push(recipe.type_symbol);
                pending.extend(recipe.param_symbols.iter().copied());
                types.push(recipe.self_param_ty);
                types.extend(recipe.explicit_param_tys.iter().copied());
                types.push(recipe.return_ty);
            }
            types.extend(headers.get(&symbol).into_iter().flatten().copied());
            for ty in types {
                TypeShape::from_id(ty, &self.type_interner)
                    .ok()?
                    .for_each_symbol(|named| pending.push(named))
                    .ok()?;
            }
        }
        Some(keep)
    }
}
