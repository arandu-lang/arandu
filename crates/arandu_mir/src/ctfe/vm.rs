//! Bounded scalar AMIR execution. Calls use an explicit heap stack, never Rust
//! recursion. The provider owns function lookup; this module knows no database.

use std::sync::Arc;

use super::{ScalarEvalError, eval_binary, eval_unary};
use arandu_middle::amir::visit::{for_each_rvalue_operand, for_each_terminator_operand};
use arandu_middle::amir::*;
use arandu_middle::ctfe::{
    ConstAggregate, ConstFloat, ConstInt, ConstString, ConstValue, ConstValueError,
    FloatArithmetic, FloatType, IntegerType,
};
use arandu_middle::intrinsics::IntrinsicKind;
use arandu_middle::layout::{DataLayout, DataLayoutError, DenseRange};
use arandu_middle::literal_pool::{AmirLiteralEntry, AmirLiteralPool, parse_int_literal};
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{
    FunctionInstance, Primitive, TypeId, TypeInterner, TypeShape, VectorType,
};
use arandu_middle::{Span, SymbolId};
use rustc_hash::FxHashMap;

mod admission;
use admission::Admission;
mod types;
use types::{AggregateKind, ValueType};

/// Required explicitly until representative workloads establish defaults.
/// Fuel charges validation, instructions and terminators. Live value slots
/// include admitted function handles, frames and simultaneous edge/call
/// argument scratch storage. This is not a compiler-wide heap/RSS limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Budget {
    pub fuel: u64,
    pub frames: u32,
    pub values: u64,
}

/// A single function with its own literal pool and resolved scalar types.
/// Type/literal IDs never cross function units; the provider shares units by Arc.
#[derive(Debug)]
pub struct CtfeFunction {
    function: AmirFunc,
    literals: AmirLiteralPool,
    layout: DataLayout,
    temp_types: Vec<ValueType>,
    local_types: Vec<ValueType>,
    projection_types: Vec<(TypeId, ValueType)>,
    return_type: ValueType,
    identity: FunctionInstance,
    calls: Vec<(SymbolId, FunctionInstance)>,
}

impl CtfeFunction {
    #[must_use]
    pub fn function(&self) -> &AmirFunc {
        &self.function
    }

    #[must_use]
    pub fn literals(&self) -> &AmirLiteralPool {
        &self.literals
    }

    #[must_use]
    pub fn layout(&self) -> DataLayout {
        self.layout
    }

    #[must_use]
    pub fn identity(&self) -> &FunctionInstance {
        &self.identity
    }

    #[must_use]
    pub fn call_identities(&self) -> &[(SymbolId, FunctionInstance)] {
        &self.calls
    }

    /// Local synthetic IDs are meaningful only in this typed unit. Translate
    /// them to the ordinary monomorphizer's structural identities at the edge.
    pub fn bind_instance(
        mut self,
        identity: FunctionInstance,
        mut calls: Vec<(SymbolId, FunctionInstance)>,
    ) -> Result<Self, EvalErrorKind> {
        calls.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        if calls.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(EvalErrorKind::InvalidIr);
        }
        self.identity = identity;
        self.calls = calls;
        Ok(self)
    }

    fn call_identity(&self, symbol: SymbolId) -> FunctionInstance {
        self.calls
            .binary_search_by_key(&(symbol.file_id, symbol.local_id.0), |(symbol, _)| {
                (symbol.file_id, symbol.local_id.0)
            })
            .map_or_else(
                |_| FunctionInstance {
                    definition: symbol,
                    arguments: Vec::new(),
                },
                |index| self.calls[index].1.clone(),
            )
    }

    pub fn new(
        function: AmirFunc,
        literals: AmirLiteralPool,
        types: &TypeInterner,
        layout: DataLayout,
    ) -> Result<Self, EvalErrorKind> {
        Self::build(function, literals, types, layout, None)
    }

    /// Admit nominal aggregates through the shared canonical metadata/Copy
    /// proof, never by guessing from their names or physical memory layout.
    pub fn new_with_provider(
        function: AmirFunc,
        literals: AmirLiteralPool,
        types: &TypeInterner,
        layout: DataLayout,
        provider: &dyn arandu_middle::layout::StructLayoutProvider,
    ) -> Result<Self, EvalErrorKind> {
        Self::build(function, literals, types, layout, Some(provider))
    }

    fn build(
        function: AmirFunc,
        literals: AmirLiteralPool,
        types: &TypeInterner,
        layout: DataLayout,
        provider: Option<&dyn arandu_middle::layout::StructLayoutProvider>,
    ) -> Result<Self, EvalErrorKind> {
        layout.validate().map_err(EvalErrorKind::InvalidLayout)?;
        IntegerType::new(Primitive::USize, layout).map_err(EvalErrorKind::Value)?;
        let mut remaining = arandu_middle::types::TypeShape::MAX_NODES;
        let mut descriptors: FxHashMap<TypeId, ValueType> = FxHashMap::default();
        let mut resolve = |id| {
            if let Some(value) = descriptors.get(&id) {
                return Ok(value.clone());
            }
            let value = ValueType::resolve(id, types, layout, provider, 0, &mut remaining)?;
            descriptors.insert(id, value.clone());
            Ok::<_, EvalErrorKind>(value)
        };
        let return_type = resolve(function.return_type)?;
        let temp_types = function
            .temps
            .iter()
            .map(|temp| resolve(temp.ty))
            .collect::<Result<_, _>>()?;
        let local_types = function
            .locals
            .iter()
            .map(|local| resolve(local.ty))
            .collect::<Result<_, _>>()?;
        let mut projection_types = Vec::new();
        for stmt in function.stmts.iter_ids().map(|id| function.stmt(id)) {
            if let AmirStmt::Assign {
                rhs:
                    AmirRvalue::EnumPayload {
                        field_ty, tuple_ty, ..
                    },
                ..
            } = stmt
            {
                for id in std::iter::once(*field_ty).chain(*tuple_ty) {
                    if !projection_types.iter().any(|(known, _)| *known == id) {
                        projection_types.push((id, resolve(id)?));
                    }
                }
            }
        }
        Ok(Self {
            identity: FunctionInstance {
                definition: function.symbol,
                arguments: Vec::new(),
            },
            calls: Vec::new(),
            function,
            literals,
            layout,
            temp_types,
            local_types,
            projection_types,
            return_type,
        })
    }

    /// Canonical result type, reconstructed without exposing this unit's IDs.
    #[must_use]
    pub fn result_type_shape(&self) -> arandu_middle::types::TypeShape {
        self.return_type.shape()
    }

    /// Pool-independent type encodings for semantic hashing by query consumers.
    pub fn scalar_type_bytes(&self) -> impl Iterator<Item = Vec<u8>> + '_ {
        std::iter::once(&self.return_type)
            .chain(&self.temp_types)
            .chain(&self.local_types)
            .chain(self.projection_types.iter().map(|(_, ty)| ty))
            .map(ValueType::canonical_bytes)
    }

    fn origin(&self) -> EvalLocation {
        EvalLocation {
            function: self.identity.definition,
            block: BlockId(0),
            span: self
                .function
                .temps
                .first()
                .map_or(Span::new(self.identity.definition.file_id, 0, 0), |temp| {
                    temp.span
                }),
        }
    }

    fn statement_origin(&self, block: BlockId, stmt: &AmirStmt) -> EvalLocation {
        let mut origin = self.origin();
        origin.block = block;
        let span = match stmt {
            AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } => self
                .function
                .temps
                .get(lhs.as_usize())
                .map(|temp| temp.span),
            AmirStmt::Store { lhs, .. } | AmirStmt::Destroy(lhs) => self
                .function
                .locals
                .get(lhs.local.as_usize())
                .map(|local| local.span),
            AmirStmt::StorageLive(id) | AmirStmt::StorageDead(id) => self
                .function
                .locals
                .get(id.as_usize())
                .map(|local| local.span),
            AmirStmt::Call { lhs: None, .. } | AmirStmt::Free(_) | AmirStmt::Nop => None,
        };
        if let Some(span) = span {
            origin.span = span;
        }
        origin
    }
}

pub trait FunctionProvider {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind>;

    fn instance(&self, key: &FunctionInstance) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        if key.arguments.is_empty() {
            self.function(key.definition)
        } else {
            Err(EvalErrorKind::UnavailableFunction(key.definition))
        }
    }

    fn intrinsic(&self, _symbol: SymbolId) -> Option<IntrinsicKind> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalErrorKind {
    Cancelled,
    FuelExhausted,
    FrameLimit,
    ValueLimit,
    AllocationFailed,
    MissingFunction(SymbolId),
    UnavailableFunction(SymbolId),
    UnsupportedType(TypeId),
    UnsupportedOperation,
    InvalidIr,
    Uninitialized,
    TypeMismatch,
    InvalidLiteral,
    TargetMismatch,
    Arithmetic(ScalarEvalError),
    Value(ConstValueError),
    InvalidLayout(DataLayoutError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalLocation {
    pub function: SymbolId,
    pub block: BlockId,
    pub span: Span,
}

/// Diagnostic traces are bounded independently of user-supplied frame limits.
const MAX_TRACE: usize = 32;

/// Internal failure with the current source location. Cancellation is not a
/// language error; a query boundary must unwind it instead of memoizing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    pub kind: EvalErrorKind,
    pub function: SymbolId,
    pub block: BlockId,
    pub span: Span,
    /// Call sites, outermost first; the failing operation is in `span`.
    pub trace: Vec<EvalLocation>,
    pub trace_truncated: bool,
}

impl EvalLocation {
    fn error(self, kind: EvalErrorKind) -> EvalError {
        EvalError {
            kind,
            function: self.function,
            block: self.block,
            span: self.span,
            trace: Vec::new(),
            trace_truncated: false,
        }
    }
}

struct Frame {
    unit: Arc<CtfeFunction>,
    temps: Vec<Option<ConstValue>>,
    locals: Vec<Option<ConstValue>>,
    block: BlockId,
    instruction: usize,
    destination: Option<TempId>,
    origin: EvalLocation,
}

struct Meter<'a, C> {
    fuel: u64,
    values: usize,
    max_values: usize,
    max_frames: usize,
    aggregate_nodes: usize,
    aggregate_bytes: usize,
    cancelled: &'a mut C,
}

// Logical storage units, deliberately independent of the Rust enum's host
// layout. The current frozen slot representation fits this conservative unit;
// this is a semantic quota, not a measurement of allocator/RSS overhead.
const FROZEN_SLOT_BYTES: usize = 64;

impl<C: FnMut() -> bool> Meter<'_, C> {
    fn step(&mut self) -> Result<(), EvalErrorKind> {
        if (self.cancelled)() {
            return Err(EvalErrorKind::Cancelled);
        }
        self.fuel = self
            .fuel
            .checked_sub(1)
            .ok_or(EvalErrorKind::FuelExhausted)?;
        Ok(())
    }
    fn reserve(&self, count: usize) -> Result<(), EvalErrorKind> {
        if count > self.max_values.saturating_sub(self.values) {
            return Err(EvalErrorKind::ValueLimit);
        }
        Ok(())
    }
    fn bytes(&mut self, count: usize) -> Result<(), EvalErrorKind> {
        self.aggregate_bytes = self
            .aggregate_bytes
            .checked_add(count)
            .ok_or(EvalErrorKind::ValueLimit)?;
        // Allocation accounting, not an RSS claim: frozen payload/backings and
        // child slots are cumulative across all frames. The factor leaves room
        // for shape metadata while retaining the existing public Budget API.
        let limit = self
            .max_values
            .checked_mul(FROZEN_SLOT_BYTES)
            .and_then(|limit| limit.checked_mul(2))
            .ok_or(EvalErrorKind::ValueLimit)?;
        if self.aggregate_bytes > limit {
            return Err(EvalErrorKind::ValueLimit);
        }
        Ok(())
    }
    fn aggregate(
        &mut self,
        shape: &arandu_middle::types::TypeShape,
        count: usize,
    ) -> Result<(), EvalErrorKind> {
        // Conservative cumulative allocation budget bounds churn as well as
        // live immutable trees. Reusing a frozen Arc does not charge again.
        self.aggregate_nodes = self
            .aggregate_nodes
            .checked_add(count.checked_add(1).ok_or(EvalErrorKind::ValueLimit)?)
            .ok_or(EvalErrorKind::ValueLimit)?;
        let shape =
            arandu_middle::ctfe::canonical_type_bytes(shape).map_err(EvalErrorKind::Value)?;
        for _ in &shape {
            self.step()?;
        }
        self.bytes(
            count
                .checked_mul(FROZEN_SLOT_BYTES)
                .and_then(|count| count.checked_add(shape.len()))
                .ok_or(EvalErrorKind::ValueLimit)?,
        )?;
        if self.aggregate_nodes > self.max_values {
            return Err(EvalErrorKind::ValueLimit);
        }
        Ok(())
    }
    fn frozen(&mut self, value: &ConstValue) -> Result<(), EvalErrorKind> {
        fn inspect<C: FnMut() -> bool>(
            value: &ConstValue,
            depth: usize,
            meter: &mut Meter<'_, C>,
        ) -> Result<(), EvalErrorKind> {
            if depth >= arandu_middle::types::TypeShape::MAX_DEPTH {
                return Err(EvalErrorKind::ValueLimit);
            }
            meter.step()?;
            if let ConstValue::Aggregate(value) = value {
                for child in value.values() {
                    inspect(child, depth + 1, meter)?;
                }
            }
            Ok(())
        }
        inspect(value, 0, self)
    }
    fn input(&mut self, value: &ConstValue) -> Result<(), EvalErrorKind> {
        match value {
            ConstValue::Aggregate(value) => {
                self.aggregate(value.shape(), value.values().len())?;
                self.frozen(&ConstValue::Aggregate(value.clone()))?;
                for child in value.values() {
                    self.input(child)?;
                }
            }
            ConstValue::String(value) => self.bytes(value.backing_len())?,
            ConstValue::Bytes(value) => self.bytes(value.backing_str().len())?,
            ConstValue::Void
            | ConstValue::Bool(_)
            | ConstValue::Integer(_)
            | ConstValue::Float(_) => {}
        }
        Ok(())
    }
}

fn slice_range(range: DenseRange) -> Result<std::ops::Range<usize>, EvalErrorKind> {
    let start = usize::try_from(range.start).map_err(|_| EvalErrorKind::InvalidIr)?;
    let len = usize::try_from(range.len).map_err(|_| EvalErrorKind::InvalidIr)?;
    Ok(start..start.checked_add(len).ok_or(EvalErrorKind::InvalidIr)?)
}

fn slots(count: usize) -> Result<Vec<Option<ConstValue>>, EvalErrorKind> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| EvalErrorKind::AllocationFailed)?;
    values.resize(count, None);
    Ok(values)
}

impl Frame {
    fn index_operand<C: FnMut() -> bool>(
        &self,
        operand: AmirOperand,
        meter: &mut Meter<'_, C>,
    ) -> Result<usize, EvalErrorKind> {
        let ConstValue::Integer(value) = self.operand(operand, None, meter)? else {
            return Err(EvalErrorKind::TypeMismatch);
        };
        usize::try_from(value.value()).map_err(|_| EvalErrorKind::InvalidIr)
    }

    fn place_path<C: FnMut() -> bool>(
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

    fn load_place<C: FnMut() -> bool>(
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

    fn store_place<C: FnMut() -> bool>(
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
    fn validate<C: FnMut() -> bool>(
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

    fn new<C: FnMut() -> bool>(
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

    fn operand_type(&self, operand: AmirOperand) -> Result<Option<ValueType>, EvalErrorKind> {
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

    fn operand<C: FnMut() -> bool>(
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

    fn assign(&mut self, id: TempId, value: ConstValue) -> Result<(), EvalErrorKind> {
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

    fn rvalue<C: FnMut() -> bool>(
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
                    ConstAggregate::new(descriptor.shape.clone(), results)
                        .map_err(EvalErrorKind::Value)?,
                ))
            }
            AmirRvalue::VectorCompare { op, left, right } => {
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
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => {
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
            AmirRvalue::VectorShuffle { left, right, mask } => {
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
            _ => Err(EvalErrorKind::UnsupportedOperation),
        }
    }

    fn jump<C: FnMut() -> bool>(
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

    fn error(&self, kind: EvalErrorKind) -> EvalError {
        self.origin.error(kind)
    }
}

enum Action {
    Continue,
    Call {
        unit: Arc<CtfeFunction>,
        values: Vec<ConstValue>,
        destination: Option<TempId>,
    },
    Return(ConstValue),
}

fn admitted_rvalue(rhs: &AmirRvalue) -> Result<(), EvalErrorKind> {
    match rhs {
        AmirRvalue::Use(_) => Ok(()),
        AmirRvalue::Unary {
            op: UnaryOp::Neg | UnaryOp::Not | UnaryOp::BitNot,
            ..
        } => Ok(()),
        AmirRvalue::Binary {
            op:
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Mod
                | BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::LtEqual
                | BinaryOp::GtEqual
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::ShiftLeft
                | BinaryOp::ShiftRight,
            ..
        } => Ok(()),
        AmirRvalue::Load(_)
        | AmirRvalue::FieldAccess { .. }
        | AmirRvalue::StructLiteral { .. }
        | AmirRvalue::IndexAccess { .. }
        | AmirRvalue::Array { .. }
        | AmirRvalue::Tuple { .. }
        | AmirRvalue::Len(_)
        | AmirRvalue::StrBytes { .. }
        | AmirRvalue::SliceSubslice { .. }
        | AmirRvalue::Discriminant { .. }
        | AmirRvalue::EnumPayload { .. }
        | AmirRvalue::EnumConstruct { .. }
        | AmirRvalue::VectorSelect { .. }
        | AmirRvalue::VectorCompare { .. }
        | AmirRvalue::VectorOp { .. }
        | AmirRvalue::VectorSplat { .. }
        | AmirRvalue::VectorShuffle { .. }
        | AmirRvalue::VectorExtractLane { .. }
        | AmirRvalue::VectorInsertLane { .. } => Ok(()),
        AmirRvalue::SliceData(_)
        | AmirRvalue::SliceView { .. }
        | AmirRvalue::StrView { .. }
        | AmirRvalue::Alloc(_)
        | AmirRvalue::Borrow(_)
        | AmirRvalue::BorrowMut(_)
        | AmirRvalue::RelativeBorrow { .. }
        | AmirRvalue::CoroutineReady { .. }
        | AmirRvalue::GenInsert { .. }
        | AmirRvalue::GenGet { .. }
        | AmirRvalue::GenSet { .. }
        | AmirRvalue::GenUpsert { .. }
        | AmirRvalue::GenRemove { .. }
        | AmirRvalue::StringInterp { .. }
        | AmirRvalue::ToStr { .. }
        | AmirRvalue::BlackBox { .. }
        | AmirRvalue::Binary { .. }
        | AmirRvalue::Unary { .. } => Err(EvalErrorKind::UnsupportedOperation),
    }
}

fn scalar_operand(operand: AmirOperand) -> bool {
    match operand {
        AmirOperand::Copy(_) | AmirOperand::Move(_) | AmirOperand::Constant(_) => true,
        AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => false,
    }
}

/// Execute scalar AMIR with one shared fuel/value budget for the entire call
/// tree. Cancellation is polled during validation, instructions and transfers.
pub fn evaluate<P: FunctionProvider, C: FnMut() -> bool>(
    provider: &P,
    symbol: SymbolId,
    args: &[ConstValue],
    budget: Budget,
    cancelled: C,
) -> Result<ConstValue, EvalError> {
    evaluate_instance(
        provider,
        &FunctionInstance {
            definition: symbol,
            arguments: Vec::new(),
        },
        args,
        budget,
        cancelled,
    )
}

/// Evaluate a source definition with structural arguments, never a synthetic
/// symbol from a different unit's namespace.
pub fn evaluate_instance<P: FunctionProvider, C: FnMut() -> bool>(
    provider: &P,
    key: &FunctionInstance,
    args: &[ConstValue],
    budget: Budget,
    mut cancelled: C,
) -> Result<ConstValue, EvalError> {
    let symbol = key.definition;
    let origin = EvalLocation {
        function: symbol,
        block: BlockId(0),
        span: Span::new(symbol.file_id, 0, 0),
    };
    if cancelled() {
        return Err(origin.error(EvalErrorKind::Cancelled));
    }
    if budget.fuel == 0 {
        return Err(origin.error(EvalErrorKind::FuelExhausted));
    }
    let root = provider.instance(key).map_err(|kind| origin.error(kind))?;
    if root.identity != *key {
        return Err(origin.error(EvalErrorKind::InvalidIr));
    }
    evaluate_root(provider, root, true, args, budget, cancelled)
}

/// Evaluate a supplied root (for an isolated typed expression) without
/// inventing a synthetic source ID or intercepting lookup of real callees.
/// The provider still owns the identity of every function called by the root.
pub fn evaluate_unit<P: FunctionProvider, C: FnMut() -> bool>(
    provider: &P,
    unit: Arc<CtfeFunction>,
    args: &[ConstValue],
    budget: Budget,
    cancelled: C,
) -> Result<ConstValue, EvalError> {
    evaluate_root(provider, unit, false, args, budget, cancelled)
}

fn evaluate_root<P: FunctionProvider, C: FnMut() -> bool>(
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
                            let (ty, _) = frame.place_path(lhs, &mut meter)?;
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
                                if args.len() != 1 {
                                    return Err(EvalErrorKind::InvalidIr);
                                }
                                meter.step()?;
                                let arg_val = frame.operand(args[0], None, &mut meter)?;
                                let ConstValue::Integer(input_int) = arg_val else {
                                    return Err(EvalErrorKind::TypeMismatch);
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

fn wrap_int(int_ty: IntegerType, val: i128) -> Result<ConstInt, EvalErrorKind> {
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

fn vector_to_bytes(
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

fn bytes_to_vector(
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
