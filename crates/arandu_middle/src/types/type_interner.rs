//! Type Interning — Canonical Type Identity via `TypeId`
//!
//! This module provides a `TypeInterner` that maps every `ArType` to a unique
//! `TypeId(u32)`. Interning serves several purposes:
//!
//! 1. **O(1) type equality** — comparing `TypeId`s is a single integer compare
//!    instead of a recursive structural walk.
//! 2. **Deduplication** — identical recursive types are stored only once.
//! 3. **Cache-friendliness** — downstream passes (monomorphization, codegen)
//!    can use `TypeId` as a dense index into `IndexVec`-backed tables.
//!
//! ## Design
//!
//! The interner uses a two-level scheme:
//! - A `HashMap<ArType, TypeId>` for dedup lookup.
//! - A `Vec<ArType>` (indexed by `TypeId`) for id→type resolution.
//!
//! Interning is additive and append-only; types are never removed.

use super::ar_type::ArType;
use super::primitive::Primitive;
use crate::SymbolTable;
use crate::hir::pool::IndexRange;
use crate::newtype_index;
use rustc_hash::FxHashMap;
use std::sync::RwLock;

newtype_index!(TypeId);

/// Arena-backed storage for type argument lists with deduplication.
///
/// Each `ArType::Named(SymbolId, Vec<TypeId>)` becomes `ArType::Named(SymbolId, IndexRange)`.
/// Arguments are stored contiguously here; identical sequences dedup to the same `IndexRange`.
#[derive(Debug, Clone, Default)]
pub struct TypeArgsPool {
    args: Vec<TypeId>,
    seen: FxHashMap<Vec<TypeId>, u32>,
}

impl TypeArgsPool {
    fn push_args_dedup(&mut self, args: &[TypeId]) -> IndexRange {
        if args.is_empty() {
            return IndexRange::empty();
        }
        debug_assert!(args.len() <= u32::MAX as usize, "type args list overflow");
        if let Some(&start) = self.seen.get(args) {
            return IndexRange {
                start,
                len: args.len() as u32,
            };
        }
        debug_assert!(
            self.args.len() <= u32::MAX as usize,
            "type args pool overflow"
        );
        let start = self.args.len() as u32;
        self.args.extend_from_slice(args);
        self.seen.insert(args.to_vec(), start);
        IndexRange {
            start,
            len: args.len() as u32,
        }
    }

    #[must_use]
    fn get(&self, range: IndexRange) -> &[TypeId] {
        &self.args[range.range()]
    }

    fn try_get(&self, range: IndexRange) -> Option<&[TypeId]> {
        self.args.get(range.range())
    }
}

/// A global interner that assigns a unique `TypeId` to every structural `ArType`.
#[derive(Debug)]
pub struct TypeInterner {
    /// Forward map: ArType → TypeId  (deduplication).
    map: RwLock<FxHashMap<ArType, TypeId>>,
    /// Reverse map: TypeId → ArType  (resolution).
    types: RwLock<Vec<ArType>>,
    /// Arena-backed type argument storage.
    type_args_pool: RwLock<TypeArgsPool>,
}

impl TypeInterner {
    #[must_use]
    pub fn new() -> Self {
        let interner = Self {
            map: RwLock::new(FxHashMap::default()),
            types: RwLock::new(Vec::new()),
            type_args_pool: RwLock::new(TypeArgsPool::default()),
        };
        // Pre-intern all Primitive variants
        let primitives = [
            Primitive::Int,
            Primitive::Uint,
            Primitive::Float,
            Primitive::I8,
            Primitive::I16,
            Primitive::I32,
            Primitive::I64,
            Primitive::U8,
            Primitive::U16,
            Primitive::U32,
            Primitive::U64,
            Primitive::F32,
            Primitive::F64,
            Primitive::Bool,
            Primitive::Byte,
            Primitive::Char,
            Primitive::Str,
            Primitive::Any,
        ];
        for prim in primitives {
            interner.intern(ArType::Primitive(prim));
        }
        // Pre-intern Void, Err, Error, IntLiteral, FloatLiteral
        interner.intern(ArType::Void);
        interner.intern(ArType::Err);
        interner.intern(ArType::Error);
        interner.intern(ArType::IntLiteral);
        interner.intern(ArType::FloatLiteral);

        interner
    }

    /// Intern a type, returning its canonical `TypeId`.
    /// If the type has been interned before, returns the same id.
    pub fn intern(&self, ty: ArType) -> TypeId {
        self.intern_ref(&ty)
    }

    /// Intern without requiring ownership of `ty` up front.
    /// Clones `ty` only on the cold path (first insert).
    pub fn intern_ref(&self, ty: &ArType) -> TypeId {
        if let Some(&id) = self.map.read().unwrap_or_else(|e| e.into_inner()).get(ty) {
            return id;
        }

        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        let mut types = self.types.write().unwrap_or_else(|e| e.into_inner());

        // Double checked locking
        if let Some(&id) = map.get(ty) {
            return id;
        }

        let id = TypeId::from_usize(types.len());
        map.insert(ty.clone(), id);
        types.push(ty.clone());
        id
    }

    /// Push type arguments into the contiguous pool (with dedup) and return the backing range.
    /// Identical argument sequences always produce the same `IndexRange`.
    #[must_use]
    pub fn push_type_args(&self, args: &[TypeId]) -> IndexRange {
        self.type_args_pool
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push_args_dedup(args)
    }

    /// Borrow the type arguments for a given range from the contiguous pool.
    #[must_use]
    pub fn type_args(&self, range: IndexRange) -> Vec<TypeId> {
        self.type_args_pool
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(range)
            .to_vec()
    }

    /// Borrow a contiguous type-argument range for the duration of `f`.
    ///
    /// Prefer this over [`Self::type_args`] in hashing and codegen hot paths;
    /// it avoids allocating a temporary `Vec<TypeId>`.
    #[inline]
    pub fn with_type_args<R>(&self, range: IndexRange, f: impl FnOnce(&[TypeId]) -> R) -> R {
        let pool = self
            .type_args_pool
            .read()
            .unwrap_or_else(|error| error.into_inner());
        f(pool.get(range))
    }

    /// Return one argument from a contiguous type-argument range without
    /// materializing the whole range as a temporary `Vec`.
    #[must_use]
    #[inline]
    pub fn type_arg(&self, range: IndexRange, index: usize) -> Option<TypeId> {
        self.type_args_pool
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .try_get(range)?
            .get(index)
            .copied()
    }

    /// Resolve a `TypeId` back to its `ArType` (clones the interned value).
    ///
    /// Prefer [`Self::with_type`] / [`Self::is_copy_v01`] when you only need a temporary view —
    /// those avoid an `ArType` heap clone.
    ///
    /// # Panics
    /// Panics if `id` was not produced by this interner.
    #[must_use]
    pub fn resolve(&self, id: TypeId) -> ArType {
        self.types.read().unwrap_or_else(|e| e.into_inner())[id.as_usize()].clone()
    }

    /// Borrow the interned type for the duration of `f` (no `ArType` clone).
    #[inline]
    pub fn with_type<R>(&self, id: TypeId, f: impl FnOnce(&ArType) -> R) -> R {
        let types = self.types.read().unwrap_or_else(|e| e.into_inner());
        f(&types[id.as_usize()])
    }

    /// Whether the interned type is copy under v0.1 rules (no clone of `ArType`).
    #[must_use]
    pub fn is_copy_v01(&self, id: TypeId) -> bool {
        self.with_type(id, ArType::is_copy_v01)
    }

    /// Whether `id` is the poison [`ArType::Error`] (no clone).
    #[must_use]
    pub fn is_error(&self, id: TypeId) -> bool {
        self.with_type(id, ArType::is_error)
    }

    /// Returns the element type when `id` uses the two-word slice-view ABI.
    /// This performs one interner read even for `ref []T` / `mut ref []T`, so
    /// hot codegen paths do not clone types or recursively acquire the lock.
    #[must_use]
    pub fn slice_abi_element(&self, id: TypeId) -> Option<TypeId> {
        let types = self.types.read().unwrap_or_else(|error| error.into_inner());
        match &types[id.as_usize()] {
            ArType::Slice(element) => Some(*element),
            ArType::Ref(inner) | ArType::RefMut(inner) => match &types[inner.as_usize()] {
                ArType::Slice(element) => Some(*element),
                _ => None,
            },
            _ => None,
        }
    }

    /// Canonical id for [`ArType::Error`] in this interner (pre-interned in [`Self::new`]).
    #[must_use]
    pub fn error_type_id(&self) -> TypeId {
        self.intern(ArType::Error)
    }

    /// Stable id of pre-interned [`ArType::Error`] for any interner built with [`Self::new`].
    ///
    /// All fresh interners share the same pre-intern order, so this id is comparable across
    /// them (used by HIR invariant checks that only store `TypeId`s).
    #[must_use]
    pub fn preinterned_error_id() -> TypeId {
        use std::sync::OnceLock;
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| TypeInterner::new().error_type_id())
    }

    /// Pre-interned primitive id (same index for every [`Self::new`] interner).
    #[must_use]
    pub fn preinterned_primitive(p: Primitive) -> TypeId {
        use std::sync::OnceLock;
        static CACHE: OnceLock<TypeInterner> = OnceLock::new();
        CACHE
            .get_or_init(TypeInterner::new)
            .intern(ArType::Primitive(p))
    }

    /// Try to resolve a `TypeId`, returning `None` if out of range.
    #[must_use]
    pub fn try_resolve(&self, id: TypeId) -> Option<ArType> {
        self.types
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id.as_usize())
            .cloned()
    }

    /// Look up a type without interning it. Returns `None` if the type
    /// has never been interned.
    #[must_use]
    pub fn lookup(&self, ty: &ArType) -> Option<TypeId> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(ty)
            .copied()
    }

    /// Number of unique types interned so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.types.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Returns `true` if no types have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Display a `TypeId` using the symbol table for named types.
    #[must_use]
    pub fn display(&self, id: TypeId, symbols: &SymbolTable) -> String {
        self.resolve(id).display(symbols, self)
    }
}

impl Default for TypeInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for TypeInterner {
    fn clone(&self) -> Self {
        Self {
            map: std::sync::RwLock::new(self.map.read().unwrap_or_else(|e| e.into_inner()).clone()),
            types: std::sync::RwLock::new(
                self.types.read().unwrap_or_else(|e| e.into_inner()).clone(),
            ),
            type_args_pool: std::sync::RwLock::new(
                self.type_args_pool
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Primitive;

    #[test]
    fn test_intern_returns_same_id_for_same_type() {
        let interner = TypeInterner::new();
        let id1 = interner.intern(ArType::Primitive(Primitive::Int));
        let id2 = interner.intern(ArType::Primitive(Primitive::Int));
        assert_eq!(id1, id2);
        assert_eq!(interner.len(), 23);
    }

    #[test]
    fn test_intern_returns_different_id_for_different_types() {
        let interner = TypeInterner::new();
        let id1 = interner.intern(ArType::Primitive(Primitive::Int));
        let id2 = interner.intern(ArType::Primitive(Primitive::Bool));
        assert_ne!(id1, id2);
        assert_eq!(interner.len(), 23);
    }

    #[test]
    fn test_resolve_roundtrip() {
        let interner = TypeInterner::new();
        let str_id = interner.intern(ArType::Primitive(Primitive::Str));
        let ty = ArType::Nullable(str_id);
        let id = interner.intern(ty.clone());
        assert_eq!(interner.resolve(id), ty);
    }

    #[test]
    fn test_lookup_returns_none_for_unknown_type() {
        let interner = TypeInterner::new();
        let dummy_id = TypeId::from_usize(999);
        assert_eq!(interner.lookup(&ArType::Ptr(dummy_id)), None);
    }

    #[test]
    fn test_lookup_returns_id_after_intern() {
        let interner = TypeInterner::new();
        assert!(interner.lookup(&ArType::Void).is_some());
    }

    #[test]
    fn test_complex_recursive_type_dedup() {
        let interner = TypeInterner::new();
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let err_id = interner.intern(ArType::Err);
        let result_ty = ArType::Result(int_id, err_id);
        let id1 = interner.intern(result_ty.clone());
        let id2 = interner.intern(result_ty);
        assert_eq!(id1, id2);
        assert_eq!(interner.len(), 24); // 23 pre-interned + 1 Result type
    }

    #[test]
    fn test_func_type_interning() {
        let interner = TypeInterner::new();
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let str_id = interner.intern(ArType::Primitive(Primitive::Str));
        let bool_id = interner.intern(ArType::Primitive(Primitive::Bool));
        let func_ty = ArType::func(&[int_id, str_id], bool_id, &interner);
        let id = interner.intern(func_ty.clone());
        assert_eq!(interner.resolve(id), func_ty);
    }

    #[test]
    fn test_all_primitives_get_unique_ids() {
        let interner = TypeInterner::new();
        let prims = [
            Primitive::Int,
            Primitive::Uint,
            Primitive::Float,
            Primitive::Bool,
            Primitive::Str,
            Primitive::Char,
            Primitive::Byte,
            Primitive::Any,
        ];
        let ids: Vec<TypeId> = prims
            .iter()
            .map(|&p| interner.intern(ArType::Primitive(p)))
            .collect();
        for (i, a) in ids.iter().enumerate() {
            for (j, b) in ids.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        a, b,
                        "primitives {:?} and {:?} got same TypeId",
                        prims[i], prims[j]
                    );
                }
            }
        }
        assert_eq!(interner.len(), 23);
    }
}
