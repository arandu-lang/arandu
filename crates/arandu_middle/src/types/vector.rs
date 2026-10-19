//! Closed set of portable 128-bit hardware vector shapes (RFC 0038).
//! Target-sized `float`, `int` and arbitrary lane counts are deliberately absent.

use super::Primitive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorType {
    F32x4,
    F64x2,
    I32x4,
    I16x8,
    U8x16,
    I64x2,
}

impl VectorType {
    pub const ALL: [Self; 6] = [
        Self::F32x4,
        Self::F64x2,
        Self::I32x4,
        Self::I16x8,
        Self::U8x16,
        Self::I64x2,
    ];

    #[must_use]
    pub const fn lane(self) -> Primitive {
        match self {
            Self::F32x4 => Primitive::F32,
            Self::F64x2 => Primitive::F64,
            Self::I32x4 => Primitive::I32,
            Self::I16x8 => Primitive::I16,
            Self::U8x16 => Primitive::U8,
            Self::I64x2 => Primitive::I64,
        }
    }

    #[must_use]
    pub const fn lanes(self) -> u8 {
        match self {
            Self::F32x4 | Self::I32x4 => 4,
            Self::F64x2 | Self::I64x2 => 2,
            Self::I16x8 => 8,
            Self::U8x16 => 16,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::F32x4 => "F32x4",
            Self::F64x2 => "F64x2",
            Self::I32x4 => "I32x4",
            Self::I16x8 => "I16x8",
            Self::U8x16 => "U8x16",
            Self::I64x2 => "I64x2",
        }
    }
}
