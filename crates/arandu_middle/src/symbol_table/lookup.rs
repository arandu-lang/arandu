use super::{LangItem, Scope, ScopeId, Symbol, SymbolId, SymbolKind};
use smol_str::SmolStr;

impl super::SymbolTable {
    /// Canonical owner for native vector methods. Imported aliases may provide
    /// several identities; select a stable ID rather than hash-table order.
    #[must_use]
    pub fn vector_type_symbol(&self, shape: crate::types::VectorType) -> Option<SymbolId> {
        self.iter()
            .filter(|symbol| symbol.lang_item.and_then(LangItem::vector_shape) == Some(shape))
            .map(|symbol| symbol.id)
            .min_by_key(|id| (id.file_id, id.local_id.0))
    }

    /// Type parameters of a named type, if any.
    #[must_use]
    pub fn type_params_of(&self, type_id: SymbolId) -> &[SymbolId] {
        self.type_params
            .get(&type_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn setup_prelude_scope(&mut self) {
        if self.global_scope_id == ScopeId(0) {
            let new_global = self.new_scope(ScopeId(0));
            self.global_scope_id = new_global;
        }
    }

    #[must_use]
    pub fn global_scope(&self) -> ScopeId {
        self.global_scope_id
    }

    /// Iterate scopes in stable ID order with their parent and ordered members.
    ///
    /// This narrow view supports deterministic fingerprints without exposing
    /// the mutable scope arena or its internal lookup operations.
    pub fn scope_layout(
        &self,
    ) -> impl Iterator<Item = (ScopeId, Option<ScopeId>, &[SymbolId])> + '_ {
        self.scopes.iter().enumerate().map(|(index, scope)| {
            (
                ScopeId(u32::try_from(index).unwrap_or(u32::MAX)),
                scope.parent,
                scope.symbols.as_slice(),
            )
        })
    }

    pub fn new_scope(&mut self, parent: ScopeId) -> ScopeId {
        let Ok(n) = u32::try_from(self.scopes.len()) else {
            crate::ice::bug("scope count overflow (u32::MAX scopes)");
        };
        let id = ScopeId(n);
        self.scopes.push(Scope {
            parent: Some(parent),
            symbols: Vec::new(),
        });
        id
    }

    #[must_use]
    pub fn get(&self, id: SymbolId) -> &Symbol {
        match self.try_get(id) {
            Some(s) => s,
            None => crate::ice::bug(&format!(
                "symbol id {id:?} not found in SymbolTable for file {}",
                self.file_id
            )),
        }
    }

    /// Safe lookup: local ids out of range or missing imports return `None`.
    #[must_use]
    pub fn try_get(&self, id: SymbolId) -> Option<&Symbol> {
        if id.file_id == self.file_id {
            if id.is_synthetic() {
                self.synthetic_symbols.get(&id.local_id.0)
            } else {
                self.symbols.get(id.local_id.0 as usize)
            }
        } else {
            self.imported_symbols.get(&id)
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Symbol> {
        self.iter_own().chain(self.imported_symbols.values())
    }

    /// Symbols of this table's own file, without imported ones.
    pub fn iter_own(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().chain(self.synthetic_symbols.values())
    }

    /// Deterministic flat symbol name used by native backends.
    ///
    /// The resolver records the defining module once. Import aliases never
    /// participate in this identity, so adding or renaming an import cannot
    /// change generated symbols or create a backend collision.
    #[must_use]
    pub fn host_func_name<'a>(&'a self, sym: &'a Symbol) -> &'a str {
        self.host_function_names
            .get(&sym.id)
            .map_or_else(|| sym.name.as_str(), SmolStr::as_str)
    }

    /// Whether `@Export` published this symbol to the linker.
    #[must_use]
    pub fn is_exported(&self, symbol: SymbolId) -> bool {
        self.exported_functions.contains(&symbol)
    }

    /// First pair of distinct `@Export`s that claim the same linker symbol,
    /// in deterministic order: `(first, second, name)`.
    ///
    /// Each module rejects a duplicate it can see while lowering, but two
    /// modules each export into their own table. Only the merged program
    /// table sees both, so every consumer of the export set (object emission
    /// and header generation) asks here instead of trusting the first writer.
    #[must_use]
    pub fn exported_symbol_collision(&self) -> Option<(SymbolId, SymbolId, &str)> {
        let mut named = self
            .exported_functions
            .iter()
            .filter_map(|symbol| {
                let name = self.host_function_names.get(symbol)?;
                Some((name.as_str(), *symbol))
            })
            .collect::<Vec<_>>();
        named.sort_unstable();
        named
            .windows(2)
            .find(|pair| pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1)
            .map(|pair| (pair[0].1, pair[1].1, pair[0].0))
    }

    #[must_use]
    pub fn lookup_value(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        self.lookup_with(scope, name, SymbolKind::is_value)
    }

    #[must_use]
    pub fn lookup_type(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        self.lookup_with(scope, name, SymbolKind::is_type)
    }

    #[must_use]
    pub fn lookup_module(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        self.lookup_with(scope, name, |kind| kind == SymbolKind::Module)
    }

    #[must_use]
    pub fn lookup_any(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            if let Some(symbol) = self.find_in_scope(scope_id, name) {
                return Some(symbol);
            }
            current = self.scope(scope_id).parent;
        }
        None
    }

    #[must_use]
    pub fn value_candidates(&self, scope: ScopeId) -> Vec<&Symbol> {
        self.candidates(scope, SymbolKind::is_value)
    }

    #[must_use]
    pub fn type_candidates(&self, scope: ScopeId) -> Vec<&Symbol> {
        self.candidates(scope, SymbolKind::is_type)
    }

    #[must_use]
    pub fn lookup_module_member(&self, module: &str, member: &str) -> Option<SymbolId> {
        self.module_members
            .get(&(SmolStr::new(module), SmolStr::new(member)))
            .copied()
    }

    #[must_use]
    pub fn lookup_associated_member(&self, parent_id: SymbolId, member: &str) -> Option<SymbolId> {
        self.associated_members
            .get(&(parent_id, SmolStr::new(member)))
            .copied()
    }

    fn lookup_with(
        &self,
        scope: ScopeId,
        name: &str,
        pred: fn(SymbolKind) -> bool,
    ) -> Option<SymbolId> {
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            for symbol_id in self.scope(scope_id).symbols.iter().rev() {
                let symbol = self.get(*symbol_id);
                if symbol.name == name && pred(symbol.kind) {
                    return Some(*symbol_id);
                }
            }
            current = self.scope(scope_id).parent;
        }
        None
    }

    fn candidates(&self, scope: ScopeId, pred: fn(SymbolKind) -> bool) -> Vec<&Symbol> {
        let mut out = Vec::new();
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            for symbol_id in &self.scope(scope_id).symbols {
                let symbol = self.get(*symbol_id);
                if pred(symbol.kind) {
                    out.push(symbol);
                }
            }
            current = self.scope(scope_id).parent;
        }
        out
    }

    #[must_use]
    pub fn find_in_scope(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        self.scope(scope)
            .symbols
            .iter()
            .copied()
            .find(|symbol_id| self.get(*symbol_id).name == name)
    }

    /// Whether either scope lexically contains the other.
    ///
    /// A rename to a name declared in a related scope can change which symbol
    /// an existing reference resolves to, even when the declarations are not
    /// direct duplicates in the same scope.
    #[must_use]
    pub fn scopes_are_related(&self, left: ScopeId, right: ScopeId) -> bool {
        self.is_scope_ancestor(left, right) || self.is_scope_ancestor(right, left)
    }

    fn is_scope_ancestor(&self, ancestor: ScopeId, mut scope: ScopeId) -> bool {
        loop {
            if scope == ancestor {
                return true;
            }
            let Some(parent) = self.scope(scope).parent else {
                return false;
            };
            scope = parent;
        }
    }

    #[must_use]
    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    pub(super) fn scope_mut(&mut self, id: ScopeId) -> &mut Scope {
        &mut self.scopes[id.0 as usize]
    }
}
