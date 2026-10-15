//! Pool-independent IEEE values and software rounding shared by CTFE/literals.
//!
//! Host floating-point instructions never decide a constant's bits. Arithmetic
//! rounds once to the declared format (nearest, ties to even), and arithmetic
//! NaNs have one positive quiet encoding. Cache equality remains bitwise.

use std::cmp::Ordering;

use rustc_apfloat::{
    Float, FloatConvert, Round, Status,
    ieee::{Double, Single},
};

use super::{ConstInt, IntegerType};
use crate::{
    layout::{DataLayout, DataLayoutError},
    types::Primitive,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FloatType {
    primitive: Primitive,
    bit_width: u8,
}

impl FloatType {
    pub fn new(primitive: Primitive, layout: DataLayout) -> Result<Self, FloatError> {
        layout.validate().map_err(FloatError::InvalidLayout)?;
        let bit_width = match primitive {
            Primitive::F32 => 32,
            Primitive::Float => u8::try_from(
                layout
                    .float
                    .size
                    .checked_mul(8)
                    .ok_or(FloatError::InvalidLayout(DataLayoutError::ScalarWidth))?,
            )
            .map_err(|_| FloatError::InvalidLayout(DataLayoutError::ScalarWidth))?,
            Primitive::F64 => u8::try_from(
                layout
                    .f64
                    .size
                    .checked_mul(8)
                    .ok_or(FloatError::InvalidLayout(DataLayoutError::ScalarWidth))?,
            )
            .map_err(|_| FloatError::InvalidLayout(DataLayoutError::ScalarWidth))?,
            _ => return Err(FloatError::TypeMismatch),
        };
        Ok(Self {
            primitive,
            bit_width,
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

    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 12] {
        let tag = match self.primitive {
            Primitive::Float => 0,
            Primitive::F32 => 1,
            Primitive::F64 => 2,
            _ => 255, // Private constructor makes this case unreachable.
        };
        let mut bytes = [0; 12];
        bytes[..4].copy_from_slice(&[1, 3, tag, self.bit_width]);
        bytes
    }
}

/// Every IEEE encoding is a valid value, including signed zero/NaN payloads.
/// Encoding equality is deliberately distinct from language float comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstFloat {
    ty: FloatType,
    bits: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatError {
    InvalidLayout(DataLayoutError),
    TypeMismatch,
    InvalidBits,
    InvalidLiteral,
    LiteralLimit,
    IntegerOutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatArithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl ConstFloat {
    pub fn new(ty: FloatType, bits: u64) -> Result<Self, FloatError> {
        if ty.bit_width == 32 && bits > u64::from(u32::MAX) {
            return Err(FloatError::InvalidBits);
        }
        Ok(Self { ty, bits })
    }

    #[must_use]
    pub const fn ty(self) -> FloatType {
        self.ty
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.bits
    }

    #[must_use]
    pub fn bits32(self) -> Option<u32> {
        (self.ty.bit_width == 32)
            .then(|| u32::try_from(self.bits).ok())
            .flatten()
    }

    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 12] {
        let mut bytes = self.ty.canonical_bytes();
        bytes[4..].copy_from_slice(&self.bits.to_le_bytes());
        bytes
    }

    /// Decimal conversion occurs directly in the destination IEEE format;
    /// parsing through f64 first would double-round some f32 literals.
    pub fn parse(ty: FloatType, text: &str) -> Result<Self, FloatError> {
        // APFloat's decimal parser uses bounded arbitrary-precision work but
        // does not accept a cancellation callback. Bound a single call before
        // allocating cleaned digits; admission additionally charges the input.
        if text.len() > crate::types::TypeShape::MAX_NODES {
            return Err(FloatError::LiteralLimit);
        }
        let cleaned: String = text.trim().chars().filter(|&c| c != '_').collect();
        let bits = if ty.bit_width == 32 {
            Single::from_str_r(&cleaned, Round::NearestTiesToEven)
                .map_err(|_| FloatError::InvalidLiteral)?
                .value
                .to_bits()
        } else {
            Double::from_str_r(&cleaned, Round::NearestTiesToEven)
                .map_err(|_| FloatError::InvalidLiteral)?
                .value
                .to_bits()
        };
        Self::new(
            ty,
            u64::try_from(bits).map_err(|_| FloatError::InvalidBits)?,
        )
    }

    #[must_use]
    pub fn negated(self) -> Self {
        Self {
            bits: self.bits ^ (1_u64 << (self.ty.bit_width - 1)),
            ..self
        }
    }

    pub fn arithmetic(self, op: FloatArithmetic, rhs: Self) -> Result<Self, FloatError> {
        if self.ty != rhs.ty {
            return Err(FloatError::TypeMismatch);
        }
        let bits = if self.ty.bit_width == 32 {
            arithmetic(
                op,
                Single::from_bits(u128::from(self.bits)),
                Single::from_bits(u128::from(rhs.bits)),
            )
        } else {
            arithmetic(
                op,
                Double::from_bits(u128::from(self.bits)),
                Double::from_bits(u128::from(rhs.bits)),
            )
        };
        Self::new(
            self.ty,
            u64::try_from(bits).map_err(|_| FloatError::InvalidBits)?,
        )
    }

    pub fn compare(self, rhs: Self) -> Result<Option<Ordering>, FloatError> {
        if self.ty != rhs.ty {
            return Err(FloatError::TypeMismatch);
        }
        Ok(if self.ty.bit_width == 32 {
            Single::from_bits(u128::from(self.bits))
                .partial_cmp(&Single::from_bits(u128::from(rhs.bits)))
        } else {
            Double::from_bits(u128::from(self.bits))
                .partial_cmp(&Double::from_bits(u128::from(rhs.bits)))
        })
    }

    pub fn cast(self, destination: FloatType) -> Result<Self, FloatError> {
        let mut loses_info = false;
        let bits = match (self.ty.bit_width, destination.bit_width) {
            (32, 64) => {
                let converted: rustc_apfloat::StatusAnd<Double> =
                    Single::from_bits(u128::from(self.bits))
                        .convert_r(Round::NearestTiesToEven, &mut loses_info);
                canonical(converted.value)
            }
            (64, 32) => {
                let converted: rustc_apfloat::StatusAnd<Single> =
                    Double::from_bits(u128::from(self.bits))
                        .convert_r(Round::NearestTiesToEven, &mut loses_info);
                canonical(converted.value)
            }
            _ => u128::from(self.bits),
        };
        Self::new(
            destination,
            u64::try_from(bits).map_err(|_| FloatError::InvalidBits)?,
        )
    }

    pub fn from_integer(value: ConstInt, destination: FloatType) -> Result<Self, FloatError> {
        let bits = if destination.bit_width == 32 {
            Single::from_i128_r(value.value(), Round::NearestTiesToEven)
                .value
                .to_bits()
        } else {
            Double::from_i128_r(value.value(), Round::NearestTiesToEven)
                .value
                .to_bits()
        };
        Self::new(
            destination,
            u64::try_from(bits).map_err(|_| FloatError::InvalidBits)?,
        )
    }

    /// Explicit float-to-int casts truncate toward zero and reject invalid or
    /// out-of-range values rather than silently saturating/wrapping.
    pub fn to_integer(self, destination: IntegerType) -> Result<ConstInt, FloatError> {
        let result = if self.ty.bit_width == 32 {
            integer(Single::from_bits(u128::from(self.bits)), destination)?
        } else {
            integer(Double::from_bits(u128::from(self.bits)), destination)?
        };
        ConstInt::new(destination, result).map_err(|_| FloatError::IntegerOutOfRange)
    }

    /// Presentation only; never parse this display back into a constant.
    #[must_use]
    pub fn display(self) -> String {
        // Rust's shortest-decimal formatter is presentation only. from_bits
        // performs no floating arithmetic/conversion, and this display is
        // never used to reconstitute a constant or decide cache identity.
        let mut text = if let Some(bits) = self.bits32() {
            f32::from_bits(bits).to_string()
        } else {
            f64::from_bits(self.bits).to_string()
        };
        if text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
        {
            text.push_str(".0");
        }
        text
    }
}

fn arithmetic<F: Float>(op: FloatArithmetic, a: F, b: F) -> u128 {
    let result = match op {
        FloatArithmetic::Add => a.add_r(b, Round::NearestTiesToEven),
        FloatArithmetic::Subtract => a.sub_r(b, Round::NearestTiesToEven),
        FloatArithmetic::Multiply => a.mul_r(b, Round::NearestTiesToEven),
        FloatArithmetic::Divide => a.div_r(b, Round::NearestTiesToEven),
    };
    canonical(result.value)
}

fn canonical<F: Float>(value: F) -> u128 {
    // This does not change signed zeros, infinities or finite/subnormal values.
    if value.is_nan() {
        F::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

fn integer<F: Float>(value: F, destination: IntegerType) -> Result<i128, FloatError> {
    let mut exact = false;
    if !destination.is_signed() {
        let unsigned = value.to_u128_r(
            usize::from(destination.bit_width()),
            Round::TowardZero,
            &mut exact,
        );
        if unsigned.status.contains(Status::INVALID_OP) {
            return Err(FloatError::IntegerOutOfRange);
        }
        return i128::try_from(unsigned.value).map_err(|_| FloatError::IntegerOutOfRange);
    }
    let result = value.to_i128_r(
        usize::from(destination.bit_width()),
        Round::TowardZero,
        &mut exact,
    );
    if result.status.contains(Status::INVALID_OP) {
        Err(FloatError::IntegerOutOfRange)
    } else {
        Ok(result.value)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn ty(primitive: Primitive) -> FloatType {
        FloatType::new(primitive, DataLayout::ptr_width(4)).unwrap()
    }

    #[test]
    fn parses_directly_without_f32_double_rounding() {
        let text = "1.000000059604644775390625000000000000000000000000000001";
        let single = ConstFloat::parse(ty(Primitive::F32), text).unwrap();
        assert_eq!(single.bits(), 0x3f800001);
        assert_eq!(
            ConstFloat::parse(ty(Primitive::F64), text)
                .unwrap()
                .cast(ty(Primitive::F32))
                .unwrap()
                .bits(),
            0x3f800000
        );
    }

    #[test]
    fn software_arithmetic_handles_ieee_edges_and_canonical_nan() {
        let t = ty(Primitive::F64);
        let zero = ConstFloat::new(t, 0).unwrap();
        let one = ConstFloat::new(t, 0x3ff0000000000000).unwrap();
        assert_eq!(
            zero.arithmetic(FloatArithmetic::Divide, zero)
                .unwrap()
                .bits(),
            0x7ff8000000000000
        );
        assert_eq!(
            one.arithmetic(FloatArithmetic::Divide, zero)
                .unwrap()
                .bits(),
            0x7ff0000000000000
        );
        assert_eq!(zero.negated().bits(), 0x8000000000000000);
        assert_ne!(zero.canonical_bytes(), zero.negated().canonical_bytes());
        assert_eq!(zero.compare(zero.negated()).unwrap(), Some(Ordering::Equal));
        let subnormal = ConstFloat::new(t, 1).unwrap();
        assert_eq!(
            subnormal
                .arithmetic(FloatArithmetic::Add, subnormal)
                .unwrap()
                .bits(),
            2
        );
        let nan = ConstFloat::new(t, 0xfff8000000001234).unwrap();
        assert_eq!(nan.compare(nan).unwrap(), None);
        assert_eq!(
            nan.arithmetic(FloatArithmetic::Add, one).unwrap().bits(),
            0x7ff8000000000000
        );
    }

    #[test]
    fn rounding_and_format_identity_are_target_independent() {
        for layout in [
            DataLayout::ptr_width(4),
            DataLayout::ptr_width(8),
            DataLayout::i686_sysv(),
        ] {
            let t = FloatType::new(Primitive::Float, layout).unwrap();
            assert_eq!(t.bit_width(), 64);
            let value =
                ConstFloat::parse(t, "1.00000000000000011102230246251565404236316680908203125")
                    .unwrap();
            assert_eq!(value.bits(), 0x3ff0000000000000);
        }
        assert_ne!(
            ty(Primitive::Float).canonical_bytes(),
            ty(Primitive::F64).canonical_bytes()
        );
    }

    #[test]
    fn rejects_invalid_bits_and_checked_integer_casts() {
        assert_eq!(
            ConstFloat::new(ty(Primitive::F32), 1_u64 << 32),
            Err(FloatError::InvalidBits)
        );
        let integer = IntegerType::new(Primitive::I8, DataLayout::ptr_width(8)).unwrap();
        assert_eq!(
            ConstFloat::parse(ty(Primitive::F64), "128")
                .unwrap()
                .to_integer(integer),
            Err(FloatError::IntegerOutOfRange)
        );
        assert_eq!(
            ConstFloat::parse(ty(Primitive::F64), "-3.75")
                .unwrap()
                .to_integer(integer)
                .unwrap()
                .value(),
            -3
        );
    }

    #[test]
    fn operations_round_once_at_overflow_underflow_and_ties() {
        let single = ty(Primitive::F32);
        let one = ConstFloat::new(single, 0x3f800000).unwrap();
        let half_ulp = ConstFloat::new(single, 0x33800000).unwrap();
        assert_eq!(
            one.arithmetic(FloatArithmetic::Add, half_ulp)
                .unwrap()
                .bits(),
            0x3f800000
        );
        let odd = ConstFloat::new(single, 0x3f800001).unwrap();
        assert_eq!(
            odd.arithmetic(FloatArithmetic::Add, half_ulp)
                .unwrap()
                .bits(),
            0x3f800002
        );
        let two = ConstFloat::new(single, 0x40000000).unwrap();
        assert_eq!(
            ConstFloat::new(single, 1)
                .unwrap()
                .arithmetic(FloatArithmetic::Divide, two)
                .unwrap()
                .bits(),
            0
        );
        assert_eq!(
            ConstFloat::new(single, 3)
                .unwrap()
                .arithmetic(FloatArithmetic::Divide, two)
                .unwrap()
                .bits(),
            2
        );
        assert_eq!(
            ConstFloat::new(single, 0x7f7fffff)
                .unwrap()
                .arithmetic(FloatArithmetic::Multiply, two)
                .unwrap()
                .bits(),
            0x7f800000
        );
        assert_eq!(
            ConstFloat::new(single, 0x80000000)
                .unwrap()
                .cast(ty(Primitive::F64))
                .unwrap()
                .bits(),
            0x8000000000000000
        );
        assert_eq!(
            ConstFloat::new(single, 0xffc01234)
                .unwrap()
                .cast(ty(Primitive::F64))
                .unwrap()
                .bits(),
            0x7ff8000000000000
        );
    }

    #[test]
    fn numeric_casts_keep_full_integer_domain_and_reject_nonfinite() {
        let integer = IntegerType::new(Primitive::U64, DataLayout::ptr_width(8)).unwrap();
        let rounded = ConstFloat::from_integer(
            ConstInt::new(integer, i128::from(u64::MAX)).unwrap(),
            ty(Primitive::F64),
        )
        .unwrap();
        assert_eq!(rounded.bits(), 0x43f0000000000000);
        assert_eq!(
            rounded.to_integer(integer),
            Err(FloatError::IntegerOutOfRange)
        );
        for bits in [0x7ff8000000000000, 0x7ff0000000000000, 0xfff0000000000000] {
            assert_eq!(
                ConstFloat::new(ty(Primitive::F64), bits)
                    .unwrap()
                    .to_integer(integer),
                Err(FloatError::IntegerOutOfRange)
            );
        }
        assert_eq!(
            ConstFloat::parse(ty(Primitive::F64), "-1")
                .unwrap()
                .to_integer(integer),
            Err(FloatError::IntegerOutOfRange)
        );
    }
}
