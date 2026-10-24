use super::primitive::Primitive;
use super::type_interner::TypeId;
use crate::SymbolId;
use crate::hir::pool::IndexRange;

/// Canonical internal type representation.
///
/// Variants that carry type-argument lists (`Named`, `Func`, `Tuple`) use
/// `IndexRange` (8 bytes) instead of `Vec<TypeId>` (24+ bytes). The actual
/// `TypeId` data lives in the `TypeInterner`'s contiguous `TypeArgsPool`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArType {
    /// Primitive types: int, float, bool, str, ...\
    Primitive(Primitive),

    /// One of the six fixed-width, 128-bit hardware vector shapes.
    Vector(super::VectorType),

    /// Named type with optional generic arguments: `User`, `List<int>`
    /// The `IndexRange` points into `TypeInterner::type_args_pool`.
    Named(SymbolId, IndexRange),

    /// Function type: func(int, str) bool
    /// The `IndexRange` points into `TypeInterner::type_args_pool`.
    Func(IndexRange, TypeId),

    /// Nullable wrapper: str?
    Nullable(TypeId),

    /// Slice type: []int
    Slice(TypeId),

    /// Exclusive mutable slice type: mut []int
    MutSlice(TypeId),

    /// Fixed-size array: `[4]float`
    Array(u64, TypeId),

    /// Fixed-size array whose length is a scalar const parameter before
    /// monomorphization (`[N]T`). Backends must only observe the substituted
    /// [`Self::Array`] form.
    ConstArray(SymbolId, TypeId),

    /// Concrete scalar const generic argument.
    Const(u64),
    FrozenConst(std::sync::Arc<crate::ctfe::ConstValue>),

    /// Reference to a scalar const generic parameter inside a generic type.
    ConstParam(SymbolId),

    /// Pointer type: `ptr[Vec2]` — raw, unsafe to deref without `unsafe`
    Ptr(TypeId),

    /// Shared reference: `&T` — safe first-class borrow (F2.0)
    Ref(TypeId),

    /// Exclusive reference: `&mut T` — safe first-class borrow (F2.0)
    RefMut(TypeId),

    /// Generational handle `{index:u32, generation:u32}` (F2.3.runtime / GenArena).
    /// ABI: 8 bytes; not a raw pointer — payload lives in `std.alloc.gen_arena`.
    GenRef,

    /// Multi-value tuple (non-`Result` returns only)
    /// The `IndexRange` points into `TypeInterner::type_args_pool`.
    Tuple(IndexRange),

    /// `Result<T, E>` — canonical success/error type
    Result(TypeId, TypeId),

    /// `Option<T>` — algebraic optional. SYN.3: `nil` fills this type as `.None`.
    /// `T?` remains [`ArType::Nullable`] (safe-nav / heap handle; see strategic plan §2.1).
    Option(TypeId),

    /// `Coroutine[T]` — coroutine machine state type returning T
    Coroutine(TypeId),

    /// `Poll[T]` — one poll step: Ready(T) | Pending (A3.6 / std.core.future)
    Poll(TypeId),

    /// `Range<T>` — representing ranges like 0..n
    Range(TypeId),

    /// The `Err` type from the grammar
    Err,

    /// Functions with no return type
    Void,

    /// An integer literal that hasn't been assigned a concrete type yet.
    /// Absorbs the context type during check mode.
    IntLiteral,

    /// A float literal that hasn't been assigned a concrete type yet.
    /// Absorbs any float context (f32, f64, float).
    FloatLiteral,

    /// Poison type — operations on Error never produce new errors.
    /// This is the key to preventing cascading error messages.
    Error,
}

mod construct;
mod query;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolTable;
    use crate::types::{Primitive, TypeInterner};

    fn empty_symbols() -> SymbolTable {
        SymbolTable::new(0)
    }

    fn new_interner() -> TypeInterner {
        TypeInterner::new()
    }

    #[test]
    fn constant_u64_projection_is_checked_and_preserves_typed_identity() {
        use crate::ctfe::{ConstInt, ConstValue, IntegerType};
        let layout = crate::DataLayout::ptr_width(8);
        for (primitive, value, expected) in [
            (Primitive::I64, -1, None),
            (Primitive::I64, 0, Some(0)),
            (Primitive::U8, 2, Some(2)),
            (Primitive::U64, i128::from(u64::MAX), Some(u64::MAX)),
        ] {
            let integer =
                ConstInt::new(IntegerType::new(primitive, layout).unwrap(), value).unwrap();
            let constant = std::sync::Arc::new(ConstValue::Integer(integer));
            let ty = ArType::FrozenConst(std::sync::Arc::clone(&constant));
            assert_eq!(ty.as_const_u64(), expected);
            assert_eq!(
                constant.type_shape(),
                super::super::shape::TypeShape::Primitive(primitive)
            );
        }
        assert_eq!(ArType::Const(4).as_const_u64(), Some(4));
        assert_eq!(
            ArType::FrozenConst(std::sync::Arc::new(ConstValue::Bool(true))).as_const_u64(),
            None
        );
        assert_eq!(ArType::Void.as_const_u64(), None);
    }

    // ── is_error ──

    #[test]
    fn error_type_is_error() {
        assert!(ArType::Error.is_error());
    }

    #[test]
    fn non_error_types_are_not_error() {
        assert!(!ArType::Primitive(Primitive::Int).is_error());
        assert!(!ArType::Void.is_error());
        assert!(!ArType::Err.is_error());
        assert!(!ArType::IntLiteral.is_error());
    }

    // ── is_numeric ──

    #[test]
    fn primitives_is_numeric() {
        for p in &[
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
            Primitive::Byte,
        ] {
            assert!(
                ArType::Primitive(*p).is_numeric(),
                "{p:?} should be numeric"
            );
        }
    }

    #[test]
    fn non_numeric_is_not_numeric() {
        assert!(!ArType::Primitive(Primitive::Bool).is_numeric());
        assert!(!ArType::Primitive(Primitive::Str).is_numeric());
        assert!(!ArType::Primitive(Primitive::Char).is_numeric());
        assert!(!ArType::Void.is_numeric());
    }

    #[test]
    fn literals_are_numeric() {
        assert!(ArType::IntLiteral.is_numeric());
        assert!(ArType::FloatLiteral.is_numeric());
    }

    // ── is_integer ──

    #[test]
    fn int_literal_is_integer() {
        assert!(ArType::IntLiteral.is_integer());
        assert!(!ArType::FloatLiteral.is_integer());
    }

    // ── is_float ──

    #[test]
    fn float_literal_is_float() {
        assert!(ArType::FloatLiteral.is_float());
        assert!(!ArType::IntLiteral.is_float());
    }

    // ── is_literal ──

    #[test]
    fn literal_types() {
        assert!(ArType::IntLiteral.is_literal());
        assert!(ArType::FloatLiteral.is_literal());
        assert!(!ArType::Primitive(Primitive::Int).is_literal());
        assert!(!ArType::Void.is_literal());
    }

    // ── is_to_str_v01 ──

    #[test]
    fn to_str_v01_primitives() {
        assert!(ArType::Primitive(Primitive::Str).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::Bool).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::Char).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::Int).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::Uint).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::Float).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::I8).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::U64).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::F32).is_to_str_v01());
        assert!(ArType::Primitive(Primitive::Byte).is_to_str_v01());
        assert!(ArType::IntLiteral.is_to_str_v01());
        assert!(ArType::FloatLiteral.is_to_str_v01());
        assert!(ArType::Err.is_to_str_v01());
    }

    #[test]
    fn to_str_v01_rejects_non_primitives() {
        assert!(!ArType::Primitive(Primitive::Any).is_to_str_v01());
        assert!(!ArType::Void.is_to_str_v01());
        assert!(!ArType::Error.is_to_str_v01());
        let interner = new_interner();
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        assert!(!ArType::Named(crate::SymbolId::new(0, 1), IndexRange::empty()).is_to_str_v01());
        assert!(!ArType::Slice(int_id).is_to_str_v01());
        assert!(!ArType::Ptr(int_id).is_to_str_v01());
        assert!(!ArType::Array(4, int_id).is_to_str_v01());
        assert!(!ArType::Option(int_id).is_to_str_v01());
        assert!(!ArType::Result(int_id, int_id).is_to_str_v01());
        assert!(!ArType::func(&[int_id], int_id, &interner).is_to_str_v01());
    }

    // ── default_literal ──

    #[test]
    fn default_int_literal() {
        assert_eq!(
            ArType::IntLiteral.default_literal(),
            ArType::Primitive(Primitive::Int)
        );
    }

    #[test]
    fn default_float_literal() {
        assert_eq!(
            ArType::FloatLiteral.default_literal(),
            ArType::Primitive(Primitive::Float)
        );
    }

    #[test]
    fn default_non_literal_returns_self() {
        assert_eq!(ArType::Void.default_literal(), ArType::Void);
        assert_eq!(ArType::Err.default_literal(), ArType::Err);
    }

    // ── literal_absorbs ──

    #[test]
    fn int_literal_absorbs_numeric() {
        assert!(ArType::IntLiteral.literal_absorbs(&ArType::Primitive(Primitive::Int)));
        assert!(ArType::IntLiteral.literal_absorbs(&ArType::Primitive(Primitive::U32)));
        assert!(ArType::IntLiteral.literal_absorbs(&ArType::Primitive(Primitive::Byte)));
        assert!(!ArType::IntLiteral.literal_absorbs(&ArType::Primitive(Primitive::Bool)));
        assert!(!ArType::IntLiteral.literal_absorbs(&ArType::Primitive(Primitive::Str)));
    }

    #[test]
    fn float_literal_absorbs_floats() {
        assert!(ArType::FloatLiteral.literal_absorbs(&ArType::Primitive(Primitive::Float)));
        assert!(ArType::FloatLiteral.literal_absorbs(&ArType::Primitive(Primitive::F32)));
        assert!(ArType::FloatLiteral.literal_absorbs(&ArType::Primitive(Primitive::F64)));
        assert!(!ArType::FloatLiteral.literal_absorbs(&ArType::Primitive(Primitive::Int)));
        assert!(!ArType::FloatLiteral.literal_absorbs(&ArType::Primitive(Primitive::Bool)));
    }

    #[test]
    fn non_literal_does_not_absorb() {
        assert!(
            !ArType::Primitive(Primitive::Int).literal_absorbs(&ArType::Primitive(Primitive::Int))
        );
        assert!(!ArType::Void.literal_absorbs(&ArType::Primitive(Primitive::Int)));
    }

    // ── is_copy_v01 ──

    #[test]
    fn numeric_primitives_are_copy() {
        assert!(ArType::Primitive(Primitive::Int).is_copy_v01());
        assert!(ArType::Primitive(Primitive::Float).is_copy_v01());
        assert!(ArType::Primitive(Primitive::U32).is_copy_v01());
    }

    #[test]
    fn bool_char_byte_any_are_copy() {
        assert!(ArType::Primitive(Primitive::Bool).is_copy_v01());
        assert!(ArType::Primitive(Primitive::Char).is_copy_v01());
        assert!(ArType::Primitive(Primitive::Byte).is_copy_v01());
        assert!(ArType::Primitive(Primitive::Any).is_copy_v01());
    }

    #[test]
    fn str_is_copy() {
        assert!(ArType::Primitive(Primitive::Str).is_copy_v01());
    }

    #[test]
    fn literals_ptr_nullable_are_copy() {
        assert!(ArType::IntLiteral.is_copy_v01());
        assert!(ArType::FloatLiteral.is_copy_v01());
        let i = new_interner();
        assert!(ArType::Ptr(i.intern(ArType::Primitive(Primitive::Int))).is_copy_v01());
        assert!(ArType::Nullable(i.intern(ArType::Primitive(Primitive::Int))).is_copy_v01());
        // Shared refs are Copy; exclusive refs preserve uniqueness by moving.
        assert!(ArType::Ref(i.intern(ArType::Primitive(Primitive::Int))).is_copy_v01());
        assert!(!ArType::RefMut(i.intern(ArType::Primitive(Primitive::Int))).is_copy_v01());
    }

    #[test]
    fn error_void_err_are_copy() {
        assert!(ArType::Error.is_copy_v01());
        assert!(ArType::Void.is_copy_v01());
        assert!(ArType::Err.is_copy_v01());
    }

    #[test]
    fn named_func_slice_array_tuple_are_not_copy() {
        assert!(!ArType::Named(SymbolId::new(0, 1), IndexRange::empty()).is_copy_v01());
        let i = new_interner();
        let int = i.intern(ArType::Primitive(Primitive::Int));
        assert!(!ArType::func(&[int], int, &i).is_copy_v01());
        assert!(ArType::Slice(int).is_copy_v01());
        assert!(!ArType::MutSlice(int).is_copy_v01());
        assert!(!ArType::Array(3, int).is_copy_v01());
        assert!(!ArType::tuple(&[int], &i).is_copy_v01());
        assert!(!ArType::Result(int, int).is_copy_v01());
        assert!(!ArType::Option(int).is_copy_v01());
        assert!(!ArType::Coroutine(int).is_copy_v01());
        assert!(!ArType::Range(int).is_copy_v01());
    }

    // ── display ──

    #[test]
    fn display_primitives() {
        let syms = empty_symbols();
        let i = new_interner();
        assert_eq!(ArType::Primitive(Primitive::Int).display(&syms, &i), "int");
        assert_eq!(
            ArType::Primitive(Primitive::Bool).display(&syms, &i),
            "bool"
        );
        assert_eq!(ArType::Primitive(Primitive::Str).display(&syms, &i), "str");
        assert_eq!(ArType::Primitive(Primitive::F64).display(&syms, &i), "f64");
    }

    #[test]
    fn display_named_no_args() {
        let mut syms = empty_symbols();
        let span = crate::Span::new(0, 0, 0);
        let id = syms
            .define(
                syms.global_scope(),
                "MyStruct",
                crate::SymbolKind::Struct,
                span,
            )
            .unwrap();
        let i = new_interner();
        assert_eq!(
            ArType::Named(id, IndexRange::empty()).display(&syms, &i),
            "MyStruct"
        );
    }

    #[test]
    fn display_named_with_generic_args() {
        let mut syms = empty_symbols();
        let span = crate::Span::new(0, 0, 0);
        let list_id = syms
            .define(syms.global_scope(), "List", crate::SymbolKind::Struct, span)
            .unwrap();
        let int_id = syms
            .define(
                syms.global_scope(),
                "int",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let i = new_interner();
        let int_tid = i.intern(ArType::Named(int_id, IndexRange::empty()));
        assert_eq!(
            ArType::named(list_id, &[int_tid], &i).display(&syms, &i),
            "List<int>"
        );
    }

    #[test]
    fn display_func_type() {
        let mut syms = empty_symbols();
        let span = crate::Span::new(0, 0, 0);
        let sym = syms
            .define(
                syms.global_scope(),
                "int",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let i = new_interner();
        let int_tid = i.intern(ArType::Named(sym, IndexRange::empty()));
        let ret = i.intern(ArType::Void);
        let f = ArType::func(&[int_tid, int_tid], ret, &i);
        assert_eq!(f.display(&syms, &i), "func(int, int)");
    }

    #[test]
    fn display_func_with_return() {
        let mut syms = empty_symbols();
        let span = crate::Span::new(0, 0, 0);
        let int_sym = syms
            .define(
                syms.global_scope(),
                "int",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let bool_sym = syms
            .define(
                syms.global_scope(),
                "bool",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let i = new_interner();
        let int_tid = i.intern(ArType::Named(int_sym, IndexRange::empty()));
        let bool_tid = i.intern(ArType::Named(bool_sym, IndexRange::empty()));
        let f = ArType::func(&[int_tid], bool_tid, &i);
        assert_eq!(f.display(&syms, &i), "func(int) bool");
    }

    #[test]
    fn display_nullable_slice_array_ptr() {
        let mut syms = empty_symbols();
        let span = crate::Span::new(0, 0, 0);
        let int_sym = syms
            .define(
                syms.global_scope(),
                "int",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let i = new_interner();
        let int_tid = i.intern(ArType::Named(int_sym, IndexRange::empty()));
        assert_eq!(ArType::Nullable(int_tid).display(&syms, &i), "int?");
        assert_eq!(ArType::Slice(int_tid).display(&syms, &i), "[]int");
        assert_eq!(ArType::MutSlice(int_tid).display(&syms, &i), "mut []int");
        assert_eq!(ArType::Array(4, int_tid).display(&syms, &i), "[4]int");
        assert_eq!(ArType::Ptr(int_tid).display(&syms, &i), "ptr[int]");
    }

    #[test]
    fn display_tuple_result_option() {
        let mut syms = empty_symbols();
        let span = crate::Span::new(0, 0, 0);
        let int_sym = syms
            .define(
                syms.global_scope(),
                "int",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let str_sym = syms
            .define(
                syms.global_scope(),
                "str",
                crate::SymbolKind::TypeAlias,
                span,
            )
            .unwrap();
        let i = new_interner();
        let int_tid = i.intern(ArType::Named(int_sym, IndexRange::empty()));
        let str_tid = i.intern(ArType::Named(str_sym, IndexRange::empty()));
        assert_eq!(
            ArType::tuple(&[int_tid, str_tid], &i).display(&syms, &i),
            "(int, str)"
        );
        assert_eq!(
            ArType::Result(int_tid, str_tid).display(&syms, &i),
            "Result<int, str>"
        );
        assert_eq!(ArType::Option(int_tid).display(&syms, &i), "Option<int>");
    }

    #[test]
    fn display_coroutine_range_err_void_literals() {
        let syms = empty_symbols();
        let i = new_interner();
        let int_tid = i.intern(ArType::Primitive(Primitive::Int));
        assert_eq!(
            ArType::Coroutine(int_tid).display(&syms, &i),
            "Coroutine<int>"
        );
        assert_eq!(ArType::Range(int_tid).display(&syms, &i), "Range<int>");
        assert_eq!(ArType::Err.display(&syms, &i), "Err");
        assert_eq!(ArType::Void.display(&syms, &i), "void");
        assert_eq!(ArType::IntLiteral.display(&syms, &i), "int");
        assert_eq!(ArType::FloatLiteral.display(&syms, &i), "float");
        assert_eq!(ArType::Error.display(&syms, &i), "<error>");
    }

    #[test]
    fn display_named_missing_symbol_no_ice() {
        let i = new_interner();
        let foreign = SymbolId::new(99, 0); // not in empty table
        let empty = empty_symbols();
        let int_tid = i.intern(ArType::Primitive(Primitive::Int));
        let ty = ArType::named(foreign, &[int_tid], &i);
        let s = ty.display(&empty, &i);
        assert!(s.contains('?'), "got {s}");
        assert!(s.contains("int"), "got {s}");
    }

    #[test]
    fn test_visit_child_type_ids_exhaustive() {
        let i = new_interner();
        let int_tid = i.intern(ArType::Primitive(Primitive::Int));
        let bool_tid = i.intern(ArType::Primitive(Primitive::Bool));
        let mut visited = Vec::new();
        ArType::Result(int_tid, bool_tid).visit_child_type_ids(&i, |tid| visited.push(tid));
        assert_eq!(visited, vec![int_tid, bool_tid]);

        let mut visited_arr = Vec::new();
        ArType::Array(5, int_tid).visit_child_type_ids(&i, |tid| visited_arr.push(tid));
        assert_eq!(visited_arr, vec![int_tid]);
    }
}
