use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LocalSymbolId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId {
    pub file_id: crate::db::FileId,
    pub local_id: LocalSymbolId,
}

impl SymbolId {
    /// High bit of [`LocalSymbolId`]. Body checkers mint these ids from a
    /// span so parallel item typeck cannot collide, and they never occupy a
    /// dense slot in [`SymbolTable`]'s source vector.
    pub const SYNTHETIC_LOCAL_BIT: u32 = 0x8000_0000;

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

    #[must_use]
    pub const fn is_synthetic(self) -> bool {
        self.local_id.0 & Self::SYNTHETIC_LOCAL_BIT != 0
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
    /// Other compiler operations explicitly declared with the intrinsic ABI.
    CompilerIntrinsic,
    /// Canonical `std.testing.blackBox` optimization barrier.
    BlackBox,
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
    /// Any other `std.thread` operation. Bare-metal targets reject the module.
    StdThread,
    /// Any `std.fs` operation. Bare-metal targets reject the module.
    StdFs,
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
    pub visibility: crate::Visibility,
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
    /// Span-derived symbols that are not dense source locals. Empty for every
    /// program that does not synthesize a thread capture.
    synthetic_symbols: FxHashMap<u32, Symbol>,
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
    /// Functions published to the linker by `@Export`. Every other definition
    /// keeps internal linkage, so a module never leaks symbols it did not
    /// explicitly export.
    pub exported_functions: FxHashSet<SymbolId>,
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
            synthetic_symbols: FxHashMap::default(),
            imported_symbols: FxHashMap::default(),
            module_members: FxHashMap::default(),
            unresolved_module_aliases: std::collections::BTreeSet::new(),
            associated_members: FxHashMap::default(),
            sealed_interfaces: FxHashSet::default(),
            interface_implementations: FxHashSet::default(),
            host_function_names: FxHashMap::default(),
            exported_functions: FxHashSet::default(),
            tooling_entrypoints: FxHashSet::default(),
            module_scopes: FxHashMap::default(),
            type_params: FxHashMap::default(),
            global_scope_id: ScopeId(0),
            builtins: BuiltinTypes::default(),
            builtin_alloc: None,
            builtin_free: None,
        }
    }
}

mod define;
mod lookup;

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
