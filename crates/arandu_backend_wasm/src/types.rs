//! Mapping from Arandu types to WebAssembly value types.
//!
//! Arandu values occupy one or more wasm stack slots — the wasm32 ABI
//! represents every Arandu value as a fixed number of slots:
//!
//! * `Empty` — no wasm value (`Void`, `Err`, `Error`, `Any`).
//! * `Scalar` — a single value. Scalar primitives map onto `i32`/`i64`/`f32`/
//!   `f64`; aggregates, references, pointers and handles are represented by
//!   the *address* of their backing memory cell, which is one `i32` slot.
//! * `Fat` — a two-slot fat pointer `(i32, i32)` (`str`, `Slice`, `Range` and
//!   `GenRef` carry data pointer + length / generation).
//!
//! The multi-value shape is resolved via `TypeInterner::with_type` so the
//! backend never clones an `ArType` on the hot path.

use arandu_middle::layout::DataLayout;
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
use wasm_encoder::ValType;

/// How many wasm stack slots a value type occupies in the wasm32 ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Empty,
    Scalar,
    Fat,
}

/// The `ValType` used for every slot of a fat pointer.
#[must_use]
fn fat_slot_type() -> ValType {
    ValType::I32
}

/// Number of slots a value of this type occupies.
#[must_use]
pub fn shape(ty: TypeId, interner: &TypeInterner, _layout: DataLayout) -> Shape {
    if interner.slice_abi_element(ty).is_some() {
        return Shape::Fat;
    }
    interner.with_type(ty, |ar| match ar {
        ArType::Primitive(p) => match p {
            Primitive::Str => Shape::Fat,
            Primitive::Any => Shape::Empty,
            _ => Shape::Scalar,
        },
        ArType::Range(_) | ArType::GenRef => Shape::Fat,
        ArType::Void | ArType::Err | ArType::Error => Shape::Empty,
        _ => Shape::Scalar,
    })
}

/// `ValType` of one slot of the value (see [`shape`] for slot semantics).
/// `slot` is 0-based; `None` for `Empty`.
#[must_use]
pub fn slot_valtype(
    ty: TypeId,
    slot: usize,
    interner: &TypeInterner,
    layout: DataLayout,
) -> Option<ValType> {
    let scope = shape(ty, interner, layout);
    match scope {
        Shape::Empty => None,
        Shape::Fat => Some(fat_slot_type()),
        Shape::Scalar => {
            if slot != 0 {
                None
            } else {
                interner.with_type(ty, |ar| scalar_valtype(ar, layout))
            }
        }
    }
}

fn scalar_valtype(ty: &ArType, layout: DataLayout) -> Option<ValType> {
    match ty {
        ArType::Primitive(p) => primitive_scalar_valtype(*p, layout),
        ArType::IntLiteral => Some(ValType::I32),
        ArType::FloatLiteral => Some(ValType::F64),
        // Aggregates (by address), refs, pointers, handles: i32 (ptr_width=4).
        _ => Some(ValType::I32),
    }
}

fn primitive_scalar_valtype(p: Primitive, layout: DataLayout) -> Option<ValType> {
    match p {
        Primitive::Bool
        | Primitive::I8
        | Primitive::U8
        | Primitive::I16
        | Primitive::U16
        | Primitive::I32
        | Primitive::U32
        | Primitive::Byte
        | Primitive::Char => Some(ValType::I32),
        Primitive::I64 | Primitive::U64 => Some(ValType::I64),
        Primitive::F32 => Some(ValType::F32),
        Primitive::F64 => Some(ValType::F64),
        Primitive::Int | Primitive::Uint => {
            if layout.pointer_width() <= 4 {
                Some(ValType::I32)
            } else {
                Some(ValType::I64)
            }
        }
        Primitive::Float => Some(ValType::F64),
        Primitive::Str | Primitive::Any => None,
    }
}

/// Compute the wasm value types for an [`ArType`].
#[must_use]
pub fn ar_type_valtypes(ty: &ArType, interner: &TypeInterner, layout: DataLayout) -> Vec<ValType> {
    if ty.slice_abi_element(interner).is_some() {
        return vec![ValType::I32, ValType::I32];
    }
    match ty {
        ArType::Void | ArType::Err | ArType::Error | ArType::Primitive(Primitive::Any) => vec![],
        ArType::Primitive(Primitive::Str) | ArType::Range(_) | ArType::GenRef => {
            vec![ValType::I32, ValType::I32]
        }
        _ => match scalar_valtype(ty, layout) {
            Some(vt) => vec![vt],
            None => vec![],
        },
    }
}

/// Whether the integer type is unsigned (selects unsigned div/rem/shift).
#[must_use]
pub fn ar_is_unsigned(ty: TypeId, interner: &TypeInterner) -> bool {
    interner.with_type(ty, |ar| match ar {
        ArType::Primitive(p) => {
            matches!(
                p,
                Primitive::U8 | Primitive::U16 | Primitive::U32 | Primitive::U64 | Primitive::Uint
            ) || *p == Primitive::Byte
        }
        _ => false,
    })
}

/// Whether the value is a floating-point type.
#[must_use]
pub fn ar_is_float(ty: TypeId, interner: &TypeInterner) -> bool {
    interner.with_type(ty, |ar| {
        matches!(
            ar,
            ArType::Primitive(Primitive::F32 | Primitive::F64 | Primitive::Float)
                | ArType::FloatLiteral
        )
    })
}

/// Whether a type has an integer scalar representation suitable for integer
/// width conversion. Aggregates and pointers are intentionally excluded.
#[must_use]
pub fn ar_is_integer(ty: TypeId, interner: &TypeInterner) -> bool {
    interner.with_type(ty, |ar| match ar {
        ArType::IntLiteral => true,
        ArType::Primitive(p) => matches!(
            p,
            Primitive::I8
                | Primitive::U8
                | Primitive::I16
                | Primitive::U16
                | Primitive::I32
                | Primitive::U32
                | Primitive::I64
                | Primitive::U64
                | Primitive::Int
                | Primitive::Uint
                | Primitive::Byte
                | Primitive::Char
        ),
        _ => false,
    })
}

/// Whether the value is stored in a 64-bit wasm slot.
#[must_use]
pub fn ar_is_64bit(ty: TypeId, interner: &TypeInterner, layout: DataLayout) -> bool {
    matches!(
        slot_valtype(ty, 0, interner, layout),
        Some(ValType::I64 | ValType::F64)
    )
}

/// Convenience: `ValType` of the primary slot of a scalar type.
#[must_use]
pub fn scalar_valtype_for(
    ty: TypeId,
    interner: &TypeInterner,
    layout: DataLayout,
) -> Option<ValType> {
    slot_valtype(ty, 0, interner, layout)
}

/// Narrowing info for sub-byte/16-bit integers that live in a wasm i32 slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrowInfo {
    /// Truncate to `bits`, zero-extend the rest.
    Mask(u32),
    /// Truncate to `bits`, sign-extend from `bits`.
    SignExtend(u32),
}

/// The narrowing mask/extension required after any assignment to keep the
/// stored i32 bit-pattern faithful to the Arandu type.
#[must_use]
pub fn narrow_info(ty: TypeId, interner: &TypeInterner) -> Option<NarrowInfo> {
    interner.with_type(ty, |ar| match ar {
        ArType::Primitive(p) => match p {
            Primitive::U8 | Primitive::Byte => Some(NarrowInfo::Mask(0xFF)),
            Primitive::U16 => Some(NarrowInfo::Mask(0xFFFF)),
            Primitive::I8 => Some(NarrowInfo::SignExtend(8)),
            Primitive::I16 => Some(NarrowInfo::SignExtend(16)),
            _ => None,
        },
        _ => None,
    })
}
