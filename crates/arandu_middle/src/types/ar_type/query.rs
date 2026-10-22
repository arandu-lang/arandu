use super::super::primitive::Primitive;
use super::super::type_interner::{TypeId, TypeInterner};
use super::ArType;
use crate::SymbolTable;

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
            ArType::Vector(_)
                | ArType::Primitive(
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
                )
                | ArType::IntLiteral
                | ArType::FloatLiteral
        )
    }

    #[must_use]
    pub fn is_u8_or_byte(&self) -> bool {
        matches!(self, ArType::Primitive(Primitive::U8 | Primitive::Byte))
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
            | ArType::Vector(_)
            | ArType::GenRef
            | ArType::Err
            | ArType::Void
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Error
            | ArType::FrozenConst(_)
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
            | ArType::MutSlice(inner)
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
            ArType::Vector(vector) => vector.name().to_string(),
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
            ArType::MutSlice(inner) => {
                let inner_str = interner.resolve(*inner).display(symbols, interner);
                format!("mut []{}", inner_str)
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
            ArType::FrozenConst(value) => match value.as_ref() {
                crate::ctfe::ConstValue::Bool(value) => value.to_string(),
                crate::ctfe::ConstValue::Integer(value) => value.value().to_string(),
                crate::ctfe::ConstValue::Aggregate(value) => value
                    .shape()
                    .intern(interner)
                    .ok()
                    .map(|id| interner.resolve(id).display(symbols, interner))
                    .unwrap_or_else(|| "constant".into()),
                _ => "constant".into(),
            },
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
            ArType::Vector(_) => true,
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
            | ArType::FrozenConst(_)
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
            | ArType::MutSlice(_)
            | ArType::RefMut(_) => false,
        }
    }
}
