//! Cranelift IR type helpers.
//!
//! Provides the mapping between Arandu [`ArType`]s and the Cranelift
//! [`Type`] system used to build function signatures and emit IR values.

use arandu_semantics::passes::type_checker::types::ArType;
use arandu_semantics::passes::type_checker::types::Primitive;
use cranelift_codegen::ir::Type;
use cranelift_codegen::ir::types::*;

/// A Cranelift IR type, extended with a `Void` variant.
///
/// Cranelift itself has no void type; this wrapper carries `Void` for
/// Arandu types that produce no value (e.g. `()`, error types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClifType {
    /// A concrete Cranelift scalar type.
    Concrete(Type),
    /// Represents no value (void return / unused slot).
    Void,
}

impl ClifType {
    /// Returns the inner [`Type`] if this is `Concrete`, or `None` for `Void`.
    #[must_use]
    pub fn concrete(self) -> Option<Type> {
        match self {
            Self::Concrete(ty) => Some(ty),
            Self::Void => None,
        }
    }
}

/// Returns `true` if `ty` is an unsigned integer Arandu primitive.
///
/// Used to select zero-extension vs sign-extension for narrowing casts.
#[must_use]
pub fn ar_type_is_unsigned_integer(ty: &ArType) -> bool {
    matches!(
        ty,
        ArType::Primitive(p) if matches!(
            p,
            Primitive::Uint
                | Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::Byte
        )
    )
}

/// Returns `true` if `ty` should use unsigned comparison in Cranelift (IntCC::Unsigned*).
/// Includes unsigned integers, char (Unicode code points), and raw pointers (address space).
#[must_use]
pub fn ar_type_is_unsigned_comparison(ty: &ArType) -> bool {
    ar_type_is_unsigned_integer(ty)
        || matches!(
            ty,
            ArType::Primitive(Primitive::Char)
                | ArType::Ptr(_)
                | ArType::Ref(_)
                | ArType::RefMut(_)
        )
}

/// Maps an Arandu [`ArType`] to a single [`ClifType`].
///
/// For composite types that expand to multiple Cranelift slots (e.g. `str`),
/// this returns the *primary* slot type only. Use [`clif_types`] when you need
/// the full slot list for ABI purposes.
#[must_use]
pub fn clif_type(ty: &ArType, ptr_type: Type) -> ClifType {
    clif_type_with_float(ty, ptr_type, F64)
}

/// Maps an Arandu [`ArType`] to a single [`ClifType`], respecting the configured `float_type`
/// from target layout (`F32` or `F64`).
#[must_use]
pub fn clif_type_with_float(ty: &ArType, ptr_type: Type, float_type: Type) -> ClifType {
    match ty {
        ArType::Primitive(p) => match p {
            Primitive::Int | Primitive::Uint => ClifType::Concrete(ptr_type),
            Primitive::Float => ClifType::Concrete(float_type),
            Primitive::I8 | Primitive::U8 | Primitive::Byte => ClifType::Concrete(I8),
            Primitive::I16 | Primitive::U16 => ClifType::Concrete(I16),
            Primitive::I32 | Primitive::U32 => ClifType::Concrete(I32),
            Primitive::I64 | Primitive::U64 => ClifType::Concrete(I64),
            Primitive::F32 => ClifType::Concrete(F32),
            Primitive::F64 => ClifType::Concrete(F64),
            Primitive::Bool => ClifType::Concrete(I8),
            Primitive::Char => ClifType::Concrete(I32),
            Primitive::Str => {
                // Single-slot fallback (ptr only). ABI/multi-value uses `clif_types` →
                // `[ptr_type, ptr_type]` matching LayoutEngine fat pointer (RC-STR-ABI).
                ClifType::Concrete(ptr_type)
            }
            Primitive::Any => ClifType::Concrete(ptr_type),
        },
        ArType::Ptr(_)
        | ArType::Ref(_)
        | ArType::RefMut(_)
        | ArType::Nullable(_)
        | ArType::Slice(_)
        | ArType::Array(_, _)
        | ArType::ConstArray(_, _) => ClifType::Concrete(ptr_type),
        // Packed GenRef: always 8-byte {u32,u32} (I64 on all hosts we JIT).
        ArType::GenRef => ClifType::Concrete(I64),
        // `Err` is a message handle (pointer to UTF-8 buffer from `err.new`).
        ArType::Err => ClifType::Concrete(ptr_type),
        ArType::Void | ArType::Error | ArType::Const(_) | ArType::ConstParam(_) => ClifType::Void,
        ArType::IntLiteral => ClifType::Concrete(ptr_type),
        ArType::FloatLiteral => ClifType::Concrete(float_type),
        ArType::Named(_, _) => {
            // TODO: Named types (structs, enums) should use a proper multi-value ABI.
            // Currently mapped to a pointer for JIT passing.
            ClifType::Concrete(ptr_type)
        }
        ArType::Func(_, _) => ClifType::Concrete(ptr_type),
        ArType::Tuple(_)
        | ArType::Result(_, _)
        | ArType::Option(_)
        | ArType::Coroutine(_)
        | ArType::Poll(_)
        | ArType::Range(_) => {
            // Composite types map to pointers for JIT passing.
            ClifType::Concrete(ptr_type)
        }
    }
}

/// Maps an Arandu [`ArType`] to the full list of Cranelift [`Type`]s needed
/// to represent it in an ABI slot.
///
/// Most types map to a single slot. `str` is special: it expands to
/// `[ptr_type, I64]` (pointer + length), matching the `ArStr` fat-pointer layout.
#[must_use]
pub fn clif_types(ty: &ArType, ptr_type: Type) -> Vec<Type> {
    clif_types_with_float(ty, ptr_type, F64)
}

/// Maps an Arandu [`ArType`] to the full list of Cranelift [`Type`]s needed
/// to represent it in an ABI slot, respecting the target `float_type`.
#[must_use]
pub fn clif_types_with_float(ty: &ArType, ptr_type: Type, float_type: Type) -> Vec<Type> {
    match ty {
        ArType::Primitive(Primitive::Str) | ArType::Slice(_) => vec![ptr_type, ptr_type],
        _ => match clif_type_with_float(ty, ptr_type, float_type) {
            ClifType::Concrete(t) => vec![t],
            ClifType::Void => vec![],
        },
    }
}

/// Returns the number of Cranelift ABI slots required to pass `ty`.
///
/// `str` requires 2 slots (ptr + len); void types require 0; everything else 1.
#[must_use]
pub fn clif_slot_count(ty: &ArType) -> usize {
    match ty {
        ArType::Primitive(Primitive::Str) | ArType::Slice(_) => 2,
        ArType::Void | ArType::Error => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_length_matches_pointer_width() {
        let ptr_type_32 = cranelift_codegen::ir::types::I32;
        let str_ty = arandu_semantics::passes::type_checker::types::ArType::Primitive(
            arandu_semantics::passes::type_checker::types::Primitive::Str,
        );
        let clif_tys = clif_types(&str_ty, ptr_type_32);
        assert_eq!(clif_tys, vec![ptr_type_32, ptr_type_32]);
    }

    #[test]
    fn slice_abi_is_two_pointer_sized_slots_on_32_and_64_bit_targets() {
        let interner = arandu_semantics::passes::type_checker::types::TypeInterner::new();
        let element = interner.intern(ArType::Primitive(Primitive::U8));
        let slice = ArType::Slice(element);

        assert_eq!(clif_types(&slice, I32), vec![I32, I32]);
        assert_eq!(clif_types(&slice, I64), vec![I64, I64]);
        assert_eq!(clif_slot_count(&slice), 2);
        assert_eq!(clif_type(&slice, I32), ClifType::Concrete(I32));
        assert_eq!(clif_type(&slice, I64), ClifType::Concrete(I64));
    }
}
