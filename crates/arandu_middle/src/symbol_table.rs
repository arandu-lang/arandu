use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use arandu_lexer::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LocalSymbolId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId {
    pub file_id: crate::db::FileId,
    pub local_id: LocalSymbolId,
}

impl SymbolId {
    pub const DUMMY: Self = Self {
        file_id: 0,
        local_id: LocalSymbolId(u32::MAX),
    };

    pub fn new(file_id: crate::db::FileId, local_id: u32) -> Self {
        Self {
            file_id,
            local_id: LocalSymbolId(local_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Module,
    ImportValue,
    ImportType,
    Func,
    Const,
    TypeAlias,
    Struct,
    Enum,
    Interface,
    ExternFunc,
    Param,
    Local,
    Field,
    EnumVariant,
    TypeParam,
    ConstParam,
    NamespaceMember,
    AssociatedFunc,
}

impl SymbolKind {
    #[must_use]
    pub fn is_value(self) -> bool {
        matches!(
            self,
            SymbolKind::ImportValue
                | SymbolKind::Func
                | SymbolKind::Const
                | SymbolKind::ExternFunc
                | SymbolKind::Param
                | SymbolKind::Local
                | SymbolKind::EnumVariant
                | SymbolKind::ConstParam
        )
    }

    #[must_use]
    pub fn is_type(self) -> bool {
        matches!(
            self,
            SymbolKind::ImportType
                | SymbolKind::TypeAlias
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::TypeParam
                | SymbolKind::ConstParam
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LangItem {
    Option,
    OptionSome,
    OptionNone,
    Result,
    ResultOk,
    ResultErr,
    Poll,
    PollReady,
    PollPending,
    Coroutine,
    String,
    Vec,
    Str,
    Slice,
    Char,
    Int,
    Float,
    Ptr,
    Copy,
    Send,
    Sync,
    /// Cooperative executor handle; its opaque ID is not a transfer proof.
    TaskHandle,
    Alloc,
    Free,
    U32,
    U64,
    /// Bit intrinsic explicitly declared with the compiler intrinsic ABI.
    /// Spelling alone must not replace an ordinary or foreign function call.
    BitIntrinsic,
    /// Atomic operation explicitly declared with the compiler intrinsic ABI.
    AtomicIntrinsic,
    /// Closed native SIMD types from the canonical core module.
    VectorF32x4,
    VectorF64x2,
    VectorI32x4,
    VectorI16x8,
    VectorU8x16,
    VectorI64x2,
    /// SIMD operations explicitly declared with the intrinsic ABI.
    SimdIntrinsic,
    UnsafeCell,
    RwLock,
    Mutex,
    RwLockReadGuard,
    RwLockWriteGuard,
    MutexGuard,
    Condvar,
    Arc,
    ArcWeak,
    ThreadJob,
    ThreadSpawn,
    ThreadParkTimeout,
    ThreadPark,
    ThreadUnpark,
}

impl LangItem {
    #[must_use]
    pub const fn vector_shape(self) -> Option<crate::types::VectorType> {
        use crate::types::VectorType;
        match self {
            Self::VectorF32x4 => Some(VectorType::F32x4),
            Self::VectorF64x2 => Some(VectorType::F64x2),
            Self::VectorI32x4 => Some(VectorType::I32x4),
            Self::VectorI16x8 => Some(VectorType::I16x8),
            Self::VectorU8x16 => Some(VectorType::U8x16),
            Self::VectorI64x2 => Some(VectorType::I64x2),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: SmolStr,
    pub kind: SymbolKind,
    pub span: Span,
    pub scope: ScopeId,
    /// Source-level visibility retained after resolution for export filtering.
    pub visibility: arandu_parser::Visibility,
    /// Canonical core item classification (zero-indirection, 1 byte).
    pub lang_item: Option<LangItem>,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    symbols: Vec<SymbolId>,
}

impl Scope {
    #[must_use]
    pub fn symbols(&self) -> &[SymbolId] {
        &self.symbols
    }
}

/// Flat, contiguous struct of canonical core type symbols.
/// Stored inline in SymbolTable without heap indirection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuiltinTypes {
    pub option: Option<SymbolId>,
    pub result: Option<SymbolId>,
    pub poll: Option<SymbolId>,
    pub coroutine: Option<SymbolId>,
    pub string: Option<SymbolId>,
    pub vec: Option<SymbolId>,
    pub str_type: Option<SymbolId>,
    pub slice_type: Option<SymbolId>,
    pub char_type: Option<SymbolId>,
    pub int_type: Option<SymbolId>,
    pub float_type: Option<SymbolId>,
    pub ptr_type: Option<SymbolId>,
    pub u32_type: Option<SymbolId>,
    pub u64_type: Option<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct SymbolTable {
    pub file_id: crate::db::FileId,
    scopes: Vec<Scope>,
    symbols: Vec<Symbol>,
    pub imported_symbols: FxHashMap<SymbolId, Symbol>,
    pub module_members: FxHashMap<(SmolStr, SmolStr), SymbolId>,
    /// Module aliases whose source import failed. Semantic passes use this
    /// error state to avoid diagnosing members of the same failed import.
    pub unresolved_module_aliases: std::collections::BTreeSet<SmolStr>,
    pub associated_members: FxHashMap<(SymbolId, SmolStr), SymbolId>,
    /// Interface symbols declared with the `sealed` modifier.
    pub sealed_interfaces: FxHashSet<SymbolId>,
    /// Explicit type/interface implementation declarations.
    pub interface_implementations: FxHashSet<(SymbolId, SymbolId)>,
    /// Canonical backend identities of functions and nominal type arguments,
    /// computed once from defining module paths, never from import aliases.
    pub host_function_names: FxHashMap<SymbolId, SmolStr>,
    /// Functions invoked by tooling rather than source calls (`@Test`, `@Benchmark`).
    pub tooling_entrypoints: FxHashSet<SymbolId>,
    pub module_scopes: FxHashMap<SymbolId, ScopeId>,
    /// Type-parameter symbols for named types (`struct` / `enum` / …), in declaration order.
    /// Used so methods on `Box<T>` can import `T` into their type scope.
    pub type_params: FxHashMap<SymbolId, smallvec::SmallVec<[SymbolId; 4]>>,
    global_scope_id: ScopeId,
    pub builtins: BuiltinTypes,
    pub builtin_alloc: Option<SymbolId>,
    pub builtin_free: Option<SymbolId>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new(0)
    }
}

impl SymbolTable {
    #[must_use]
    pub fn new(file_id: crate::db::FileId) -> Self {
        Self {
            file_id,
            scopes: vec![Scope {
                parent: None,
                symbols: Vec::new(),
            }],
            symbols: Vec::new(),
            imported_symbols: FxHashMap::default(),
            module_members: FxHashMap::default(),
            unresolved_module_aliases: std::collections::BTreeSet::new(),
            associated_members: FxHashMap::default(),
            sealed_interfaces: FxHashSet::default(),
            interface_implementations: FxHashSet::default(),
            host_function_names: FxHashMap::default(),
            tooling_entrypoints: FxHashSet::default(),
            module_scopes: FxHashMap::default(),
            type_params: FxHashMap::default(),
            global_scope_id: ScopeId(0),
            builtins: BuiltinTypes::default(),
            builtin_alloc: None,
            builtin_free: None,
        }
    }

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
            SymbolId {
                file_id: self.file_id,
                local_id: LocalSymbolId(old_symbol.local_id.0 + self_symbols_len),
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
        if self.builtins.ptr_type.is_none() {
            self.builtins.ptr_type = other.builtins.ptr_type.map(map_symbol);
        }
    }

    /// Canonical owner for native vector methods. Imported aliases may provide
    /// several identities; select a stable ID rather than hash-table order.
    #[must_use]
    pub fn vector_type_symbol(&self, shape: crate::types::VectorType) -> Option<SymbolId> {
        self.iter()
            .filter(|symbol| symbol.lang_item.and_then(LangItem::vector_shape) == Some(shape))
            .map(|symbol| symbol.id)
            .min_by_key(|id| (id.file_id, id.local_id.0))
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
        self.builtin_alloc = valid(self.builtin_alloc);
        self.builtin_free = valid(self.builtin_free);
    }

    pub fn set_lang_item(&mut self, sym: SymbolId, item: LangItem) {
        if sym.file_id == self.file_id {
            if let Some(s) = self.symbols.get_mut(sym.local_id.0 as usize) {
                s.lang_item = Some(item);
            }
        } else if let Some(s) = self.imported_symbols.get_mut(&sym) {
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

    /// Type parameters of a named type, if any.
    #[must_use]
    pub fn type_params_of(&self, type_id: SymbolId) -> &[SymbolId] {
        self.type_params
            .get(&type_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
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
        self.define_with_visibility(scope, name, kind, span, arandu_parser::Visibility::Module)
    }

    /// Define a symbol while preserving its full source visibility.
    pub fn define_with_visibility(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: SymbolKind,
        span: Span,
        visibility: arandu_parser::Visibility,
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
                self.imported_symbols.insert(symbol.id, symbol);
                return Ok(Some(existing));
            }
            if existing == symbol.id {
                return Ok(None); // exact same symbol already imported
            }
            return Err(existing);
        }

        let id = symbol.id;
        self.imported_symbols.insert(id, symbol);
        self.scope_mut(self.global_scope_id).symbols.push(id);
        Ok(None)
    }

    /// Registers a symbol from another file so it can be looked up by ID,
    /// but DOES NOT put it into the global scope.
    pub fn register_imported_symbol(&mut self, symbol: Symbol) {
        self.imported_symbols.insert(symbol.id, symbol);
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
            self.symbols.get(id.local_id.0 as usize)
        } else {
            self.imported_symbols.get(&id)
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().chain(self.imported_symbols.values())
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
            arandu_parser::Visibility::Public,
        )?;
        self.module_members
            .insert((module.into(), member.into()), id);
        Ok(id)
    }

    #[must_use]
    pub fn lookup_module_member(&self, module: &str, member: &str) -> Option<SymbolId> {
        self.module_members
            .get(&(SmolStr::new(module), SmolStr::new(member)))
            .copied()
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
            arandu_parser::Visibility::Module,
        )
    }

    pub fn define_associated_member_with_visibility(
        &mut self,
        parent_id: SymbolId,
        member: &str,
        span: Span,
        visibility: arandu_parser::Visibility,
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

    fn scope_mut(&mut self, id: ScopeId) -> &mut Scope {
        &mut self.scopes[id.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const S: Span = Span::new(0, 0, 0);

    #[test]
    fn new_table_has_global_scope() {
        let table = SymbolTable::new(0);
        assert_eq!(table.global_scope(), ScopeId(0));
    }

    #[test]
    fn define_and_get_symbol() {
        let mut table = SymbolTable::new(0);
        let id = table
            .define(ScopeId(0), "foo", SymbolKind::Func, S)
            .unwrap();
        assert_eq!(id, SymbolId::new(0, 0));
        assert_eq!(table.get(id).name, "foo");
        assert_eq!(table.get(id).kind, SymbolKind::Func);
    }

    #[test]
    fn define_duplicate_in_same_scope_fails() {
        let mut table = SymbolTable::new(0);
        table
            .define(ScopeId(0), "dup", SymbolKind::Const, S)
            .unwrap();
        let result = table.define(ScopeId(0), "dup", SymbolKind::Const, S);
        assert!(result.is_err());
    }

    #[test]
    fn lookup_any_walks_scope_chain() {
        let mut table = SymbolTable::new(0);
        let outer = ScopeId(0);
        let inner = table.new_scope(outer);
        table.define(outer, "a", SymbolKind::Const, S).unwrap();
        assert!(table.lookup_any(inner, "a").is_some());
    }

    #[test]
    fn lookup_any_not_found() {
        let table = SymbolTable::new(0);
        assert!(table.lookup_any(ScopeId(0), "nonexistent").is_none());
    }

    #[test]
    fn lookup_value_skips_type_symbols() {
        let mut table = SymbolTable::new(0);
        table
            .define(ScopeId(0), "MyType", SymbolKind::Struct, S)
            .unwrap();
        assert!(table.lookup_value(ScopeId(0), "MyType").is_none());
        assert!(table.lookup_type(ScopeId(0), "MyType").is_some());
    }

    #[test]
    fn new_scope_increases_scope_count() {
        let mut table = SymbolTable::new(0);
        assert_eq!(table.scopes.len(), 1);
        let child = table.new_scope(ScopeId(0));
        assert_eq!(child, ScopeId(1));
        assert_eq!(table.scopes.len(), 2);
    }

    #[test]
    fn module_members_basic() {
        let mut table = SymbolTable::new(0);
        let id = table.define_module_member("mod1", "foo", S).unwrap();
        assert_eq!(table.lookup_module_member("mod1", "foo"), Some(id));
        assert_eq!(table.lookup_module_member("mod1", "bar"), None);
    }

    #[test]
    fn associated_members_basic() {
        let mut table = SymbolTable::new(0);
        let struct_id = table
            .define(ScopeId(0), "MyStruct", SymbolKind::Struct, S)
            .unwrap();
        let id = table
            .define_associated_member(struct_id, "method", S)
            .unwrap();
        assert_eq!(
            table.lookup_associated_member(struct_id, "method"),
            Some(id)
        );
    }

    #[test]
    fn value_candidates_filters_type() {
        let mut table = SymbolTable::new(0);
        table
            .define(ScopeId(0), "fn1", SymbolKind::Func, S)
            .unwrap();
        table
            .define(ScopeId(0), "St", SymbolKind::Struct, S)
            .unwrap();
        let vals = table.value_candidates(ScopeId(0));
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].name, "fn1");
    }

    #[test]
    fn type_candidates_filters_value() {
        let mut table = SymbolTable::new(0);
        table
            .define(ScopeId(0), "fn1", SymbolKind::Func, S)
            .unwrap();
        table
            .define(ScopeId(0), "St", SymbolKind::Struct, S)
            .unwrap();
        let types = table.type_candidates(ScopeId(0));
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "St");
    }

    #[test]
    fn merge_from_basic() {
        let mut table1 = SymbolTable::new(0);
        let mut table2 = SymbolTable::new(0);
        let _id = table2
            .define(ScopeId(0), "other", SymbolKind::Const, S)
            .unwrap();
        let len_before = table1.symbols.len();
        table1.merge_from(table2);
        assert_eq!(table1.symbols.len(), len_before + 1);
        assert!(table1.lookup_any(ScopeId(0), "other").is_some());
    }

    #[test]
    fn iter_includes_all_symbols() {
        let mut table = SymbolTable::new(0);
        table.define(ScopeId(0), "a", SymbolKind::Func, S).unwrap();
        table.define(ScopeId(0), "b", SymbolKind::Const, S).unwrap();
        let names: Vec<&str> = table.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn kind_classification() {
        assert!(SymbolKind::Func.is_value());
        assert!(!SymbolKind::Func.is_type());
        assert!(SymbolKind::Struct.is_type());
        assert!(!SymbolKind::Struct.is_value());
        assert!(SymbolKind::Local.is_value());
        assert!(SymbolKind::TypeParam.is_type());
    }
}
