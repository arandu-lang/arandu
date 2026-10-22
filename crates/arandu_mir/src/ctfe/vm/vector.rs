use std::sync::Arc;

use super::bytes::{bytes_to_vector, vector_to_bytes, wrap_int};
use super::{AggregateKind, EvalErrorKind, Frame, Meter, ValueType};
use arandu_middle::amir::*;
use arandu_middle::ctfe::{ConstAggregate, ConstValue, FloatArithmetic};
use arandu_middle::types::VectorType;

impl Frame {
    pub(super) fn rvalue_vector_op<C: FnMut() -> bool>(
        &self,
        op: &VectorOp,
        left: &AmirOperand,
        right: &AmirOperand,
        ty: ValueType,
        meter: &mut Meter<'_, C>,
    ) -> Result<ConstValue, EvalErrorKind> {
        let ValueType::Aggregate(descriptor) = &ty else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let AggregateKind::Vector(shape) = descriptor.kind else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        if !op.supports(shape) {
            return Err(EvalErrorKind::InvalidIr);
        }
        let a = self.operand(*left, Some(ty.clone()), meter)?;
        let b = self.operand(*right, Some(ty.clone()), meter)?;
        let (ConstValue::Aggregate(agg_a), ConstValue::Aggregate(agg_b)) = (a, b) else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let lanes = usize::from(shape.lanes());
        if agg_a.values().len() != lanes || agg_b.values().len() != lanes {
            return Err(EvalErrorKind::TypeMismatch);
        }
        let mut results = Vec::new();
        results
            .try_reserve_exact(lanes)
            .map_err(|_| EvalErrorKind::AllocationFailed)?;
        if shape.lane().is_float() {
            let float_op = match op {
                VectorOp::Add => FloatArithmetic::Add,
                VectorOp::Sub => FloatArithmetic::Subtract,
                VectorOp::Mul => FloatArithmetic::Multiply,
                VectorOp::Div => FloatArithmetic::Divide,
                _ => return Err(EvalErrorKind::InvalidIr),
            };
            for (va, vb) in agg_a.values().iter().zip(agg_b.values()) {
                meter.step()?;
                let (ConstValue::Float(fa), ConstValue::Float(fb)) = (va, vb) else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let res = fa
                    .arithmetic(float_op, *fb)
                    .map_err(|_| EvalErrorKind::TypeMismatch)?;
                results.push(ConstValue::Float(res));
            }
        } else {
            for (va, vb) in agg_a.values().iter().zip(agg_b.values()) {
                meter.step()?;
                let (ConstValue::Integer(ia), ConstValue::Integer(ib)) = (va, vb) else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let val_a = ia.value();
                let val_b = ib.value();
                let raw = match op {
                    VectorOp::Add => val_a.wrapping_add(val_b),
                    VectorOp::Sub => val_a.wrapping_sub(val_b),
                    VectorOp::Mul => val_a.wrapping_mul(val_b),
                    VectorOp::BitAnd => val_a & val_b,
                    VectorOp::BitOr => val_a | val_b,
                    VectorOp::BitXor => val_a ^ val_b,
                    _ => return Err(EvalErrorKind::InvalidIr),
                };
                results.push(ConstValue::Integer(wrap_int(ia.ty(), raw)?));
            }
        }
        meter.aggregate(&descriptor.shape, lanes)?;
        for v in &results {
            meter.frozen(v)?;
        }
        Ok(ConstValue::Aggregate(
            ConstAggregate::new(descriptor.shape.clone(), results).map_err(EvalErrorKind::Value)?,
        ))
    }

    pub(super) fn rvalue_vector_compare<C: FnMut() -> bool>(
        &self,
        op: &VectorCompareOp,
        left: &AmirOperand,
        right: &AmirOperand,
        meter: &mut Meter<'_, C>,
    ) -> Result<ConstValue, EvalErrorKind> {
        let operand_ty = self.operand_type(*left)?.or(self.operand_type(*right)?);
        let Some(ValueType::Aggregate(left_desc)) = operand_ty else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let AggregateKind::Vector(shape) = left_desc.kind else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let a = self.operand(
            *left,
            Some(ValueType::Aggregate(Arc::clone(&left_desc))),
            meter,
        )?;
        let b = self.operand(
            *right,
            Some(ValueType::Aggregate(Arc::clone(&left_desc))),
            meter,
        )?;
        let (ConstValue::Aggregate(agg_a), ConstValue::Aggregate(agg_b)) = (a, b) else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let lanes = usize::from(shape.lanes());
        let bytes_per_lane = 16 / lanes;
        let mut out_bytes = [0u8; 16];
        if shape.lane().is_float() {
            for (l, (va, vb)) in agg_a.values().iter().zip(agg_b.values()).enumerate() {
                meter.step()?;
                let (ConstValue::Float(fa), ConstValue::Float(fb)) = (va, vb) else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let ord = fa.compare(*fb).map_err(|_| EvalErrorKind::TypeMismatch)?;
                let cond = match op {
                    VectorCompareOp::Eq => ord == Some(std::cmp::Ordering::Equal),
                    VectorCompareOp::Ne => ord != Some(std::cmp::Ordering::Equal),
                    VectorCompareOp::Lt => ord == Some(std::cmp::Ordering::Less),
                    VectorCompareOp::Le => {
                        matches!(
                            ord,
                            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                        )
                    }
                    VectorCompareOp::Gt => ord == Some(std::cmp::Ordering::Greater),
                    VectorCompareOp::Ge => {
                        matches!(
                            ord,
                            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                        )
                    }
                };
                let byte_val = if cond { 0xff } else { 0x00 };
                for b in 0..bytes_per_lane {
                    out_bytes[l * bytes_per_lane + b] = byte_val;
                }
            }
        } else {
            let unsigned = shape == VectorType::U8x16;
            for (l, (va, vb)) in agg_a.values().iter().zip(agg_b.values()).enumerate() {
                meter.step()?;
                let (ConstValue::Integer(ia), ConstValue::Integer(ib)) = (va, vb) else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let cond = if unsigned {
                    let (ua, ub) = (ia.value() as u8, ib.value() as u8);
                    match op {
                        VectorCompareOp::Eq => ua == ub,
                        VectorCompareOp::Ne => ua != ub,
                        VectorCompareOp::Lt => ua < ub,
                        VectorCompareOp::Le => ua <= ub,
                        VectorCompareOp::Gt => ua > ub,
                        VectorCompareOp::Ge => ua >= ub,
                    }
                } else {
                    let (sa, sb) = (ia.value(), ib.value());
                    match op {
                        VectorCompareOp::Eq => sa == sb,
                        VectorCompareOp::Ne => sa != sb,
                        VectorCompareOp::Lt => sa < sb,
                        VectorCompareOp::Le => sa <= sb,
                        VectorCompareOp::Gt => sa > sb,
                        VectorCompareOp::Ge => sa >= sb,
                    }
                };
                let byte_val = if cond { 0xff } else { 0x00 };
                for b in 0..bytes_per_lane {
                    out_bytes[l * bytes_per_lane + b] = byte_val;
                }
            }
        }
        let agg = bytes_to_vector(VectorType::U8x16, &out_bytes, self.unit.layout)?;
        meter.aggregate(agg.shape(), 16)?;
        for v in agg.values() {
            meter.frozen(v)?;
        }
        Ok(ConstValue::Aggregate(agg))
    }

    pub(super) fn rvalue_vector_select<C: FnMut() -> bool>(
        &self,
        mask: &AmirOperand,
        if_true: &AmirOperand,
        if_false: &AmirOperand,
        ty: ValueType,
        meter: &mut Meter<'_, C>,
    ) -> Result<ConstValue, EvalErrorKind> {
        let ValueType::Aggregate(descriptor) = &ty else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let AggregateKind::Vector(shape) = descriptor.kind else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let mask_val = self.operand(*mask, None, meter)?;
        let true_val = self.operand(*if_true, Some(ty.clone()), meter)?;
        let false_val = self.operand(*if_false, Some(ty.clone()), meter)?;
        let (
            ConstValue::Aggregate(mask_agg),
            ConstValue::Aggregate(true_agg),
            ConstValue::Aggregate(false_agg),
        ) = (mask_val, true_val, false_val)
        else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let mask_bytes = vector_to_bytes(VectorType::U8x16, &mask_agg)?;
        let true_bytes = vector_to_bytes(shape, &true_agg)?;
        let false_bytes = vector_to_bytes(shape, &false_agg)?;
        let mut out_bytes = [0u8; 16];
        for (out, ((t, f), m)) in out_bytes
            .iter_mut()
            .zip(true_bytes.iter().zip(&false_bytes).zip(&mask_bytes))
        {
            meter.step()?;
            *out = (t & m) | (f & !m);
        }
        let agg = bytes_to_vector(shape, &out_bytes, self.unit.layout)?;
        meter.aggregate(&descriptor.shape, usize::from(shape.lanes()))?;
        for v in agg.values() {
            meter.frozen(v)?;
        }
        Ok(ConstValue::Aggregate(agg))
    }

    pub(super) fn rvalue_vector_shuffle<C: FnMut() -> bool>(
        &self,
        left: &AmirOperand,
        right: &AmirOperand,
        mask: &[u8; 16],
        ty: ValueType,
        meter: &mut Meter<'_, C>,
    ) -> Result<ConstValue, EvalErrorKind> {
        let ValueType::Aggregate(descriptor) = &ty else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let AggregateKind::Vector(shape) = descriptor.kind else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let a = self.operand(*left, Some(ty.clone()), meter)?;
        let b = self.operand(*right, Some(ty.clone()), meter)?;
        let (ConstValue::Aggregate(agg_a), ConstValue::Aggregate(agg_b)) = (a, b) else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        let left_bytes = vector_to_bytes(shape, &agg_a)?;
        let right_bytes = vector_to_bytes(shape, &agg_b)?;
        let mut concat = [0u8; 32];
        concat[..16].copy_from_slice(&left_bytes);
        concat[16..].copy_from_slice(&right_bytes);
        let mut out_bytes = [0u8; 16];
        for (i, &idx) in mask.iter().enumerate() {
            meter.step()?;
            let index = usize::from(idx);
            if index >= 32 {
                return Err(EvalErrorKind::InvalidIr);
            }
            out_bytes[i] = concat[index];
        }
        let agg = bytes_to_vector(shape, &out_bytes, self.unit.layout)?;
        meter.aggregate(&descriptor.shape, usize::from(shape.lanes()))?;
        for v in agg.values() {
            meter.frozen(v)?;
        }
        Ok(ConstValue::Aggregate(agg))
    }
}
