//! Bounded scalar AMIR execution. Calls use an explicit heap stack, never Rust
//! recursion. The provider owns function lookup; this module knows no database.

use super::{ScalarEvalError, eval_binary, eval_unary};
use arandu_middle::amir::visit::{for_each_rvalue_operand, for_each_terminator_operand};
use arandu_middle::amir::*;
use arandu_middle::ctfe::{ConstValue, ConstValueError, IntegerType};
use arandu_middle::intrinsics::IntrinsicKind;
use arandu_middle::layout::{DataLayout, DataLayoutError, DenseRange};
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{FunctionInstance, Primitive, TypeId, TypeInterner};
use arandu_middle::{Span, SymbolId};
use rustc_hash::FxHashMap;
use std::sync::Arc;

mod admission;
mod bytes;
mod eval;
mod frame;
mod rvalue;
mod types;
mod vector;

use admission::Admission;
use eval::evaluate_root;
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
