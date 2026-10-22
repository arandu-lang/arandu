use super::{
    AggregateKind, EvalErrorKind, Frame, Meter, ScalarEvalError, ValueType, eval_binary, eval_unary,
};
use arandu_middle::amir::*;
use arandu_middle::ctfe::{ConstAggregate, ConstFloat, ConstInt, ConstValue};

impl Frame {
    pub(super) fn rvalue<C: FnMut() -> bool>(
        &self,
        rhs: &AmirRvalue,
        ty: ValueType,
        meter: &mut Meter<'_, C>,
    ) -> Result<ConstValue, EvalErrorKind> {
        match rhs {
            AmirRvalue::Use(operand) => {
                if matches!(operand, AmirOperand::Copy(_) | AmirOperand::Move(_)) {
                    let value = self.operand(*operand, None, meter)?;
                    match (&ty, &value) {
                        (ValueType::Float(destination), ConstValue::Float(value)) => {
                            return value
                                .cast(*destination)
                                .map(ConstValue::Float)
                                .map_err(|_| EvalErrorKind::TypeMismatch);
                        }
                        (ValueType::Float(destination), ConstValue::Integer(value)) => {
                            return ConstFloat::from_integer(*value, *destination)
                                .map(ConstValue::Float)
                                .map_err(|_| EvalErrorKind::TypeMismatch);
                        }
                        (ValueType::Integer(destination), ConstValue::Float(value)) => {
                            return value
                                .to_integer(*destination)
                                .map(ConstValue::Integer)
                                .map_err(|_| {
                                    EvalErrorKind::Arithmetic(ScalarEvalError::Overflow(
                                        *destination,
                                    ))
                                });
                        }
                        _ => {}
                    }
                }
                // AMIR represents scalar coercions/casts as typed Use stores.
                // Convert a typed integer explicitly; pool literals instead
                // obtain their type from the destination at decoding time.
                if let (
                    ValueType::Integer(destination),
                    AmirOperand::Copy(_) | AmirOperand::Move(_),
                ) = (&ty, operand)
                {
                    let ConstValue::Integer(value) = self.operand(*operand, None, meter)? else {
                        return Err(EvalErrorKind::TypeMismatch);
                    };
                    value
                        .cast(*destination)
                        .map(ConstValue::Integer)
                        .map_err(EvalErrorKind::Value)
                } else {
                    self.operand(*operand, Some(ty), meter)
                }
            }
            AmirRvalue::Unary { op, operand } => {
                eval_unary(*op, self.operand(*operand, Some(ty), meter)?)
                    .map_err(EvalErrorKind::Arithmetic)
            }
            AmirRvalue::Binary { op, left, right } => {
                let operand_ty =
                    self.operand_type(*left)?
                        .or(self.operand_type(*right)?)
                        .or(match &ty {
                            ValueType::Integer(_) | ValueType::Float(_) => Some(ty.clone()),
                            _ => None,
                        });
                let a = self.operand(*left, operand_ty.clone(), meter)?;
                let right_hint = if matches!(
                    op,
                    arandu_middle::ops::BinaryOp::ShiftLeft
                        | arandu_middle::ops::BinaryOp::ShiftRight
                ) {
                    self.operand_type(*right)?.or(operand_ty.clone())
                } else {
                    operand_ty
                };
                eval_binary(*op, a, self.operand(*right, right_hint, meter)?)
                    .map_err(EvalErrorKind::Arithmetic)
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                let ValueType::Enum(descriptor) = &ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let (symbol, payload_ty) = descriptor
                    .variants
                    .get(*variant_tag)
                    .ok_or(EvalErrorKind::InvalidIr)?;
                let value = match (payload, payload_ty) {
                    (None, None) => None,
                    (Some(operand), Some(ty)) => {
                        Some(self.operand(*operand, Some(ty.clone()), meter)?)
                    }
                    _ => return Err(EvalErrorKind::TypeMismatch),
                };
                meter.aggregate(&descriptor.shape, usize::from(value.is_some()))?;
                if let Some(value) = &value {
                    meter.frozen(value)?;
                }
                Ok(ConstValue::Aggregate(
                    ConstAggregate::enumeration(
                        descriptor.shape.clone(),
                        arandu_middle::ctfe::ConstVariant {
                            tag: *variant_tag,
                            symbol: *symbol,
                        },
                        value,
                    )
                    .map_err(EvalErrorKind::Value)?,
                ))
            }
            AmirRvalue::Discriminant { value } => {
                let ConstValue::Aggregate(value) = self.operand(*value, None, meter)? else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let variant = value.variant().ok_or(EvalErrorKind::TypeMismatch)?;
                let ValueType::Integer(integer) = ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                ConstInt::new(
                    integer,
                    i128::try_from(variant.tag).map_err(|_| EvalErrorKind::ValueLimit)?,
                )
                .map(ConstValue::Integer)
                .map_err(EvalErrorKind::Value)
            }
            AmirRvalue::EnumPayload {
                value,
                variant,
                variant_tag,
                index,
                field_ty,
                tuple_ty,
            } => {
                let ConstValue::Aggregate(value) = self.operand(*value, None, meter)? else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let identity = value.variant().ok_or(EvalErrorKind::TypeMismatch)?;
                if identity.tag != *variant_tag
                    || identity.symbol.is_some_and(|symbol| symbol != *variant)
                {
                    return Err(EvalErrorKind::InvalidIr);
                }
                let [payload] = value.values() else {
                    return Err(EvalErrorKind::InvalidIr);
                };
                let projected = if let Some(tuple_ty) = tuple_ty {
                    let ConstValue::Aggregate(tuple) = payload else {
                        return Err(EvalErrorKind::TypeMismatch);
                    };
                    let descriptor = self
                        .unit
                        .projection_types
                        .iter()
                        .find(|(id, _)| id == tuple_ty)
                        .map(|(_, ty)| ty)
                        .ok_or(EvalErrorKind::InvalidIr)?;
                    if tuple.variant().is_some() || !descriptor.accepts(payload) {
                        return Err(EvalErrorKind::TypeMismatch);
                    }
                    tuple.values().get(*index).ok_or(EvalErrorKind::InvalidIr)?
                } else {
                    if *index != 0 {
                        return Err(EvalErrorKind::InvalidIr);
                    }
                    payload
                };
                let field = self
                    .unit
                    .projection_types
                    .iter()
                    .find(|(id, _)| id == field_ty)
                    .map(|(_, ty)| ty)
                    .ok_or(EvalErrorKind::InvalidIr)?;
                if field != &ty || !ty.accepts(projected) {
                    return Err(EvalErrorKind::TypeMismatch);
                }
                meter.step()?;
                Ok(projected.clone())
            }
            AmirRvalue::Load(place) => self.load_place(place, meter).map(|(_, value)| value),
            AmirRvalue::Array { items } | AmirRvalue::Tuple { items } => {
                let ValueType::Aggregate(descriptor) = &ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let kind = if matches!(rhs, AmirRvalue::Array { .. }) {
                    AggregateKind::Array
                } else {
                    AggregateKind::Tuple
                };
                if descriptor.kind != kind || descriptor.fields.len() != items.len() {
                    return Err(EvalErrorKind::TypeMismatch);
                }
                meter.reserve(items.len())?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(items.len())
                    .map_err(|_| EvalErrorKind::AllocationFailed)?;
                for (operand, field) in items.iter().zip(&descriptor.fields) {
                    meter.step()?;
                    values.push(self.operand(*operand, Some(field.clone()), meter)?);
                }
                meter.aggregate(&descriptor.shape, items.len())?;
                for value in &values {
                    meter.frozen(value)?;
                }
                Ok(ConstValue::Aggregate(
                    ConstAggregate::new(descriptor.shape.clone(), values)
                        .map_err(EvalErrorKind::Value)?,
                ))
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let ValueType::Aggregate(descriptor) = &ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                if descriptor.kind != AggregateKind::Struct(*struct_symbol)
                    || fields.len() != descriptor.fields.len()
                {
                    return Err(EvalErrorKind::TypeMismatch);
                }
                meter.reserve(fields.len())?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(fields.len())
                    .map_err(|_| EvalErrorKind::AllocationFailed)?;
                for (name, field_ty) in descriptor.names.iter().zip(&descriptor.fields) {
                    meter.step()?;
                    let mut matches = fields.iter().filter(|(field, _)| field == name);
                    let (_, operand) = matches.next().ok_or(EvalErrorKind::Uninitialized)?;
                    if matches.next().is_some() {
                        return Err(EvalErrorKind::InvalidIr);
                    }
                    values.push(self.operand(*operand, Some(field_ty.clone()), meter)?);
                }
                meter.aggregate(&descriptor.shape, values.len())?;
                for value in &values {
                    meter.frozen(value)?;
                }
                Ok(ConstValue::Aggregate(
                    ConstAggregate::new(descriptor.shape.clone(), values)
                        .map_err(EvalErrorKind::Value)?,
                ))
            }
            AmirRvalue::FieldAccess { base, field } => {
                let ConstValue::Aggregate(value) = self.operand(*base, None, meter)? else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                meter.step()?;
                if value.variant().is_some() {
                    if *field != arandu_middle::amir::ENUM_PAYLOAD_FIELD {
                        return Err(EvalErrorKind::InvalidIr);
                    }
                    return value
                        .values()
                        .first()
                        .cloned()
                        .ok_or(EvalErrorKind::InvalidIr);
                }
                value
                    .values()
                    .get(*field)
                    .cloned()
                    .ok_or(EvalErrorKind::InvalidIr)
            }
            AmirRvalue::IndexAccess { base, index } => {
                let base = self.operand(*base, None, meter)?;
                let index = self.index_operand(*index, meter)?;
                meter.step()?;
                match base {
                    ConstValue::Aggregate(value) if value.variant().is_none() => value
                        .values()
                        .get(index)
                        .cloned()
                        .ok_or(EvalErrorKind::InvalidIr),
                    ConstValue::Bytes(value) => {
                        let ValueType::Integer(integer) = ty else {
                            return Err(EvalErrorKind::TypeMismatch);
                        };
                        let value = value
                            .as_bytes()
                            .get(index)
                            .ok_or(EvalErrorKind::InvalidIr)?;
                        ConstInt::new(integer, i128::from(*value))
                            .map(ConstValue::Integer)
                            .map_err(EvalErrorKind::Value)
                    }
                    _ => Err(EvalErrorKind::TypeMismatch),
                }
            }
            AmirRvalue::Len(base) => {
                let length = match self.operand(*base, None, meter)? {
                    ConstValue::Aggregate(value) if value.variant().is_none() => {
                        value.values().len()
                    }
                    ConstValue::String(value) => value.len(),
                    ConstValue::Bytes(value) => value.len(),
                    _ => return Err(EvalErrorKind::TypeMismatch),
                };
                let ValueType::Integer(integer) = ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                ConstInt::new(
                    integer,
                    i128::try_from(length).map_err(|_| EvalErrorKind::ValueLimit)?,
                )
                .map(ConstValue::Integer)
                .map_err(EvalErrorKind::Value)
            }
            AmirRvalue::StrBytes { source } => {
                let ConstValue::String(value) = self.operand(*source, None, meter)? else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                Ok(ConstValue::Bytes(value.bytes()))
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let ConstValue::Bytes(value) = self.operand(*slice, None, meter)? else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let start = self.index_operand(*start, meter)?;
                let len = self.index_operand(*len, meter)?;
                value
                    .view(start, len)
                    .map(ConstValue::Bytes)
                    .map_err(|_| EvalErrorKind::InvalidIr)
            }
            AmirRvalue::VectorSplat { value } => {
                let ValueType::Aggregate(descriptor) = &ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let AggregateKind::Vector(shape) = descriptor.kind else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let lanes = usize::from(shape.lanes());
                let lane_ty = descriptor
                    .fields
                    .first()
                    .cloned()
                    .ok_or(EvalErrorKind::InvalidIr)?;
                let lane_val = self.operand(*value, Some(lane_ty), meter)?;
                meter.reserve(lanes)?;
                let values = vec![lane_val; lanes];
                meter.aggregate(&descriptor.shape, lanes)?;
                for v in &values {
                    meter.frozen(v)?;
                }
                Ok(ConstValue::Aggregate(
                    ConstAggregate::new(descriptor.shape.clone(), values)
                        .map_err(EvalErrorKind::Value)?,
                ))
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                let ConstValue::Aggregate(agg) = self.operand(*vector, None, meter)? else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let index = usize::from(*lane);
                let val = agg.values().get(index).ok_or(EvalErrorKind::InvalidIr)?;
                meter.step()?;
                Ok(val.clone())
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                let ValueType::Aggregate(descriptor) = &ty else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let AggregateKind::Vector(shape) = descriptor.kind else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let index = usize::from(*lane);
                if index >= usize::from(shape.lanes()) {
                    return Err(EvalErrorKind::InvalidIr);
                }
                let ConstValue::Aggregate(agg) = self.operand(*vector, Some(ty.clone()), meter)?
                else {
                    return Err(EvalErrorKind::TypeMismatch);
                };
                let lane_ty = descriptor
                    .fields
                    .get(index)
                    .cloned()
                    .ok_or(EvalErrorKind::InvalidIr)?;
                let new_lane = self.operand(*value, Some(lane_ty), meter)?;
                let mut values = agg.values().to_vec();
                if index >= values.len() {
                    return Err(EvalErrorKind::InvalidIr);
                }
                values[index] = new_lane;
                meter.aggregate(&descriptor.shape, values.len())?;
                for v in &values {
                    meter.frozen(v)?;
                }
                Ok(ConstValue::Aggregate(
                    ConstAggregate::new(descriptor.shape.clone(), values)
                        .map_err(EvalErrorKind::Value)?,
                ))
            }
            AmirRvalue::VectorOp { op, left, right } => {
                self.rvalue_vector_op(op, left, right, ty, meter)
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                self.rvalue_vector_compare(op, left, right, meter)
            }
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => self.rvalue_vector_select(mask, if_true, if_false, ty, meter),
            AmirRvalue::VectorShuffle { left, right, mask } => {
                self.rvalue_vector_shuffle(left, right, mask, ty, meter)
            }
            _ => Err(EvalErrorKind::UnsupportedOperation),
        }
    }
}
