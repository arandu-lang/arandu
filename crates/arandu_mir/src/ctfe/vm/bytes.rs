use super::EvalErrorKind;
use arandu_middle::ctfe::{
    ConstAggregate, ConstFloat, ConstInt, ConstValue, FloatType, IntegerType,
};
use arandu_middle::layout::DataLayout;
use arandu_middle::types::{Primitive, TypeId, TypeShape, VectorType};

pub(super) fn wrap_int(int_ty: IntegerType, val: i128) -> Result<ConstInt, EvalErrorKind> {
    let w = int_ty.bit_width();
    let mask = if w >= 128 {
        u128::MAX
    } else {
        (1u128 << w) - 1
    };
    let unsigned = (val as u128) & mask;
    let wrapped = if int_ty.is_signed() {
        let sign_bit = 1u128 << (w - 1);
        if (unsigned & sign_bit) != 0 {
            (unsigned as i128) - (1_i128 << w)
        } else {
            unsigned as i128
        }
    } else {
        unsigned as i128
    };
    ConstInt::new(int_ty, wrapped).map_err(EvalErrorKind::Value)
}

pub(super) fn vector_to_bytes(
    vector: VectorType,
    aggregate: &ConstAggregate,
) -> Result<[u8; 16], EvalErrorKind> {
    let mut bytes = [0u8; 16];
    let values = aggregate.values();
    if values.len() != usize::from(vector.lanes()) {
        return Err(EvalErrorKind::TypeMismatch);
    }
    match vector {
        VectorType::U8x16 => {
            for (i, v) in values.iter().enumerate() {
                let ConstValue::Integer(int) = v else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                bytes[i] = int.value() as u8;
            }
        }
        VectorType::I16x8 => {
            for (i, v) in values.iter().enumerate() {
                let ConstValue::Integer(int) = v else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let b = (int.value() as i16).to_le_bytes();
                bytes[2 * i..2 * i + 2].copy_from_slice(&b);
            }
        }
        VectorType::I32x4 => {
            for (i, v) in values.iter().enumerate() {
                let ConstValue::Integer(int) = v else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let b = (int.value() as i32).to_le_bytes();
                bytes[4 * i..4 * i + 4].copy_from_slice(&b);
            }
        }
        VectorType::I64x2 => {
            for (i, v) in values.iter().enumerate() {
                let ConstValue::Integer(int) = v else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let b = (int.value() as i64).to_le_bytes();
                bytes[8 * i..8 * i + 8].copy_from_slice(&b);
            }
        }
        VectorType::F32x4 => {
            for (i, v) in values.iter().enumerate() {
                let ConstValue::Float(f) = v else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let b = (f.bits() as u32).to_le_bytes();
                bytes[4 * i..4 * i + 4].copy_from_slice(&b);
            }
        }
        VectorType::F64x2 => {
            for (i, v) in values.iter().enumerate() {
                let ConstValue::Float(f) = v else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let b = f.bits().to_le_bytes();
                bytes[8 * i..8 * i + 8].copy_from_slice(&b);
            }
        }
    }
    Ok(bytes)
}

pub(super) fn bytes_to_vector(
    vector: VectorType,
    bytes: &[u8; 16],
    layout: DataLayout,
) -> Result<ConstAggregate, EvalErrorKind> {
    let lanes = usize::from(vector.lanes());
    let mut values = Vec::new();
    values
        .try_reserve_exact(lanes)
        .map_err(|_| EvalErrorKind::AllocationFailed)?;
    match vector {
        VectorType::U8x16 => {
            let int_ty = IntegerType::new(Primitive::U8, layout).map_err(EvalErrorKind::Value)?;
            for b in bytes {
                values.push(ConstValue::Integer(
                    ConstInt::new(int_ty, i128::from(*b)).map_err(EvalErrorKind::Value)?,
                ));
            }
        }
        VectorType::I16x8 => {
            let int_ty = IntegerType::new(Primitive::I16, layout).map_err(EvalErrorKind::Value)?;
            for i in 0..8 {
                let chunk: [u8; 2] = bytes[2 * i..2 * i + 2]
                    .try_into()
                    .map_err(|_| EvalErrorKind::InvalidIr)?;
                let val = i16::from_le_bytes(chunk);
                values.push(ConstValue::Integer(
                    ConstInt::new(int_ty, i128::from(val)).map_err(EvalErrorKind::Value)?,
                ));
            }
        }
        VectorType::I32x4 => {
            let int_ty = IntegerType::new(Primitive::I32, layout).map_err(EvalErrorKind::Value)?;
            for i in 0..4 {
                let chunk: [u8; 4] = bytes[4 * i..4 * i + 4]
                    .try_into()
                    .map_err(|_| EvalErrorKind::InvalidIr)?;
                let val = i32::from_le_bytes(chunk);
                values.push(ConstValue::Integer(
                    ConstInt::new(int_ty, i128::from(val)).map_err(EvalErrorKind::Value)?,
                ));
            }
        }
        VectorType::I64x2 => {
            let int_ty = IntegerType::new(Primitive::I64, layout).map_err(EvalErrorKind::Value)?;
            for i in 0..2 {
                let chunk: [u8; 8] = bytes[8 * i..8 * i + 8]
                    .try_into()
                    .map_err(|_| EvalErrorKind::InvalidIr)?;
                let val = i64::from_le_bytes(chunk);
                values.push(ConstValue::Integer(
                    ConstInt::new(int_ty, i128::from(val)).map_err(EvalErrorKind::Value)?,
                ));
            }
        }
        VectorType::F32x4 => {
            let flt_ty = FloatType::new(Primitive::F32, layout)
                .map_err(|_| EvalErrorKind::UnsupportedType(TypeId::from_usize(0)))?;
            for i in 0..4 {
                let chunk: [u8; 4] = bytes[4 * i..4 * i + 4]
                    .try_into()
                    .map_err(|_| EvalErrorKind::InvalidIr)?;
                let bits = u32::from_le_bytes(chunk);
                values.push(ConstValue::Float(
                    ConstFloat::new(flt_ty, u64::from(bits))
                        .map_err(|_| EvalErrorKind::InvalidIr)?,
                ));
            }
        }
        VectorType::F64x2 => {
            let flt_ty = FloatType::new(Primitive::F64, layout)
                .map_err(|_| EvalErrorKind::UnsupportedType(TypeId::from_usize(0)))?;
            for i in 0..2 {
                let chunk: [u8; 8] = bytes[8 * i..8 * i + 8]
                    .try_into()
                    .map_err(|_| EvalErrorKind::InvalidIr)?;
                let bits = u64::from_le_bytes(chunk);
                values.push(ConstValue::Float(
                    ConstFloat::new(flt_ty, bits).map_err(|_| EvalErrorKind::InvalidIr)?,
                ));
            }
        }
    }
    ConstAggregate::new(TypeShape::Vector(vector), values).map_err(EvalErrorKind::Value)
}
