use std::sync::Arc;

use super::{
    Admission, Budget, CtfeFunction, EvalError, EvalErrorKind, Frame, FunctionProvider, MAX_TRACE,
    Meter, ValueType, slice_range, types::AggregateKind,
};
use arandu_middle::amir::*;
use arandu_middle::ctfe::{ConstAggregate, ConstInt, ConstValue, IntegerType};
use arandu_middle::intrinsics::{ArithmeticInt, IntrinsicKind, OverflowOp, SaturatingOp};

enum Action {
    Continue,
    Call {
        unit: Arc<CtfeFunction>,
        values: Vec<ConstValue>,
        destination: Option<TempId>,
    },
    Return(ConstValue),
}

pub(super) fn evaluate_root<P: FunctionProvider, C: FnMut() -> bool>(
    provider: &P,
    unit: Arc<CtfeFunction>,
    source_root: bool,
    args: &[ConstValue],
    budget: Budget,
    mut cancelled: C,
) -> Result<ConstValue, EvalError> {
    let origin = unit.origin();
    let location = |kind| origin.error(kind);
    let mut meter = Meter {
        fuel: budget.fuel,
        values: 0,
        max_values: usize::try_from(budget.values)
            .map_err(|_| location(EvalErrorKind::ValueLimit))?,
        max_frames: usize::try_from(budget.frames)
            .map_err(|_| location(EvalErrorKind::FrameLimit))?,
        aggregate_nodes: 0,
        aggregate_bytes: 0,
        arena: Vec::new(),
        cancelled: &mut cancelled,
    };
    meter.step().map_err(location)?;
    if meter.max_frames == 0 {
        return Err(location(EvalErrorKind::FrameLimit));
    }
    let layout = unit.layout;
    let admitted = Admission::inspect(provider, Arc::clone(&unit), source_root, &mut meter)?;
    for argument in args {
        meter.input(argument).map_err(location)?;
    }
    let root = Frame::new(unit, args, None, &mut meter).map_err(location)?;
    let mut stack = Vec::new();
    stack
        .try_reserve(1)
        .map_err(|_| location(EvalErrorKind::AllocationFailed))?;
    stack.push(root);
    let result = (|| {
        loop {
            let depth = stack.len();
            let Some(frame) = stack.last_mut() else {
                return Err(location(EvalErrorKind::InvalidIr));
            };
            let step = (|| -> Result<Action, EvalErrorKind> {
                meter.step()?;
                let unit = Arc::clone(&frame.unit);
                let block = unit
                    .function
                    .blocks
                    .get(frame.block.as_usize())
                    .ok_or(EvalErrorKind::InvalidIr)?;
                let range = slice_range(block.statements)?;
                if frame.instruction < range.len() {
                    let index = range
                        .start
                        .checked_add(frame.instruction)
                        .ok_or(EvalErrorKind::InvalidIr)?;
                    let id = u32::try_from(index)
                        .map(InstrId)
                        .map_err(|_| EvalErrorKind::InvalidIr)?;
                    let stmt = unit.function.try_stmt(id).ok_or(EvalErrorKind::InvalidIr)?;
                    frame.origin = unit.statement_origin(frame.block, stmt);
                    frame.instruction += 1;
                    match stmt {
                        AmirStmt::Assign { lhs, rhs } => {
                            let ty = frame
                                .unit
                                .temp_types
                                .get(lhs.as_usize())
                                .ok_or(EvalErrorKind::InvalidIr)?
                                .clone();
                            let value = frame.rvalue(rhs, ty, &mut meter)?;
                            frame.assign(*lhs, value)?;
                        }
                        AmirStmt::Store { lhs, rhs } => {
                            let (ty, _, _) = frame.place_path(lhs, &mut meter)?;
                            let value = frame.operand(*rhs, Some(ty), &mut meter)?;
                            frame.store_place(lhs, value, &mut meter)?;
                        }
                        AmirStmt::StorageLive(id) | AmirStmt::StorageDead(id) => {
                            *frame
                                .locals
                                .get_mut(id.as_usize())
                                .ok_or(EvalErrorKind::InvalidIr)? = None;
                        }
                        AmirStmt::Nop => {}
                        AmirStmt::Call {
                            lhs,
                            callee: AmirOperand::FunctionRef(callee),
                            args,
                            ..
                        } => {
                            if let Some(intrinsic) = admitted.intrinsic(*callee) {
                                if intrinsic == IntrinsicKind::Abort {
                                    return Err(EvalErrorKind::UnsupportedOperation);
                                }
                                if intrinsic == IntrinsicKind::SpinLoopHint {
                                    meter.step()?;
                                    return Ok(Action::Continue);
                                }
                                if matches!(
                                    intrinsic,
                                    IntrinsicKind::Likely | IntrinsicKind::Unlikely
                                ) {
                                    if args.len() != 1 {
                                        return Err(EvalErrorKind::InvalidIr);
                                    }
                                    meter.step()?;
                                    let cond = frame.operand(
                                        args[0],
                                        Some(ValueType::Bool),
                                        &mut meter,
                                    )?;
                                    if !matches!(cond, ConstValue::Bool(_)) {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    }
                                    if let Some(lhs) = lhs {
                                        frame.assign(*lhs, cond)?;
                                    }
                                    return Ok(Action::Continue);
                                }
                                if intrinsic == IntrinsicKind::Fma {
                                    if args.len() != 3 {
                                        return Err(EvalErrorKind::InvalidIr);
                                    }
                                    meter.step()?;
                                    let a_val = frame.operand(args[0], None, &mut meter)?;
                                    let b_val = frame.operand(args[1], None, &mut meter)?;
                                    let c_val = frame.operand(args[2], None, &mut meter)?;
                                    let (
                                        ConstValue::Float(a),
                                        ConstValue::Float(b),
                                        ConstValue::Float(c),
                                    ) = (a_val, b_val, c_val)
                                    else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let is_64 = a.ty().bit_width() == 64;
                                    let res_bits = if is_64 {
                                        let fa = f64::from_bits(a.bits());
                                        let fb = f64::from_bits(b.bits());
                                        let fc = f64::from_bits(c.bits());
                                        fa.mul_add(fb, fc).to_bits()
                                    } else {
                                        let fa = f32::from_bits(a.bits() as u32);
                                        let fb = f32::from_bits(b.bits() as u32);
                                        let fc = f32::from_bits(c.bits() as u32);
                                        u64::from(fa.mul_add(fb, fc).to_bits())
                                    };
                                    let result_float =
                                        arandu_middle::ctfe::ConstFloat::new(a.ty(), res_bits)
                                            .map_err(|_| EvalErrorKind::InvalidIr)?;
                                    if let Some(lhs) = lhs {
                                        frame.assign(*lhs, ConstValue::Float(result_float))?;
                                    }
                                    return Ok(Action::Continue);
                                }
                                if let IntrinsicKind::FloatToBits(width) = intrinsic {
                                    if args.len() != 1 {
                                        return Err(EvalErrorKind::InvalidIr);
                                    }
                                    meter.step()?;
                                    let arg_val = frame.operand(args[0], None, &mut meter)?;
                                    let ConstValue::Float(f) = arg_val else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let bits = match width {
                                        arandu_middle::intrinsics::FloatWidth::F64 => {
                                            f.bits() as i128
                                        }
                                        arandu_middle::intrinsics::FloatWidth::F32 => {
                                            (f.bits() as u32) as i128
                                        }
                                    };
                                    if let Some(lhs) = lhs {
                                        let dest_ty = frame
                                            .unit
                                            .temp_types
                                            .get(lhs.as_usize())
                                            .ok_or(EvalErrorKind::InvalidIr)?
                                            .clone();
                                        let ValueType::Integer(dest_int_ty) = dest_ty else {
                                            return Err(EvalErrorKind::TypeMismatch);
                                        };
                                        let val = ConstValue::Integer(
                                            ConstInt::new(dest_int_ty, bits)
                                                .map_err(EvalErrorKind::Value)?,
                                        );
                                        frame.assign(*lhs, val)?;
                                    }
                                    return Ok(Action::Continue);
                                }
                                if let IntrinsicKind::FloatFromBits(width) = intrinsic {
                                    if args.len() != 1 {
                                        return Err(EvalErrorKind::InvalidIr);
                                    }
                                    meter.step()?;
                                    let arg_val = frame.operand(args[0], None, &mut meter)?;
                                    let ConstValue::Integer(u) = arg_val else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let bits = match width {
                                        arandu_middle::intrinsics::FloatWidth::F64 => {
                                            u.value() as u64
                                        }
                                        arandu_middle::intrinsics::FloatWidth::F32 => {
                                            u64::from(u.value() as u32)
                                        }
                                    };
                                    if let Some(lhs) = lhs {
                                        let dest_ty = frame
                                            .unit
                                            .temp_types
                                            .get(lhs.as_usize())
                                            .ok_or(EvalErrorKind::InvalidIr)?
                                            .clone();
                                        let ValueType::Float(flt_ty) = dest_ty else {
                                            return Err(EvalErrorKind::TypeMismatch);
                                        };
                                        let result_float =
                                            arandu_middle::ctfe::ConstFloat::new(flt_ty, bits)
                                                .map_err(|_| EvalErrorKind::InvalidIr)?;
                                        frame.assign(*lhs, ConstValue::Float(result_float))?;
                                    }
                                    return Ok(Action::Continue);
                                }
                                if let IntrinsicKind::OverflowArithmetic(op, arith_int) = intrinsic
                                {
                                    if args.len() != 2 {
                                        return Err(EvalErrorKind::InvalidIr);
                                    }
                                    meter.step()?;
                                    let input_int_ty =
                                        IntegerType::new(arith_int.primitive(), layout)
                                            .map_err(EvalErrorKind::Value)?;
                                    let hint = Some(ValueType::Integer(input_int_ty));
                                    let ConstValue::Integer(a) =
                                        frame.operand(args[0], hint.clone(), &mut meter)?
                                    else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let ConstValue::Integer(b) =
                                        frame.operand(args[1], hint, &mut meter)?
                                    else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let (res, ov) =
                                        eval_overflow_arith(op, arith_int, a.value(), b.value());
                                    if let Some(lhs) = lhs {
                                        let dest_ty = frame
                                            .unit
                                            .temp_types
                                            .get(lhs.as_usize())
                                            .ok_or(EvalErrorKind::InvalidIr)?
                                            .clone();
                                        let ValueType::Aggregate(descriptor) = dest_ty else {
                                            return Err(EvalErrorKind::TypeMismatch);
                                        };
                                        if descriptor.kind != AggregateKind::Tuple
                                            || descriptor.fields.len() != 2
                                        {
                                            return Err(EvalErrorKind::TypeMismatch);
                                        }
                                        let ValueType::Integer(dest_int_ty) = descriptor.fields[0]
                                        else {
                                            return Err(EvalErrorKind::TypeMismatch);
                                        };
                                        meter.aggregate(&descriptor.shape, 2)?;
                                        let val = ConstValue::Integer(
                                            ConstInt::new(dest_int_ty, res)
                                                .map_err(EvalErrorKind::Value)?,
                                        );
                                        let agg = ConstAggregate::new(
                                            descriptor.shape.clone(),
                                            vec![val, ConstValue::Bool(ov)],
                                        )
                                        .map_err(EvalErrorKind::Value)?;
                                        frame.assign(*lhs, ConstValue::Aggregate(agg))?;
                                    }
                                    return Ok(Action::Continue);
                                }
                                if let IntrinsicKind::SaturatingArithmetic(op, arith_int) =
                                    intrinsic
                                {
                                    if args.len() != 2 {
                                        return Err(EvalErrorKind::InvalidIr);
                                    }
                                    meter.step()?;
                                    let input_int_ty =
                                        IntegerType::new(arith_int.primitive(), layout)
                                            .map_err(EvalErrorKind::Value)?;
                                    let hint = Some(ValueType::Integer(input_int_ty));
                                    let ConstValue::Integer(a) =
                                        frame.operand(args[0], hint.clone(), &mut meter)?
                                    else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let ConstValue::Integer(b) =
                                        frame.operand(args[1], hint, &mut meter)?
                                    else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let res =
                                        eval_saturating_arith(op, arith_int, a.value(), b.value());
                                    if let Some(lhs) = lhs {
                                        let dest_ty = frame
                                            .unit
                                            .temp_types
                                            .get(lhs.as_usize())
                                            .ok_or(EvalErrorKind::InvalidIr)?
                                            .clone();
                                        let ValueType::Integer(dest_int_ty) = dest_ty else {
                                            return Err(EvalErrorKind::TypeMismatch);
                                        };
                                        let val = ConstValue::Integer(
                                            ConstInt::new(dest_int_ty, res)
                                                .map_err(EvalErrorKind::Value)?,
                                        );
                                        frame.assign(*lhs, val)?;
                                    }
                                    return Ok(Action::Continue);
                                }
                                if Some(args.len()) != intrinsic.bit_operand_count() {
                                    return Err(EvalErrorKind::InvalidIr);
                                }
                                meter.step()?;
                                let arg_val = frame.operand(args[0], None, &mut meter)?;
                                let ConstValue::Integer(input_int) = arg_val else {
                                    return Err(EvalErrorKind::TypeMismatch);
                                };
                                // Rotation amounts are `u32` and reduced modulo the
                                // width by `rotate_*`, matching every backend.
                                let amount = if args.len() == 2 {
                                    let amount_val = frame.operand(args[1], None, &mut meter)?;
                                    let ConstValue::Integer(amount_int) = amount_val else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    amount_int.value() as u32
                                } else {
                                    0
                                };
                                let is_64 = input_int.ty().bit_width() == 64;
                                let res_i128 = if is_64 {
                                    let u = input_int.value() as u64;
                                    match intrinsic {
                                        IntrinsicKind::CountOnes => i128::from(u.count_ones()),
                                        IntrinsicKind::LeadingZeros => {
                                            i128::from(u.leading_zeros())
                                        }
                                        IntrinsicKind::TrailingZeros => {
                                            i128::from(u.trailing_zeros())
                                        }
                                        IntrinsicKind::ReverseBits => i128::from(u.reverse_bits()),
                                        IntrinsicKind::RotateLeft => {
                                            i128::from(u.rotate_left(amount))
                                        }
                                        IntrinsicKind::RotateRight => {
                                            i128::from(u.rotate_right(amount))
                                        }
                                        IntrinsicKind::SwapBytes => i128::from(u.swap_bytes()),
                                        _ => return Err(EvalErrorKind::UnsupportedOperation),
                                    }
                                } else {
                                    let u = input_int.value() as u32;
                                    match intrinsic {
                                        IntrinsicKind::CountOnes => i128::from(u.count_ones()),
                                        IntrinsicKind::LeadingZeros => {
                                            i128::from(u.leading_zeros())
                                        }
                                        IntrinsicKind::TrailingZeros => {
                                            i128::from(u.trailing_zeros())
                                        }
                                        IntrinsicKind::ReverseBits => i128::from(u.reverse_bits()),
                                        IntrinsicKind::RotateLeft => {
                                            i128::from(u.rotate_left(amount))
                                        }
                                        IntrinsicKind::RotateRight => {
                                            i128::from(u.rotate_right(amount))
                                        }
                                        IntrinsicKind::SwapBytes => i128::from(u.swap_bytes()),
                                        _ => return Err(EvalErrorKind::UnsupportedOperation),
                                    }
                                };
                                if let Some(lhs) = lhs {
                                    let ty = frame
                                        .unit
                                        .temp_types
                                        .get(lhs.as_usize())
                                        .ok_or(EvalErrorKind::InvalidIr)?
                                        .clone();
                                    let ValueType::Integer(dest_int_ty) = ty else {
                                        return Err(EvalErrorKind::TypeMismatch);
                                    };
                                    let result_val = ConstValue::Integer(
                                        ConstInt::new(dest_int_ty, res_i128)
                                            .map_err(EvalErrorKind::Value)?,
                                    );
                                    frame.assign(*lhs, result_val)?;
                                }
                            } else {
                                if depth >= meter.max_frames {
                                    return Err(EvalErrorKind::FrameLimit);
                                }
                                meter.reserve(args.len())?;
                                let key = unit.call_identity(*callee);
                                let callee_unit = admitted.function(&key)?;
                                if callee_unit.identity != key {
                                    return Err(EvalErrorKind::InvalidIr);
                                }
                                if callee_unit.layout != layout {
                                    return Err(EvalErrorKind::TargetMismatch);
                                }
                                if callee_unit.function.params.len() != args.len() {
                                    return Err(EvalErrorKind::InvalidIr);
                                }
                                let mut values = Vec::new();
                                values
                                    .try_reserve_exact(args.len())
                                    .map_err(|_| EvalErrorKind::AllocationFailed)?;
                                for (&operand, &parameter) in
                                    args.iter().zip(&callee_unit.function.params)
                                {
                                    meter.step()?;
                                    let ty = callee_unit
                                        .temp_types
                                        .get(parameter.as_usize())
                                        .ok_or(EvalErrorKind::InvalidIr)?
                                        .clone();
                                    values.push(frame.operand(operand, Some(ty), &mut meter)?);
                                }
                                return Ok(Action::Call {
                                    unit: callee_unit,
                                    values,
                                    destination: *lhs,
                                });
                            }
                        }
                        AmirStmt::ArenaRewind(operand) => {
                            let mark = frame.index_operand(*operand, &mut meter)?;
                            meter.arena_rewind(mark)?;
                        }
                        AmirStmt::Call { .. } | AmirStmt::Free(_) | AmirStmt::Destroy(_) => {
                            return Err(EvalErrorKind::UnsupportedOperation);
                        }
                    }
                } else {
                    frame.origin.block = frame.block;
                    match &block.terminator {
                        AmirTerminator::Goto { target, args } => {
                            frame.jump(*target, args, &mut meter)?;
                        }
                        AmirTerminator::Branch {
                            condition,
                            if_true,
                            true_args,
                            if_false,
                            false_args,
                        } => {
                            let ConstValue::Bool(condition) =
                                frame.operand(*condition, Some(ValueType::Bool), &mut meter)?
                            else {
                                return Err(EvalErrorKind::TypeMismatch);
                            };
                            let (target, args) = if condition {
                                (*if_true, true_args)
                            } else {
                                (*if_false, false_args)
                            };
                            frame.jump(target, args, &mut meter)?;
                        }
                        AmirTerminator::SwitchInt {
                            discriminant,
                            targets,
                            otherwise,
                        } => {
                            let ConstValue::Integer(value) =
                                frame.operand(*discriminant, None, &mut meter)?
                            else {
                                return Err(EvalErrorKind::TypeMismatch);
                            };
                            let mut selected = (&otherwise.0, &otherwise.1);
                            for (tag, target, args) in targets {
                                meter.step()?;
                                if *tag == value.value() {
                                    selected = (target, args);
                                    break;
                                }
                            }
                            let (target, args) = selected;
                            frame.jump(*target, args, &mut meter)?;
                        }
                        AmirTerminator::Return => {
                            let value = if unit.return_type == ValueType::Void {
                                ConstValue::Void
                            } else {
                                frame.operand(
                                    AmirOperand::Copy(TempId(0)),
                                    Some(unit.return_type.clone()),
                                    &mut meter,
                                )?
                            };
                            return Ok(Action::Return(value));
                        }
                        AmirTerminator::Unreachable => return Err(EvalErrorKind::InvalidIr),
                        AmirTerminator::Trap(kind) => return Err(EvalErrorKind::Trapped(*kind)),
                        AmirTerminator::Suspend { .. } => {
                            return Err(EvalErrorKind::UnsupportedOperation);
                        }
                    }
                }
                Ok(Action::Continue)
            })();
            let action = step.map_err(|kind| frame.error(kind))?;
            match action {
                Action::Continue => {}
                Action::Call {
                    unit,
                    values,
                    destination,
                } => {
                    meter.values += values.len();
                    let child = Frame::new(unit, &values, destination, &mut meter)
                        .map_err(|kind| frame.error(kind))?;
                    meter.values -= values.len();
                    let error = frame.error(EvalErrorKind::AllocationFailed);
                    stack.try_reserve(1).map_err(|_| error)?;
                    stack.push(child);
                }
                Action::Return(value) => {
                    let destination = frame.destination;
                    let count = frame.temps.len() + frame.locals.len();
                    stack.pop();
                    meter.values -= count;
                    if let Some(parent) = stack.last_mut() {
                        if let Some(destination) = destination {
                            parent
                                .assign(destination, value)
                                .map_err(|kind| parent.error(kind))?;
                        }
                    } else {
                        return Ok(value);
                    }
                }
            }
        }
    })();
    result.map_err(|mut error: EvalError| {
        let callers = stack.len().saturating_sub(1);
        let count = callers.min(MAX_TRACE);
        error.trace_truncated = callers > count;
        if error.trace.try_reserve(count).is_err() {
            error.trace_truncated = true;
        } else {
            error.trace.extend(
                stack
                    .iter()
                    .take(callers)
                    .skip(callers - count)
                    .map(|frame| frame.origin),
            );
        }
        error
    })
}

fn eval_overflow_arith(op: OverflowOp, int_ty: ArithmeticInt, a: i128, b: i128) -> (i128, bool) {
    match int_ty {
        ArithmeticInt::I32 => {
            let (a, b) = (a as i32, b as i32);
            let (res, ov) = match op {
                OverflowOp::Add => a.overflowing_add(b),
                OverflowOp::Sub => a.overflowing_sub(b),
                OverflowOp::Mul => a.overflowing_mul(b),
            };
            (i128::from(res), ov)
        }
        ArithmeticInt::U32 => {
            let (a, b) = (a as u32, b as u32);
            let (res, ov) = match op {
                OverflowOp::Add => a.overflowing_add(b),
                OverflowOp::Sub => a.overflowing_sub(b),
                OverflowOp::Mul => a.overflowing_mul(b),
            };
            (i128::from(res), ov)
        }
        ArithmeticInt::I64 => {
            let (a, b) = (a as i64, b as i64);
            let (res, ov) = match op {
                OverflowOp::Add => a.overflowing_add(b),
                OverflowOp::Sub => a.overflowing_sub(b),
                OverflowOp::Mul => a.overflowing_mul(b),
            };
            (i128::from(res), ov)
        }
        ArithmeticInt::U64 => {
            let (a, b) = (a as u64, b as u64);
            let (res, ov) = match op {
                OverflowOp::Add => a.overflowing_add(b),
                OverflowOp::Sub => a.overflowing_sub(b),
                OverflowOp::Mul => a.overflowing_mul(b),
            };
            (i128::from(res), ov)
        }
    }
}

fn eval_saturating_arith(op: SaturatingOp, int_ty: ArithmeticInt, a: i128, b: i128) -> i128 {
    match int_ty {
        ArithmeticInt::I32 => {
            let (a, b) = (a as i32, b as i32);
            let res = match op {
                SaturatingOp::Add => a.saturating_add(b),
                SaturatingOp::Sub => a.saturating_sub(b),
            };
            i128::from(res)
        }
        ArithmeticInt::U32 => {
            let (a, b) = (a as u32, b as u32);
            let res = match op {
                SaturatingOp::Add => a.saturating_add(b),
                SaturatingOp::Sub => a.saturating_sub(b),
            };
            i128::from(res)
        }
        ArithmeticInt::I64 => {
            let (a, b) = (a as i64, b as i64);
            let res = match op {
                SaturatingOp::Add => a.saturating_add(b),
                SaturatingOp::Sub => a.saturating_sub(b),
            };
            i128::from(res)
        }
        ArithmeticInt::U64 => {
            let (a, b) = (a as u64, b as u64);
            let res = match op {
                SaturatingOp::Add => a.saturating_add(b),
                SaturatingOp::Sub => a.saturating_sub(b),
            };
            i128::from(res)
        }
    }
}
