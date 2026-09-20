use super::primitive::Primitive;
use super::type_interner::{TypeId, TypeInterner};
use crate::hir::pool::IndexRange;
use crate::{SymbolId, SymbolTable};

/// Canonical internal type representation.
///
/// Variants that carry type-argument lists (`Named`, `Func`, `Tuple`) use
/// `IndexRange` (8 bytes) instead of `Vec<TypeId>` (24+ bytes). The actual
/// `TypeId` data lives in the `TypeInterner`'s contiguous `TypeArgsPool`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArType {
    /// Primitive types: int, float, bool, str, ...\
    Primitive(Primitive),

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

    /// Fixed-size array: `[4]float`
    Array(u64, TypeId),

    /// Fixed-size array whose length is a scalar const parameter before
    /// monomorphization (`[N]T`). Backends must only observe the substituted
    /// [`Self::Array`] form.
    ConstArray(SymbolId, TypeId),

    /// Concrete scalar const generic argument.
    Const(u64),

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

impl ArType {
    /// Construct a `Named` type, pushing generic args into the interner's pool.
    pub fn named(sym: SymbolId, args: &[TypeId], interner: &TypeInterner) -> Self {
        let range = interner.push_type_args(args);
        ArType::Named(sym, range)
    }

    /// Construct a `Func` type, pushing param types into the interner's pool.
    pub fn func(params: &[TypeId], ret: TypeId, interner: &TypeInterner) -> Self {
        let range = interner.push_type_args(params);
        ArType::Func(range, ret)
    }

    /// Construct a `Tuple` type, pushing element types into the interner's pool.
    pub fn tuple(elems: &[TypeId], interner: &TypeInterner) -> Self {
        let range = interner.push_type_args(elems);
        ArType::Tuple(range)
    }

    /// Borrow the type arguments for this variant from the interner's pool.
    /// Returns empty vec for non-argument-carrying variants.
    #[must_use]
    pub fn type_args(&self, interner: &TypeInterner) -> Vec<TypeId> {
        match self {
            ArType::Named(_, range) | ArType::Func(range, _) | ArType::Tuple(range) => {
                interner.type_args(*range)
            }
            _ => Vec::new(),
        }
    }
}

impl ArType {
    /// Returns true if this type is the poison type.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, ArType::Error)
    }

    /// Returns true for any numeric primitive type.
    #[must_use]
    pub fn is_numeric(&self) -> bool {
        match self {
            ArType::Primitive(p) => p.is_numeric(),
            ArType::IntLiteral | ArType::FloatLiteral => true,
            _ => false,
        }
    }

    /// Returns true for integer primitives.
    #[must_use]
    pub fn is_integer(&self) -> bool {
        match self {
            ArType::Primitive(p) => p.is_integer(),
            ArType::IntLiteral => true,
            _ => false,
        }
    }

    /// Returns true for float primitives.
    #[must_use]
    pub fn is_float(&self) -> bool {
        match self {
            ArType::Primitive(p) => p.is_float(),
            ArType::FloatLiteral => true,
            _ => false,
        }
    }

    /// Returns true if this is an unresolved literal type.
    #[must_use]
    pub fn is_literal(&self) -> bool {
        matches!(self, ArType::IntLiteral | ArType::FloatLiteral)
    }

    /// Returns true if this type is signed (signed integers, floats, or unresolved numeric literals).
    #[must_use]
    pub fn is_signed(&self) -> bool {
        match self {
            ArType::Primitive(p) => p.is_signed(),
            ArType::IntLiteral | ArType::FloatLiteral => true,
            _ => false,
        }
    }

    /// Returns true if this type has an intrinsic order (<, <=, >, >=).
    ///
    /// Specifically: numeric primitives (int, uint, float), unresolved numeric literals, and `char`.
    /// Does NOT include `bool`, `str`, `struct`, `tuple`, `ptr`, etc.
    #[must_use]
    pub fn is_orderable(&self) -> bool {
        match self {
            ArType::Primitive(p) => p.is_numeric() || matches!(p, Primitive::Char),
            ArType::IntLiteral | ArType::FloatLiteral => true,
            _ => false,
        }
    }

    /// Whether this type can be auto-formatted to `str` in ToStr v0.1
    /// (string interpolation and call arguments whose formal type is `str`).
    ///
    /// Includes `str` (identity), `bool`, `char`, all integers/floats (including
    /// unresolved literals). Does **not** include structs, enums, pointers, etc.
    #[must_use]
    pub fn is_to_str_v01(&self) -> bool {
        match self {
            ArType::Primitive(p) => {
                matches!(p, Primitive::Str | Primitive::Bool | Primitive::Char)
                    || p.is_integer()
                    || p.is_float()
            }
            ArType::IntLiteral | ArType::FloatLiteral => true,
            // `Err` is a message handle (NUL-terminated UTF-8 from `err.new`).
            ArType::Err => true,
            _ => false,
        }
    }

    /// Value types that live *behind* a pointer when wrapped in `T?`.
    ///
    /// Named/heap types (`Point?`) already are a null-or-object pointer, so they
    /// are **not** boxed again. Scalars (`int?`, `bool?`, …) are boxed so that
    /// payload `0` is distinct from `nil`.
    #[must_use]
    pub fn needs_nullable_box(&self) -> bool {
        matches!(
            self,
            ArType::Primitive(
                Primitive::Bool
                    | Primitive::Char
                    | Primitive::Byte
                    | Primitive::I8
                    | Primitive::U8
                    | Primitive::I16
                    | Primitive::U16
                    | Primitive::I32
                    | Primitive::U32
                    | Primitive::I64
                    | Primitive::U64
                    | Primitive::Int
                    | Primitive::Uint
                    | Primitive::F32
                    | Primitive::F64
                    | Primitive::Float
            ) | ArType::IntLiteral
                | ArType::FloatLiteral
        )
    }

    /// Exhaustively visit every direct child `TypeId` contained in this type.
    ///
    /// This method matches every `ArType` variant explicitly (no wildcard) so that
    /// adding any new variant to `ArType` immediately alerts the compiler to update
    /// all type hierarchy visitors.
    pub fn visit_child_type_ids<F>(&self, interner: &TypeInterner, mut f: F)
    where
        F: FnMut(TypeId),
    {
        match self {
            ArType::Primitive(_)
            | ArType::GenRef
            | ArType::Err
            | ArType::Void
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Error
            | ArType::Const(_)
            | ArType::ConstParam(_) => {}
            ArType::Named(_, args) => {
                for arg in interner.type_args(*args) {
                    f(arg);
                }
            }
            ArType::Func(params, ret) => {
                for param in interner.type_args(*params) {
                    f(param);
                }
                f(*ret);
            }
            ArType::Nullable(inner)
            | ArType::Slice(inner)
            | ArType::Array(_, inner)
            | ArType::ConstArray(_, inner)
            | ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Option(inner)
            | ArType::Coroutine(inner)
            | ArType::Poll(inner)
            | ArType::Range(inner) => {
                f(*inner);
            }
            ArType::Tuple(args) => {
                for arg in interner.type_args(*args) {
                    f(arg);
                }
            }
            ArType::Result(ok, err) => {
                f(*ok);
                f(*err);
            }
        }
    }

    /// Produce a human-readable name for this type.
    #[must_use]
    pub fn display(&self, symbols: &SymbolTable, interner: &TypeInterner) -> String {
        match self {
            ArType::Primitive(p) => p.to_string(),
            ArType::Named(id, args) => {
                let name = symbols.try_get(*id).map(|s| s.name.as_str()).unwrap_or("?");
                let arg_tys = interner.type_args(*args);
                if arg_tys.is_empty() {
                    name.to_string()
                } else {
                    let args_str: Vec<String> = arg_tys
                        .iter()
                        .map(|&a| interner.resolve(a).display(symbols, interner))
                        .collect();
                    format!("{}<{}>", name, args_str.join(", "))
                }
            }
            ArType::Func(params, ret) => {
                let param_tys = interner.type_args(*params);
                let params_str: Vec<String> = param_tys
                    .iter()
                    .map(|&p| interner.resolve(p).display(symbols, interner))
                    .collect();
                let ret_ty = interner.resolve(*ret);
                let is_void = matches!(ret_ty, ArType::Void);
                let ret_str = ret_ty.display(symbols, interner);
                if is_void {
                    format!("func({})", params_str.join(", "))
                } else {
                    format!("func({}) {}", params_str.join(", "), ret_str)
                }
            }
            ArType::Nullable(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("{}?", inner_str)
            }
            ArType::Slice(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("[]{}", inner_str)
            }
            ArType::Array(size, inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("[{}]{}", size, inner_str)
            }
            ArType::ConstArray(param, inner) => {
                let name = symbols
                    .try_get(*param)
                    .map(|symbol| symbol.name.as_str())
                    .unwrap_or("?");
                let inner = interner.resolve(*inner).display(symbols, interner);
                format!("[{name}]{inner}")
            }
            ArType::Const(value) => value.to_string(),
            ArType::ConstParam(param) => symbols
                .try_get(*param)
                .map(|symbol| symbol.name.to_string())
                .unwrap_or_else(|| "?".to_string()),
            ArType::Ptr(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("ptr[{}]", inner_str)
            }
            ArType::Ref(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("ref {}", inner_str)
            }
            ArType::RefMut(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("mut ref {}", inner_str)
            }
            ArType::GenRef => "GenRef".to_string(),
            ArType::Tuple(range) => {
                let type_tys = interner.type_args(*range);
                let parts: Vec<String> = type_tys
                    .iter()
                    .map(|&t| interner.resolve(t).display(symbols, interner))
                    .collect();
                format!("({})", parts.join(", "))
            }
            ArType::Result(ok, err) => {
                let ok_str = interner.resolve(*ok).display(symbols, interner);
                let err_str = interner.resolve(*err).display(symbols, interner);
                format!("Result<{}, {}>", ok_str, err_str)
            }
            ArType::Option(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("Option<{}>", inner_str)
            }
            ArType::Coroutine(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("Coroutine<{}>", inner_str)
            }
            ArType::Poll(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("Poll<{}>", inner_str)
            }
            ArType::Range(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("Range<{}>", inner_str)
            }
            ArType::Err => "Err".to_string(),
            ArType::Void => "void".to_string(),
            ArType::IntLiteral => "int".to_string(),
            ArType::FloatLiteral => "float".to_string(),
            ArType::Error => "<error>".to_string(),
        }
    }

    /// Resolve a literal type to a concrete type. If the type is a literal
    /// and no context is available, defaults to `int` or `float`.
    #[must_use]
    pub fn default_literal(&self) -> ArType {
        match self {
            ArType::IntLiteral => ArType::Primitive(Primitive::Int),
            ArType::FloatLiteral => ArType::Primitive(Primitive::Float),
            other => other.clone(),
        }
    }

    /// Check if this literal type can absorb the given target type.
    /// Used for literal context-absorption (Swift-style).
    #[must_use]
    pub fn literal_absorbs(&self, target: &ArType) -> bool {
        match (self, target) {
            // IntLiteral absorbs any numeric type
            (ArType::IntLiteral, ArType::Primitive(p)) => p.is_numeric(),
            // FloatLiteral absorbs any float type
            (ArType::FloatLiteral, ArType::Primitive(p)) => p.is_float(),
            _ => false,
        }
    }

    /// Leaf / non-structural copy predicate (no named-field lookup).
    ///
    /// For named structs / POD aggregates, use `TypeInfo::is_copy` in typeck
    /// (structural: all scalar fields → auto-copy; ptr fields → not copy).
    #[must_use]
    pub fn is_copy_v01(&self) -> bool {
        match self {
            ArType::Primitive(p) => {
                p.is_numeric()
                    || matches!(
                        p,
                        Primitive::Bool
                            | Primitive::Char
                            | Primitive::Byte
                            | Primitive::Str
                            | Primitive::Any
                    )
            }
            ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Const(_)
            | ArType::Ptr(_)
            | ArType::Nullable(_)
            | ArType::Ref(_)
            | ArType::Slice(_)
            | ArType::GenRef => true,
            ArType::Error | ArType::Void | ArType::Err => true,
            // Named / aggregates: structural decision needs `TypeInfo::is_copy`.
            ArType::Named(_, _)
            | ArType::Func(_, _)
            | ArType::Array(_, _)
            | ArType::ConstArray(_, _)
            | ArType::ConstParam(_)
            | ArType::Tuple(_)
            | ArType::Result(_, _)
            | ArType::Option(_)
            | ArType::Coroutine(_)
            | ArType::Poll(_)
            | ArType::Range(_)
            | ArType::RefMut(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolTable;
    use crate::types::Primitive;

    fn empty_symbols() -> SymbolTable {
        SymbolTable::new(0)
    }

    fn new_interner() -> TypeInterner {
        TypeInterner::new()
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
        let span = arandu_lexer::Span::new(0, 0, 0);
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
        let span = arandu_lexer::Span::new(0, 0, 0);
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
        let span = arandu_lexer::Span::new(0, 0, 0);
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
        let span = arandu_lexer::Span::new(0, 0, 0);
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
        let span = arandu_lexer::Span::new(0, 0, 0);
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
        assert_eq!(ArType::Array(4, int_tid).display(&syms, &i), "[4]int");
        assert_eq!(ArType::Ptr(int_tid).display(&syms, &i), "ptr[int]");
    }

    #[test]
    fn display_tuple_result_option() {
        let mut syms = empty_symbols();
        let span = arandu_lexer::Span::new(0, 0, 0);
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
