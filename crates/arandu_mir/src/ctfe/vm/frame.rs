use std::sync::Arc;

use super::{CtfeFunction, EvalError, EvalErrorKind, Frame, Meter, ValueType, slice_range};
use arandu_middle::amir::*;
use arandu_middle::ctfe::{
    ConstAggregate, ConstFloat, ConstInt, ConstString, ConstValue, FloatType, IntegerType,
};
use arandu_middle::literal_pool::{AmirLiteralEntry, parse_int_literal};
use arandu_middle::types::Primitive;

fn slots(count: usize) -> Result<Vec<Option<ConstValue>>, EvalErrorKind> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| EvalErrorKind::AllocationFailed)?;
    values.resize(count, None);
    Ok(values)
}

impl Frame {
    pub(super) fn index_operand<C: FnMut() -> bool>(
        &self,
        operand: AmirOperand,
        meter: &mut Meter<'_, C>,
    ) -> Result<usize, EvalErrorKind> {
        let ConstValue::Integer(value) = self.operand(operand, None, meter)? else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        usize::try_from(value.value()).map_err(|_| EvalErrorKind::InvalidIr)
    }

    pub(super) fn place_path<C: FnMut() -> bool>(
        &self,
        place: &AmirPlace,
        meter: &mut Meter<'_, C>,
    ) -> Result<(ValueType, Vec<usize>), EvalErrorKind> {
        let mut ty = self
            .unit
            .local_types
            .get(place.local.as_usize())
            .cloned()
            .ok_or(EvalErrorKind::InvalidIr)?;
        let mut path = Vec::new();
        if place.projections.len() > arandu_middle::types::TypeShape::MAX_DEPTH {
            return Err(EvalErrorKind::ValueLimit);
        }
        for projection in &place.projections {
            meter.step()?;
            let ValueType::Aggregate(descriptor) = &ty else {
                return Err(EvalErrorKind::TypeMismatch);
            };
            let index = match projection {
                AmirProjection::Field(symbol) => descriptor
                    .symbols
                    .iter()
                    .position(|field| *field == Some(*symbol))
                    .ok_or(EvalErrorKind::InvalidIr)?,
                AmirProjection::TupleField(index) | AmirProjection::IndexConstant(index) => *index,
                AmirProjection::Index(index) => self.index_operand(*index, meter)?,
                AmirProjection::Deref
                | AmirProjection::Variant(_)
                | AmirProjection::Payload { .. } => {
                    return Err(EvalErrorKind::UnsupportedOperation);
                }
            };
            ty = descriptor
                .fields
                .get(index)
                .cloned()
                .ok_or(EvalErrorKind::InvalidIr)?;
            path.push(index);
        }
        Ok((ty, path))
    }

    pub(super) fn load_place<C: FnMut() -> bool>(
        &self,
        place: &AmirPlace,
        meter: &mut Meter<'_, C>,
    ) -> Result<(ValueType, ConstValue), EvalErrorKind> {
        let (ty, path) = self.place_path(place, meter)?;
        let mut value = self
            .locals
            .get(place.local.as_usize())
            .ok_or(EvalErrorKind::InvalidIr)?
            .as_ref()
            .ok_or(EvalErrorKind::Uninitialized)?;
        for index in path {
            meter.step()?;
            let ConstValue::Aggregate(aggregate) = value else {
                return Err(EvalErrorKind::TypeMismatch);
            };
            value = aggregate
                .values()
                .get(index)
                .ok_or(EvalErrorKind::InvalidIr)?;
        }
        Ok((ty, value.clone()))
    }

    pub(super) fn store_place<C: FnMut() -> bool>(
        &mut self,
        place: &AmirPlace,
        value: ConstValue,
        meter: &mut Meter<'_, C>,
    ) -> Result<(), EvalErrorKind> {
        let (_, path) = self.place_path(place, meter)?;
        let slot = self
            .locals
            .get_mut(place.local.as_usize())
            .ok_or(EvalErrorKind::InvalidIr)?;
        if path.is_empty() {
            *slot = Some(value);
            return Ok(());
        }
        fn update<C: FnMut() -> bool>(
            node: &mut ConstValue,
            path: &[usize],
            value: ConstValue,
            meter: &mut Meter<'_, C>,
        ) -> Result<(), EvalErrorKind> {
            meter.step()?;
            if path.is_empty() {
                *node = value;
                return Ok(());
            }
            let ConstValue::Aggregate(aggregate) = node else {
                return Err(EvalErrorKind::TypeMismatch);
            };
            meter.aggregate(aggregate.shape(), aggregate.values().len())?;
            for child in aggregate.values() {
                meter.frozen(child)?;
            }
            let mut children = aggregate.values().to_vec();
            update(
                children.get_mut(path[0]).ok_or(EvalErrorKind::InvalidIr)?,
                &path[1..],
                value,
                meter,
            )?;
            *node = ConstValue::Aggregate(
                ConstAggregate::new(aggregate.shape().clone(), children)
                    .map_err(EvalErrorKind::Value)?,
            );
            Ok(())
        }
        update(
            slot.as_mut().ok_or(EvalErrorKind::Uninitialized)?,
            &path,
            value,
            meter,
        )
    }
    pub(super) fn validate<C: FnMut() -> bool>(
        unit: &CtfeFunction,
        meter: &mut Meter<'_, C>,
    ) -> Result<(), EvalErrorKind> {
        let function = &unit.function;
        if function.blocks.is_empty() {
            return Err(EvalErrorKind::InvalidIr);
        }
        if let Some(receiver) = &function.receiver
            && function.params.first() != Some(&receiver.temp)
        {
            return Err(EvalErrorKind::InvalidIr);
        }
        // Inspect every block before executing this function, including dead
        // branches. Optimizers must not hide an inadmissible effect from CTFE.
        for (index, block) in function.blocks.iter().enumerate() {
            meter.step()?;
            if block.id.as_usize() != index
                || function
                    .block_params
                    .get(slice_range(block.params)?)
                    .is_none()
            {
                return Err(EvalErrorKind::InvalidIr);
            }
            for index in slice_range(block.statements)? {
                meter.step()?;
                let id = u32::try_from(index)
                    .map(InstrId)
                    .map_err(|_| EvalErrorKind::InvalidIr)?;
                function.try_stmt(id).ok_or(EvalErrorKind::InvalidIr)?;
            }
            // Bound variable-length transfers before a visitor inspects them.
            // One terminator can hold many cases/arguments; charging only the
            // block would permit unbounded work even with a tiny fuel budget.
            match &block.terminator {
                AmirTerminator::Return | AmirTerminator::Unreachable => {}
                AmirTerminator::Goto { args, .. } => {
                    for _ in args {
                        meter.step()?;
                    }
                }
                AmirTerminator::Branch {
                    true_args,
                    false_args,
                    ..
                } => {
                    meter.step()?;
                    for _ in true_args.iter().chain(false_args) {
                        meter.step()?;
                    }
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    meter.step()?;
                    for (_, _, args) in targets {
                        meter.step()?;
                        for _ in args {
                            meter.step()?;
                        }
                    }
                    for _ in &otherwise.1 {
                        meter.step()?;
                    }
                }
                AmirTerminator::Suspend { .. } => return Err(EvalErrorKind::UnsupportedOperation),
            }
        }
        for (index, temp) in function.temps.iter().enumerate() {
            meter.step()?;
            if temp.id.as_usize() != index {
                return Err(EvalErrorKind::InvalidIr);
            }
            if !temp.is_copy {
                return Err(EvalErrorKind::UnsupportedType(temp.ty));
            }
        }
        for (index, local) in function.locals.iter().enumerate() {
            meter.step()?;
            if local.id.as_usize() != index {
                return Err(EvalErrorKind::InvalidIr);
            }
        }
        for entry in &unit.literals.entries {
            // Literal decoding is bounded too: spellings can contain arbitrarily
            // many separators/leading zeros despite producing a small scalar.
            let text = match entry {
                AmirLiteralEntry::Int(text)
                | AmirLiteralEntry::Float(text)
                | AmirLiteralEntry::Str(text) => text,
                AmirLiteralEntry::FloatBits(_) => {
                    meter.step()?;
                    continue;
                }
                AmirLiteralEntry::Char(_) => return Err(EvalErrorKind::InvalidLiteral),
            };
            for _ in text.bytes() {
                meter.step()?;
            }
        }
        // No runtime frame or argument values are needed for admission.
        // Kept separate so recursive calls are validated once per closure.
        Ok(())
    }

    pub(super) fn new<C: FnMut() -> bool>(
        unit: Arc<CtfeFunction>,
        args: &[ConstValue],
        destination: Option<TempId>,
        meter: &mut Meter<'_, C>,
    ) -> Result<Self, EvalErrorKind> {
        let function = &unit.function;
        if function.params.len() != args.len() {
            return Err(EvalErrorKind::InvalidIr);
        }
        let count = function
            .temps
            .len()
            .checked_add(function.locals.len())
            .ok_or(EvalErrorKind::ValueLimit)?;
        meter.reserve(count)?;
        let mut frame = Self {
            temps: slots(function.temps.len())?,
            locals: slots(function.locals.len())?,
            block: BlockId(0),
            instruction: 0,
            destination,
            origin: unit.origin(),
            unit,
        };
        for (&parameter, argument) in frame.unit.function.params.iter().zip(args) {
            meter.step()?;
            let index = parameter.as_usize();
            let ty = frame
                .unit
                .temp_types
                .get(index)
                .ok_or(EvalErrorKind::InvalidIr)?;
            if !ty.accepts(argument) {
                return Err(EvalErrorKind::TypeMismatch);
            }
            *frame.temps.get_mut(index).ok_or(EvalErrorKind::InvalidIr)? = Some(argument.clone());
        }
        meter.values += count;
        Ok(frame)
    }

    pub(super) fn operand_type(
        &self,
        operand: AmirOperand,
    ) -> Result<Option<ValueType>, EvalErrorKind> {
        match operand {
            AmirOperand::Copy(id) | AmirOperand::Move(id) => self
                .unit
                .temp_types
                .get(id.as_usize())
                .cloned()
                .map(Some)
                .ok_or(EvalErrorKind::InvalidIr),
            AmirOperand::Constant(AmirConstant::Bool(_)) => Ok(Some(ValueType::Bool)),
            AmirOperand::Constant(AmirConstant::Nil) => Ok(Some(ValueType::Void)),
            AmirOperand::Constant(AmirConstant::Pool(_)) => Ok(None),
            AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => {
                Err(EvalErrorKind::UnsupportedOperation)
            }
        }
    }

    pub(super) fn operand<C: FnMut() -> bool>(
        &self,
        operand: AmirOperand,
        hint: Option<ValueType>,
        meter: &mut Meter<'_, C>,
    ) -> Result<ConstValue, EvalErrorKind> {
        let value = match operand {
            // Every admitted scalar is Copy. Move here is a transfer of scalar
            // bits, not a simulation of owned runtime memory.
            AmirOperand::Copy(id) | AmirOperand::Move(id) => self
                .temps
                .get(id.as_usize())
                .ok_or(EvalErrorKind::InvalidIr)?
                .as_ref()
                .ok_or(EvalErrorKind::Uninitialized)?
                .clone(),
            AmirOperand::Constant(AmirConstant::Bool(value)) => ConstValue::Bool(value),
            AmirOperand::Constant(AmirConstant::Nil) => ConstValue::Void,
            AmirOperand::Constant(AmirConstant::Pool(id)) => {
                let index = usize::try_from(id.0).map_err(|_| EvalErrorKind::InvalidIr)?;
                let entry = self
                    .unit
                    .literals
                    .entries
                    .get(index)
                    .ok_or(EvalErrorKind::InvalidLiteral)?;
                if let AmirLiteralEntry::FloatBits(value) = entry {
                    meter.step()?;
                    let value = match hint {
                        Some(ValueType::Float(destination)) => value
                            .cast(destination)
                            .map_err(|_| EvalErrorKind::TypeMismatch)?,
                        Some(_) => return Err(EvalErrorKind::TypeMismatch),
                        None => *value,
                    };
                    return Ok(ConstValue::Float(value));
                }
                let text = match entry {
                    AmirLiteralEntry::Int(text)
                    | AmirLiteralEntry::Float(text)
                    | AmirLiteralEntry::Str(text) => text,
                    AmirLiteralEntry::Char(_) | AmirLiteralEntry::FloatBits(_) => {
                        return Err(EvalErrorKind::InvalidLiteral);
                    }
                };
                // Charge every decoding, not just admission: a loop can read
                // the same long spelling repeatedly. Bounding each parse keeps
                // work proportional to fuel rather than fuel × spelling size.
                for _ in text.bytes() {
                    meter.step()?;
                }
                if matches!(entry, AmirLiteralEntry::Str(_)) {
                    meter.bytes(text.len())?;
                    return Ok(ConstValue::String(ConstString::new(text.as_str())));
                }
                if matches!(entry, AmirLiteralEntry::Float(_)) {
                    let ty = match hint {
                        Some(ValueType::Float(ty)) => ty,
                        None => FloatType::new(Primitive::Float, self.unit.layout)
                            .map_err(|_| EvalErrorKind::InvalidLiteral)?,
                        _ => return Err(EvalErrorKind::TypeMismatch),
                    };
                    return ConstFloat::parse(ty, text)
                        .map(ConstValue::Float)
                        .map_err(|_| EvalErrorKind::InvalidLiteral);
                }
                let value = parse_int_literal(text).ok_or(EvalErrorKind::InvalidLiteral)?;
                if let Some(ValueType::Float(ty)) = hint {
                    return ConstFloat::parse(ty, text)
                        .map(ConstValue::Float)
                        .map_err(|_| EvalErrorKind::InvalidLiteral);
                }
                let ty = match hint {
                    Some(ValueType::Integer(ty)) => ty,
                    None => IntegerType::new(Primitive::Int, self.unit.layout)
                        .map_err(EvalErrorKind::Value)?,
                    _ => return Err(EvalErrorKind::TypeMismatch),
                };
                ConstValue::Integer(ConstInt::new(ty, value).map_err(EvalErrorKind::Value)?)
            }
            AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => {
                return Err(EvalErrorKind::UnsupportedOperation);
            }
        };
        if hint.is_some_and(|ty| !ty.accepts(&value)) {
            return Err(EvalErrorKind::TypeMismatch);
        }
        Ok(value)
    }

    pub(super) fn assign(&mut self, id: TempId, value: ConstValue) -> Result<(), EvalErrorKind> {
        let ty = self
            .unit
            .temp_types
            .get(id.as_usize())
            .ok_or(EvalErrorKind::InvalidIr)?;
        if !ty.accepts(&value) {
            return Err(EvalErrorKind::TypeMismatch);
        }
        *self
            .temps
            .get_mut(id.as_usize())
            .ok_or(EvalErrorKind::InvalidIr)? = Some(value);
        Ok(())
    }

    pub(super) fn jump<C: FnMut() -> bool>(
        &mut self,
        target: BlockId,
        args: &[AmirOperand],
        meter: &mut Meter<'_, C>,
    ) -> Result<(), EvalErrorKind> {
        let unit = Arc::clone(&self.unit);
        let block = unit
            .function
            .blocks
            .get(target.as_usize())
            .ok_or(EvalErrorKind::InvalidIr)?;
        let parameters = unit
            .function
            .block_params
            .get(slice_range(block.params)?)
            .ok_or(EvalErrorKind::InvalidIr)?;
        if args.len() != parameters.len() {
            return Err(EvalErrorKind::InvalidIr);
        }
        meter.reserve(args.len())?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(args.len())
            .map_err(|_| EvalErrorKind::AllocationFailed)?;
        for (&operand, parameter) in args.iter().zip(parameters) {
            meter.step()?;
            let ty = self
                .unit
                .temp_types
                .get(parameter.id.as_usize())
                .ok_or(EvalErrorKind::InvalidIr)?
                .clone();
            values.push(self.operand(operand, Some(ty), meter)?);
        }
        // Read all arguments before overwriting any block parameter (phi swap).
        for (parameter, value) in parameters.iter().zip(values) {
            self.assign(parameter.id, value)?;
        }
        self.block = target;
        self.instruction = 0;
        Ok(())
    }

    pub(super) fn error(&self, kind: EvalErrorKind) -> EvalError {
        self.origin.error(kind)
    }
}
