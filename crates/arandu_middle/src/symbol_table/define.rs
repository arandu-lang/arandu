use super::{LangItem, LocalSymbolId, Scope, ScopeId, Symbol, SymbolId, SymbolKind, SymbolTable};
use crate::Span;

impl super::SymbolTable {
    /// Merges definitions and scopes from another `SymbolTable` into `self`.
    ///
    /// Preserves local IDs, parent pointers, and public flags. Remaps symbol
    /// IDs in scopes, module members, and associated members to match the new
    /// table.
    pub fn merge_from(&mut self, other: SymbolTable) {
        self.unresolved_module_aliases
            .extend(other.unresolved_module_aliases.iter().cloned());
        let self_symbols_len = self.symbols.len() as u32;
        let self_scopes_len = self.scopes.len() as u32;

        let other_global = other.global_scope();
        let self_global = self.global_scope();

        let map_scope = |old_scope: ScopeId| -> ScopeId {
            if old_scope == other_global {
                self_global
            } else {
                ScopeId(old_scope.0 + self_scopes_len - 1)
            }
        };

        let map_symbol = |old_symbol: SymbolId| -> SymbolId {
            if old_symbol.is_synthetic() {
                SymbolId {
                    file_id: self.file_id,
                    local_id: old_symbol.local_id,
                }
            } else {
                SymbolId {
                    file_id: self.file_id,
                    local_id: LocalSymbolId(old_symbol.local_id.0 + self_symbols_len),
                }
            }
        };

        // 1. Merge other's global scope symbols into self's global scope symbols
        for old_symbol_id in &other.scopes[other_global.0 as usize].symbols {
            self.scopes[self_global.0 as usize]
                .symbols
                .push(map_symbol(*old_symbol_id));
        }

        for i in 0..other.scopes.len() {
            if i == other_global.0 as usize {
                continue;
            }
            let old_scope = &other.scopes[i];
            let new_parent = old_scope.parent.map(map_scope);
            let new_symbols = old_scope.symbols.iter().map(|id| map_symbol(*id)).collect();
            self.scopes.push(Scope {
                parent: new_parent,
                symbols: new_symbols,
            });
        }

        for old_symbol in other.synthetic_symbols.into_values() {
            let mut symbol = old_symbol;
            symbol.id.file_id = self.file_id;
            symbol.scope = map_scope(symbol.scope);
            self.synthetic_symbols.insert(symbol.id.local_id.0, symbol);
        }

        // 2. Merge other symbols
        for old_symbol in other.symbols {
            let new_id = map_symbol(old_symbol.id);
            let new_scope = map_scope(old_symbol.scope);
            self.symbols.push(Symbol {
                id: new_id,
                name: old_symbol.name,
                kind: old_symbol.kind,
                span: old_symbol.span,
                scope: new_scope,
                visibility: old_symbol.visibility,
                lang_item: old_symbol.lang_item,
            });
        }

        // 3. Merge other module members
        for ((module, member), old_symbol_id) in other.module_members {
            self.module_members
                .insert((module, member), map_symbol(old_symbol_id));
        }

        // 4. Merge other associated members
        for ((ty, member), old_symbol_id) in other.associated_members {
            self.associated_members
                .insert((map_symbol(ty), member), map_symbol(old_symbol_id));
        }
        self.sealed_interfaces
            .extend(other.sealed_interfaces.iter().map(|id| map_symbol(*id)));
        self.interface_implementations.extend(
            other
                .interface_implementations
                .iter()
                .map(|(ty, iface)| (map_symbol(*ty), map_symbol(*iface))),
        );

        for (old_symbol_id, name) in other.host_function_names {
            self.host_function_names
                .insert(map_symbol(old_symbol_id), name);
        }
        self.exported_functions
            .extend(other.exported_functions.into_iter().map(map_symbol));
        self.tooling_entrypoints
            .extend(other.tooling_entrypoints.into_iter().map(map_symbol));

        for (old_symbol_id, scope) in other.module_scopes {
            self.module_scopes
                .insert(map_symbol(old_symbol_id), map_scope(scope));
        }

        // 5. Merge type-parameter tables
        for (type_id, params) in other.type_params {
            let mapped: smallvec::SmallVec<[SymbolId; 4]> =
                params.into_iter().map(map_symbol).collect();
            self.type_params.insert(map_symbol(type_id), mapped);
        }

        // 6. Merge canonical builtins
        if self.builtin_alloc.is_none() {
            self.builtin_alloc = other.builtin_alloc.map(map_symbol);
        }
        if self.builtin_free.is_none() {
            self.builtin_free = other.builtin_free.map(map_symbol);
        }
        if self.builtins.option.is_none() {
            self.builtins.option = other.builtins.option.map(map_symbol);
        }
        if self.builtins.result.is_none() {
            self.builtins.result = other.builtins.result.map(map_symbol);
        }
        if self.builtins.poll.is_none() {
            self.builtins.poll = other.builtins.poll.map(map_symbol);
        }
        if self.builtins.coroutine.is_none() {
            self.builtins.coroutine = other.builtins.coroutine.map(map_symbol);
        }
        if self.builtins.string.is_none() {
            self.builtins.string = other.builtins.string.map(map_symbol);
        }
        if self.builtins.vec.is_none() {
            self.builtins.vec = other.builtins.vec.map(map_symbol);
        }
        if self.builtins.str_type.is_none() {
            self.builtins.str_type = other.builtins.str_type.map(map_symbol);
        }
        if self.builtins.slice_type.is_none() {
            self.builtins.slice_type = other.builtins.slice_type.map(map_symbol);
        }
        if self.builtins.char_type.is_none() {
            self.builtins.char_type = other.builtins.char_type.map(map_symbol);
        }
        if self.builtins.int_type.is_none() {
            self.builtins.int_type = other.builtins.int_type.map(map_symbol);
        }
        if self.builtins.float_type.is_none() {
            self.builtins.float_type = other.builtins.float_type.map(map_symbol);
        }
        if self.builtins.u32_type.is_none() {
            self.builtins.u32_type = other.builtins.u32_type.map(map_symbol);
        }
        if self.builtins.u64_type.is_none() {
            self.builtins.u64_type = other.builtins.u64_type.map(map_symbol);
        }
        if self.builtins.usize_type.is_none() {
            self.builtins.usize_type = other.builtins.usize_type.map(map_symbol);
        }
        if self.builtins.ptr_type.is_none() {
            self.builtins.ptr_type = other.builtins.ptr_type.map(map_symbol);
        }
    }

    /// Remove untrusted source declarations from the compiler identity set.
    /// Builtin compatibility symbols (empty synthetic span) are unaffected.
    pub fn clear_source_lang_items(&mut self) {
        for symbol in &mut self.symbols {
            if symbol.span.end > symbol.span.start {
                symbol.lang_item = None;
            }
        }
        // Canonical caches must not retain a source identity after revocation.
        let valid = |id: Option<SymbolId>| {
            id.filter(|id| {
                self.symbols
                    .get(id.local_id.0 as usize)
                    .is_some_and(|symbol| symbol.lang_item.is_some())
            })
        };
        self.builtins.option = valid(self.builtins.option);
        self.builtins.result = valid(self.builtins.result);
        self.builtins.poll = valid(self.builtins.poll);
        self.builtins.coroutine = valid(self.builtins.coroutine);
        self.builtins.string = valid(self.builtins.string);
        self.builtins.vec = valid(self.builtins.vec);
        self.builtins.str_type = valid(self.builtins.str_type);
        self.builtins.slice_type = valid(self.builtins.slice_type);
        self.builtins.char_type = valid(self.builtins.char_type);
        self.builtins.int_type = valid(self.builtins.int_type);
        self.builtins.float_type = valid(self.builtins.float_type);
        self.builtins.ptr_type = valid(self.builtins.ptr_type);
        self.builtins.u32_type = valid(self.builtins.u32_type);
        self.builtins.u64_type = valid(self.builtins.u64_type);
        self.builtins.usize_type = valid(self.builtins.usize_type);
        self.builtin_alloc = valid(self.builtin_alloc);
        self.builtin_free = valid(self.builtin_free);
    }

    pub fn set_lang_item(&mut self, sym: SymbolId, item: LangItem) {
        if sym.file_id == self.file_id {
            if let Some(s) = self.symbols.get_mut(sym.local_id.0 as usize) {
                s.lang_item = Some(item);
            }
        } else if let Some(s) = self
            .imported_symbols
            .contains_key(&sym)
            .then(|| self.imported_symbols_mut().get_mut(&sym))
            .flatten()
        {
            s.lang_item = Some(item);
        }
        match item {
            LangItem::Option => self.builtins.option = Some(sym),
            LangItem::Result => self.builtins.result = Some(sym),
            LangItem::Poll => self.builtins.poll = Some(sym),
            LangItem::Coroutine => self.builtins.coroutine = Some(sym),
            LangItem::String => self.builtins.string = Some(sym),
            LangItem::Vec => self.builtins.vec = Some(sym),
            LangItem::Str => self.builtins.str_type = Some(sym),
            LangItem::Slice => self.builtins.slice_type = Some(sym),
            LangItem::Char => self.builtins.char_type = Some(sym),
            LangItem::Int => self.builtins.int_type = Some(sym),
            LangItem::Float => self.builtins.float_type = Some(sym),
            LangItem::Ptr => self.builtins.ptr_type = Some(sym),
            LangItem::U32 => self.builtins.u32_type = Some(sym),
            LangItem::U64 => self.builtins.u64_type = Some(sym),
            LangItem::Usize => self.builtins.usize_type = Some(sym),
            LangItem::Alloc => self.builtin_alloc = Some(sym),
            LangItem::Free => self.builtin_free = Some(sym),
            _ => {}
        }
    }

    #[must_use]
    pub fn is_alloc_func(&self, sym: SymbolId) -> bool {
        self.try_get(sym).and_then(|s| s.lang_item) == Some(LangItem::Alloc)
            || Some(sym) == self.builtin_alloc
    }

    #[must_use]
    pub fn is_free_func(&self, sym: SymbolId) -> bool {
        self.try_get(sym).and_then(|s| s.lang_item) == Some(LangItem::Free)
            || Some(sym) == self.builtin_free
    }

    #[must_use]
    pub fn is_poll_type(&self, sym: SymbolId) -> bool {
        self.try_get(sym).and_then(|s| s.lang_item) == Some(LangItem::Poll)
            || Some(sym) == self.builtins.poll
            || self.lookup_module_member("std.core.future", "Poll") == Some(sym)
    }

    #[must_use]
    pub fn is_result_type(&self, sym: SymbolId) -> bool {
        self.try_get(sym).and_then(|s| s.lang_item) == Some(LangItem::Result)
            || Some(sym) == self.builtins.result
            || self.lookup_module_member("std.core.result", "Result") == Some(sym)
    }

    #[must_use]
    pub fn is_option_type(&self, sym: SymbolId) -> bool {
        self.try_get(sym).and_then(|s| s.lang_item) == Some(LangItem::Option)
            || Some(sym) == self.builtins.option
            || self.lookup_module_member("std.core.option", "Option") == Some(sym)
    }

    #[must_use]
    pub fn is_coroutine_type(&self, sym: SymbolId) -> bool {
        self.try_get(sym).and_then(|s| s.lang_item) == Some(LangItem::Coroutine)
            || Some(sym) == self.builtins.coroutine
            || self.lookup_module_member("std.core.coroutine", "Coroutine") == Some(sym)
    }

    /// Record ordered type parameters for a named type (struct/enum/alias).
    pub fn record_type_params(
        &mut self,
        type_id: SymbolId,
        params: impl IntoIterator<Item = SymbolId>,
    ) {
        self.type_params
            .insert(type_id, params.into_iter().collect());
    }

    /// Bind an already-defined symbol into `scope` for name lookup (no new Symbol).
    ///
    /// Used to import a parent type's type parameters into a method scope so
    /// `func Box.get(): T` can resolve `T`.
    pub fn bind_existing(&mut self, scope: ScopeId, symbol: SymbolId) {
        let name = self.get(symbol).name.clone();
        if self.find_in_scope(scope, &name).is_none() {
            self.scope_mut(scope).symbols.push(symbol);
        }
    }

    /// Defines a new symbol in the specified scope.
    /// Define a private (non-exported) symbol. Prefer [`Self::define_with_visibility`] for API items.
    ///
    /// # Errors
    ///
    /// Returns `Err(existing_symbol_id)` if a symbol with the same name already exists in the given scope.
    pub fn define(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: SymbolKind,
        span: Span,
    ) -> Result<SymbolId, SymbolId> {
        self.define_with_visibility(scope, name, kind, span, crate::Visibility::Module)
    }

    /// Define a symbol while preserving its full source visibility.
    pub fn define_with_visibility(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: SymbolKind,
        span: Span,
        visibility: crate::Visibility,
    ) -> Result<SymbolId, SymbolId> {
        if let Some(existing) = self.find_in_scope(scope, name) {
            return Err(existing);
        }

        let Ok(local) = u32::try_from(self.symbols.len()) else {
            crate::ice::bug("symbol count overflow (u32::MAX symbols)");
        };
        let id = SymbolId {
            file_id: self.file_id,
            local_id: LocalSymbolId(local),
        };
        self.symbols.push(Symbol {
            id,
            name: name.into(),
            kind,
            span,
            scope,
            visibility,
            lang_item: None,
        });
        self.scope_mut(scope).symbols.push(id);
        Ok(id)
    }

    /// Inserts an imported symbol into the global scope.
    ///
    /// # Errors
    /// Returns `Err(existing)` if a different symbol with the same name already exists in the global scope.
    pub fn insert_imported(&mut self, symbol: Symbol) -> Result<Option<SymbolId>, SymbolId> {
        if let Some(existing) = self.find_in_scope(self.global_scope_id, &symbol.name) {
            let existing_kind = self.get(existing).kind;
            if existing_kind == SymbolKind::ImportType || existing_kind == SymbolKind::ImportValue {
                // Replace placeholder in scope.
                let scope_syms = &mut self.scope_mut(self.global_scope_id).symbols;
                if let Some(pos) = scope_syms.iter().position(|&x| x == existing) {
                    scope_syms[pos] = symbol.id;
                }
                self.imported_symbols_mut().insert(symbol.id, symbol);
                return Ok(Some(existing));
            }
            if existing == symbol.id {
                return Ok(None); // exact same symbol already imported
            }
            return Err(existing);
        }

        let id = symbol.id;
        self.imported_symbols_mut().insert(id, symbol);
        self.scope_mut(self.global_scope_id).symbols.push(id);
        Ok(None)
    }

    /// Registers a symbol from another file so it can be looked up by ID,
    /// but DOES NOT put it into the global scope.
    pub fn register_imported_symbol(&mut self, symbol: Symbol) {
        self.imported_symbols_mut().insert(symbol.id, symbol);
    }

    /// The imported symbols, copied first when another table shares them.
    pub fn imported_symbols_mut(&mut self) -> &mut rustc_hash::FxHashMap<SymbolId, Symbol> {
        std::sync::Arc::make_mut(&mut self.imported_symbols)
    }

    /// Registers a span-derived symbol. The id must already carry
    /// [`SymbolId::SYNTHETIC_LOCAL_BIT`] and this table's file id.
    pub fn register_synthetic(&mut self, symbol: Symbol) {
        debug_assert!(symbol.id.is_synthetic());
        debug_assert_eq!(symbol.id.file_id, self.file_id);
        self.synthetic_symbols.insert(symbol.id.local_id.0, symbol);
    }

    /// Makes `symbol` reachable from [`Self::try_get`] without rebasing a
    /// synthetic local id onto the dense source vector.
    pub fn ensure_visible(&mut self, symbol: Symbol) {
        if self.try_get(symbol.id).is_some() {
            return;
        }
        if symbol.id.is_synthetic() && symbol.id.file_id == self.file_id {
            self.register_synthetic(symbol);
        } else {
            self.register_imported_symbol(symbol);
        }
    }

    /// Defines a member in the specified module.
    ///
    /// # Errors
    ///
    /// Returns `Err(existing_symbol_id)` if a member with the same name already exists in the module.
    pub fn define_module_member(
        &mut self,
        module: &str,
        member: &str,
        span: Span,
    ) -> Result<SymbolId, SymbolId> {
        // Prelude / module surface is part of the public API of that namespace.
        let id = self.define_with_visibility(
            self.global_scope(),
            &format!("{module}.{member}"),
            SymbolKind::NamespaceMember,
            span,
            crate::Visibility::Public,
        )?;
        self.module_members
            .insert((module.into(), member.into()), id);
        Ok(id)
    }

    /// Defines an associated member (e.g. method) on a type.
    ///
    /// # Errors
    ///
    /// Returns `Err(existing_symbol_id)` if the member is already defined on the type.
    pub fn define_associated_member(
        &mut self,
        parent_id: SymbolId,
        member: &str,
        span: Span,
    ) -> Result<SymbolId, SymbolId> {
        self.define_associated_member_with_visibility(
            parent_id,
            member,
            span,
            crate::Visibility::Module,
        )
    }

    pub fn define_associated_member_with_visibility(
        &mut self,
        parent_id: SymbolId,
        member: &str,
        span: Span,
        visibility: crate::Visibility,
    ) -> Result<SymbolId, SymbolId> {
        let parent_name = self.get(parent_id).name.clone();
        let base_ty = parent_name.split('.').next_back().unwrap_or(&parent_name);
        let id = self.define_with_visibility(
            self.global_scope(),
            &format!("{base_ty}.{member}"),
            SymbolKind::AssociatedFunc,
            span,
            visibility,
        )?;
        self.associated_members
            .insert((parent_id, member.into()), id);
        Ok(id)
    }
}
