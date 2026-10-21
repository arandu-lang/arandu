use super::super::primitive::Primitive;
use super::super::type_interner::{TypeId, TypeInterner};
use super::ArType;
use crate::SymbolId;

impl ArType {
    /// Read a non-negative integer constant without erasing its typed identity.
    #[must_use]
    pub fn as_const_u64(&self) -> Option<u64> {
        match self {
            Self::Const(value) => Some(*value),
            Self::FrozenConst(value) => match value.as_ref() {
                crate::ctfe::ConstValue::Integer(value) => u64::try_from(value.value()).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns the element type when this value uses the slice-view ABI.
    ///
    /// A shared or exclusive borrow of a slice denotes a borrow of the
    /// dynamically-sized sequence, not a pointer to a `{ data, len }`
    /// descriptor. Consequently `[]T`, `ref []T`, and `mut ref []T` all use
    /// the same two-word ABI; ownership and exclusivity remain compile-time
    /// facts.
    #[must_use]
    pub fn slice_abi_element(&self, interner: &TypeInterner) -> Option<TypeId> {
        match self {
            Self::Slice(element) | Self::MutSlice(element) => Some(*element),
            Self::Ref(inner) | Self::RefMut(inner) => interner.with_type(*inner, |inner| {
                if let Self::Slice(element) | Self::MutSlice(element) = inner {
                    Some(*element)
                } else {
                    None
                }
            }),
            _ => None,
        }
    }

    /// Whether this is specifically a borrowed slice view (`ref []T` or
    /// `mut ref []T`), excluding an ordinary `[]T` value.
    #[must_use]
    pub fn is_borrowed_slice_abi(&self, interner: &TypeInterner) -> bool {
        matches!(self, Self::Ref(_) | Self::RefMut(_)) && self.slice_abi_element(interner).is_some()
    }

    /// Number of reference indirections before a string descriptor. A plain
    /// `str` has depth zero; raw pointers keep pointer-comparison semantics.
    #[must_use]
    pub fn str_reference_depth(&self, interner: &TypeInterner) -> Option<usize> {
        let mut ty = self.clone();
        for depth in 0..=256 {
            match ty {
                Self::Primitive(Primitive::Str) => return Some(depth),
                Self::Ref(inner) | Self::RefMut(inner) => ty = interner.resolve(inner),
                _ => return None,
            }
        }
        None
    }

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
