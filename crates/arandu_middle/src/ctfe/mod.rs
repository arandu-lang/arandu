//! Pool-independent values shared by compile-time evaluation and its consumers.
//!
//! These types describe values, not an interpreter or a Salsa provider. Integer
//! constructors validate the target width before a value can enter the domain.

use crate::layout::DataLayout;
use crate::types::Primitive;
use crate::types::TypeShape;
use std::sync::Arc;

mod aggregate;
pub use aggregate::{ConstAggregate, ConstVariant, canonical_type_bytes, validate_ctfe_type_shape};
mod float;
pub use float::{ConstFloat, FloatArithmetic, FloatError, FloatType};
mod string;
pub use string::{ConstBytes, ConstString, StringError};

/// An admitted integer type with a resolved, target-dependent bit width.
///
/// Fields are private so equality, bounds and encoding cannot observe an invalid
/// width or an inconsistent primitive tag. Default integers remain 32-bit;
/// only `isize` and `usize` depend on the target pointer width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntegerType {
    primitive: Primitive,
    bit_width: u8,
    encoding_tag: u8,
}

impl IntegerType {
    /// Resolve an integer primitive without consulting the host or a type pool.
    pub fn new(primitive: Primitive, layout: DataLayout) -> Result<Self, ConstValueError> {
        let (bit_width, encoding_tag) = match primitive {
            Primitive::Int => (32, 0),
            Primitive::Uint => (32, 1),
            Primitive::ISize => (pointer_bits(layout)?, 2),
            Primitive::USize => (pointer_bits(layout)?, 3),
            Primitive::I8 => (8, 4),
            Primitive::I16 => (16, 5),
            Primitive::I32 => (32, 6),
            Primitive::I64 => (64, 7),
            Primitive::U8 => (8, 8),
            Primitive::U16 => (16, 9),
            Primitive::U32 => (32, 10),
            Primitive::U64 => (64, 11),
            Primitive::Byte => (8, 12),
            Primitive::Float
            | Primitive::F32
            | Primitive::F64
            | Primitive::Bool
            | Primitive::Char
            | Primitive::Str
            | Primitive::Any => return Err(ConstValueError::UnsupportedIntegerType(primitive)),
        };
        Ok(Self {
            primitive,
            bit_width,
            encoding_tag,
        })
    }

    #[must_use]
    pub const fn primitive(self) -> Primitive {
        self.primitive
    }

    #[must_use]
    pub const fn bit_width(self) -> u8 {
        self.bit_width
    }

    /// Canonical encoding of this type (same header as integer zero).
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 20] {
        let mut bytes = [0; 20];
        bytes[..4].copy_from_slice(&[1, 2, self.encoding_tag, self.bit_width]);
        bytes
    }

    #[must_use]
    pub fn is_signed(self) -> bool {
        self.primitive.is_signed()
    }

    /// Smallest representable mathematical value (not a host bit pattern).
    #[must_use]
    pub fn min(self) -> i128 {
        if self.is_signed() {
            -(1_i128 << (self.bit_width - 1))
        } else {
            0
        }
    }

    /// Largest representable value, including the entire `u64` range.
    #[must_use]
    pub fn max(self) -> i128 {
        let magnitude_bits = self.bit_width - u8::from(self.is_signed());
        (1_i128 << magnitude_bits) - 1
    }
}

fn pointer_bits(layout: DataLayout) -> Result<u8, ConstValueError> {
    match layout.pointer_width() {
        4 => Ok(32),
        8 => Ok(64),
        width => Err(ConstValueError::UnsupportedPointerWidth(width)),
    }
}

/// A typed mathematical integer whose range was checked at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstInt {
    ty: IntegerType,
    value: i128,
}

impl ConstInt {
    pub fn new(ty: IntegerType, value: i128) -> Result<Self, ConstValueError> {
        if value < ty.min() || value > ty.max() {
            return Err(ConstValueError::OutOfRange { ty, value });
        }
        Ok(Self { ty, value })
    }

    #[must_use]
    pub const fn ty(self) -> IntegerType {
        self.ty
    }

    #[must_use]
    pub const fn value(self) -> i128 {
        self.value
    }

    /// An explicit, checked conversion; it never truncates or wraps.
    pub fn cast(self, ty: IntegerType) -> Result<Self, ConstValueError> {
        Self::new(ty, self.value)
    }

    /// Lossless bridge to the existing `ArType::Const(u64)` domain.
    ///
    /// The caller still validates the declared generic parameter type. This
    /// conversion only checks that the scalar is representable by the existing
    /// generic argument storage; it does not create a new specialization key.
    pub fn to_const_generic(self) -> Result<u64, ConstValueError> {
        u64::try_from(self.value).map_err(|_| ConstValueError::NegativeConstGeneric(self.value))
    }
}

/// Initial CTFE value domain. Aggregate values are a separate implementation
/// step; no pointer, pool-local identity or reference can escape this domain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstValue {
    Void,
    Bool(bool),
    Integer(ConstInt),
    Aggregate(ConstAggregate),
    Float(ConstFloat),
    String(ConstString),
    Bytes(ConstBytes),
}

impl ConstValue {
    /// Structural type of a frozen value, independent of any interner.
    #[must_use]
    pub fn type_shape(&self) -> crate::types::TypeShape {
        use crate::types::{Primitive, TypeShape};
        match self {
            Self::Void => TypeShape::Void,
            Self::Bool(_) => TypeShape::Primitive(Primitive::Bool),
            Self::Integer(value) => TypeShape::Primitive(value.ty().primitive()),
            Self::Float(value) => TypeShape::Primitive(value.ty().primitive()),
            Self::String(_) => TypeShape::Primitive(Primitive::Str),
            Self::Bytes(_) => TypeShape::Slice(Box::new(TypeShape::Primitive(Primitive::U8))),
            Self::Aggregate(value) => value.shape().clone(),
        }
    }

    /// Canonical scalar encoding v1, suitable as input to a stable digest.
    ///
    /// Bytes are version, value tag, primitive tag, resolved bit width, then
    /// a 16-byte little-endian mathematical value. Unused bytes are zero. This
    /// does not serialize the Rust memory layout or depend on `TypeId`/`LiteralId`.
    /// Keep existing tags fixed if the value domain grows.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = [0; 20];
        bytes[0] = 1;
        match self {
            Self::Void => {}
            Self::Bool(value) => {
                bytes[1] = 1;
                bytes[4] = u8::from(*value);
            }
            Self::Integer(integer) => {
                bytes[1] = 2;
                bytes[2] = integer.ty.encoding_tag;
                bytes[3] = integer.ty.bit_width;
                bytes[4..].copy_from_slice(&integer.value.to_le_bytes());
            }
            Self::Aggregate(value) => return value.canonical_bytes(),
            Self::Float(value) => return value.canonical_bytes().to_vec(),
            Self::String(value) => return value.canonical_bytes(),
            Self::Bytes(value) => return value.canonical_bytes(),
        }
        bytes.to_vec()
    }
}

/// Internal value-domain failures. User-facing diagnostics belong to the
/// evaluation boundary, which supplies spans and the CTFE call context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstValueError {
    UnsupportedIntegerType(Primitive),
    UnsupportedPointerWidth(u64),
    OutOfRange { ty: IntegerType, value: i128 },
    NegativeConstGeneric(i128),
    StructuralLimit,
    InvalidAggregateShape,
}

#[cfg(test)]
mod tests;
