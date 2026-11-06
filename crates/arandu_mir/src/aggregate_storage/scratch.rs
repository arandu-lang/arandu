use super::cycles::{block_in_cycle, block_param_safe};
use super::string_origin::independent_string_result;
use arandu_middle::amir::{
    AmirFunc, AmirOperand, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator, TempId,
};
use arandu_middle::layout::{
    StructLayoutProvider, instantiated_enum_variant_payload_type, instantiated_field_type,
};
use arandu_middle::types::{ArType, Primitive, ReturnBorrowSummary, TypeId, TypeInterner};
use arandu_middle::{SymbolKind, SymbolTable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchResultAbi {
    ScalarsOnly,
    CallerOwnedAggregates,
    /// The backend transfers an owned result into distinct backing before
    /// reclaiming invocation storage; affine payload ownership follows the
    /// already elaborated AMIR moves and drops, not a second destructor.
    TransferredAggregates,
}

/// Admit backing independently of Copy, but never a safe view or a suspended
/// state hidden inside an aggregate. Raw pointers are ownership handles (or
/// unsafe pointers), not safe borrows; their pointees are not this backing.
#[must_use]
pub fn scratch_type_safe(
    ty: TypeId,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> bool {
    backing_type_safe(ty, interner, provider, false)
}

/// [`scratch_type_safe`] admitting safe views (`str`, slices, references) as
/// leaves: an aggregate that carries them. Where its backing lives decides
/// nothing about what its views point to. Their referents are governed by
/// typeck's borrow rules and by [`function_scratch_safe`], which keeps any
/// function that could retain a view of this frame's backing (extern calls,
/// back-edge phis of borrows, borrow-returning results) out of promotion; the
/// carrier's own backing has the lifetime of any other admitted aggregate.
#[must_use]
pub fn scratch_carrier_safe(
    ty: TypeId,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> bool {
    backing_type_safe(ty, interner, provider, true)
}

fn backing_type_safe(
    ty: TypeId,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    views: bool,
) -> bool {
    fn visit(
        ty: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
        depth: usize,
        views: bool,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        let visit = |inner, depth| visit(inner, interner, provider, depth, views);
        match interner.resolve(ty) {
            ArType::Primitive(Primitive::Str) => views,
            ArType::Slice(_) | ArType::MutSlice(_) | ArType::Ref(_) | ArType::RefMut(_) => views,
            ArType::Primitive(_) => true,
            ArType::Void | ArType::Ptr(_) | ArType::Vector(_) => true,
            ArType::Array(_, inner) | ArType::Option(inner) | ArType::Poll(inner) => {
                visit(inner, depth + 1)
            }
            ArType::Result(ok, err) => visit(ok, depth + 1) && visit(err, depth + 1),
            ArType::Tuple(args) => interner
                .type_args(args)
                .iter()
                .all(|&inner| visit(inner, depth + 1)),
            owner @ ArType::Named(_, _) => {
                if let Some(fields) = provider.get_struct_fields_for_type(&owner, interner) {
                    fields.iter().all(|field| {
                        instantiated_field_type(&owner, field.name.as_str(), interner, provider)
                            .is_some_and(|inner| visit(inner, depth + 1))
                    })
                } else {
                    provider
                        .get_enum_variants_for_type(&owner, interner)
                        .is_some_and(|variants| {
                            variants.iter().enumerate().all(|(tag, variant)| {
                                variant.payload_ty.is_none()
                                    || instantiated_enum_variant_payload_type(
                                        &owner, tag, interner, provider,
                                    )
                                    .is_some_and(|inner| visit(inner, depth + 1))
                            })
                        })
                }
            }
            ArType::Nullable(_)
            | ArType::Coroutine(_)
            | ArType::Range(_)
            | ArType::Func(..)
            | ArType::GenRef
            | ArType::ConstArray(..)
            | ArType::FrozenConst(_)
            | ArType::Const(_)
            | ArType::ConstParam(_)
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Err
            | ArType::Error => false,
        }
    }
    visit(ty, interner, provider, 0, views)
}

/// Whether an opaque (extern) callee could observe the address of backing
/// this function promotes.
///
/// A static frame slot is reused on the next call/iteration, so a retained
/// pointer would alias fresh contents. Scalars, raw ownership handles, and
/// static/value operands travel as values and cannot name this frame;
/// by-address aggregates, safe borrows, and slices (whose `data` can alias a
/// promoted array literal) can. A slice would be a fat `{data, len}` pair, but
/// admitting it would let a retained `data` point into reused backing, so it
/// stays with the aggregates.
fn extern_exposes_backing(
    function: &AmirFunc,
    lhs: Option<TempId>,
    args: &[AmirOperand],
    returns_borrow: bool,
    interner: &TypeInterner,
) -> bool {
    fn by_address(ty: TypeId, interner: &TypeInterner) -> bool {
        !matches!(
            interner.resolve(ty),
            ArType::Primitive(_)
                | ArType::Vector(_)
                | ArType::Ptr(_)
                | ArType::Void
                | ArType::IntLiteral
                | ArType::FloatLiteral
        )
    }
    fn temp_exposes(function: &AmirFunc, interner: &TypeInterner, temp: TempId) -> bool {
        function
            .temps
            .get(temp.as_usize())
            .is_none_or(|temp| by_address(temp.ty, interner))
    }
    returns_borrow
        || lhs.is_some_and(|temp| temp_exposes(function, interner, temp))
        || args.iter().any(|arg| match arg {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                temp_exposes(function, interner, *temp)
            }
            // Function values can carry an environment; the rest name static
            // or literal data, never this frame's storage.
            AmirOperand::FunctionRef(_) => true,
            AmirOperand::Constant(_) | AmirOperand::GlobalRef(_) => false,
        })
}

/// Whether the reference `function` returns borrows only what its parameters'
/// views reach, as its proven `summary` states: then no reference into this
/// invocation's backing can be returned, and a reference result is a value
/// like a scalar for promotion. A source path names a borrow inside the
/// formal, whose value points outside it; a root path borrows the formal
/// itself, which qualifies only for a view formal, since a by-value one may
/// live in that backing.
fn returns_parameter_views(
    function: &AmirFunc,
    interner: &TypeInterner,
    summary: Option<&ReturnBorrowSummary>,
) -> bool {
    let Some(summary) = summary else {
        return false;
    };
    !summary.dependencies.is_empty()
        && summary.dependencies.iter().all(|dependency| {
            !dependency.sources.is_empty()
                && dependency.sources.iter().all(|source| {
                    !source.parameter_path.0.is_empty()
                        || usize::try_from(source.parameter_index)
                            .ok()
                            .and_then(|index| function.params.get(index))
                            .and_then(|param| function.temps.get(param.as_usize()))
                            .is_some_and(|param| {
                                matches!(
                                    interner.resolve(param.ty),
                                    ArType::Ref(_)
                                        | ArType::RefMut(_)
                                        | ArType::Slice(_)
                                        | ArType::MutSlice(_)
                                        | ArType::Primitive(Primitive::Str)
                                )
                            })
                })
        })
}

/// Whether `ty` can hold no address: no raw pointer, reference, slice or
/// `str` anywhere inside it.
fn pointer_free(ty: TypeId, interner: &TypeInterner, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    match interner.resolve(ty) {
        ArType::Primitive(p) => p != Primitive::Str,
        ArType::Void | ArType::Vector(_) | ArType::IntLiteral | ArType::FloatLiteral => true,
        ArType::Array(_, inner) | ArType::Option(inner) => pointer_free(inner, interner, depth + 1),
        ArType::Result(ok, err) => {
            pointer_free(ok, interner, depth + 1) && pointer_free(err, interner, depth + 1)
        }
        ArType::Tuple(args) => interner
            .type_args(args)
            .iter()
            .all(|&inner| pointer_free(inner, interner, depth + 1)),
        // Nominal fields are not visible here; a named result may hold one.
        _ => false,
    }
}

/// Whether `function` never makes an address of storage it owns: every borrow
/// goes through a pointer it was given or allocated, and no call that is
/// handed one of its by-address values returns something that may hold an
/// address (which could be that value's). Then no pointer into this
/// invocation's backing exists to return or to store, and a pointer or
/// reference result is a value like a scalar for promotion.
fn makes_no_own_address(function: &AmirFunc, interner: &TypeInterner) -> bool {
    let by_address = |operand: &AmirOperand| match operand {
        AmirOperand::Copy(temp) | AmirOperand::Move(temp) => function
            .temps
            .get(temp.as_usize())
            .is_none_or(|temp| aggregate(temp.ty, interner)),
        AmirOperand::FunctionRef(_) => true,
        AmirOperand::Constant(_) | AmirOperand::GlobalRef(_) => false,
    };
    function.blocks.iter().all(|block| {
        function
            .block_stmts(block.id)
            .all(|statement| match statement {
                AmirStmt::Assign {
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                    ..
                } => matches!(place.projections.first(), Some(AmirProjection::Deref)),
                AmirStmt::Call { lhs, args, .. } => {
                    lhs.is_none_or(|temp| {
                        function
                            .temps
                            .get(temp.as_usize())
                            .is_some_and(|temp| pointer_free(temp.ty, interner, 0))
                    }) || !args.iter().any(by_address)
                }
                _ => true,
            })
    })
}

/// Aggregates live in backing this invocation may own.
fn aggregate(ty: TypeId, interner: &TypeInterner) -> bool {
    !matches!(
        interner.resolve(ty),
        ArType::Primitive(_)
            | ArType::Vector(_)
            | ArType::Ptr(_)
            | ArType::Ref(_)
            | ArType::RefMut(_)
            | ArType::Void
            | ArType::IntLiteral
            | ArType::FloatLiteral
    )
}

/// `returned_borrows` is the function's proven return borrow summary, which
/// admits a reference result (see `returns_parameter_views`); a pointer or
/// reference result is also admitted when the function makes no address of
/// its own storage (see `makes_no_own_address`).
#[must_use]
pub fn function_scratch_safe(
    function: &AmirFunc,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    result_abi: ScratchResultAbi,
    returned_borrows: Option<&ReturnBorrowSummary>,
) -> bool {
    // Back-edge aggregate phi values may retain a previous iteration's
    // materialization. Without slot liveness/coloring, a cyclic merge must not
    // overwrite or reclaim that backing while the phi still references it;
    // value-passed handles and acyclic joins cannot re-enter a predecessor site.
    let mut covered_block_params = 0usize;
    for block in &function.blocks {
        let params = function.block_params(block.params);
        covered_block_params = covered_block_params.saturating_add(params.len());
        if !params.is_empty() {
            let in_cycle = block_in_cycle(function, block.id);
            if params
                .iter()
                .any(|parameter| !block_param_safe(parameter.ty, interner, in_cycle, 0))
            {
                return false;
            }
        }
    }
    if covered_block_params != function.block_params.len()
        && function
            .block_params
            .iter()
            .any(|parameter| !block_param_safe(parameter.ty, interner, true, 0))
    {
        return false;
    }
    // A called async function can construct the state on behalf of this
    // function; detecting only local CoroutineReady instructions is too weak.
    let scalar_ready_result = match interner.resolve(function.return_type) {
        ArType::Coroutine(inner) if result_abi == ScratchResultAbi::CallerOwnedAggregates => {
            matches!(interner.resolve(inner), ArType::Void)
                || matches!(interner.resolve(inner), ArType::Primitive(p) if p != Primitive::Str)
        }
        _ => false,
    };
    if function.temps.iter().any(|temp| {
        matches!(interner.resolve(temp.ty), ArType::Coroutine(_))
            && (!scalar_ready_result || temp.ty != function.return_type)
    }) {
        return false;
    }
    // Pointer-returning carriers still use their legacy backing-pointer ABI.
    // Scalars, strings proved independent of this invocation, and aggregates
    // with caller-owned/direct returns qualify.
    match interner.resolve(function.return_type) {
        ArType::Primitive(Primitive::Str) if !independent_string_result(function, interner) => {
            return false;
        }
        ArType::Primitive(_)
        | ArType::Vector(_)
        | ArType::Void
        | ArType::IntLiteral
        | ArType::FloatLiteral => {}
        ArType::Coroutine(_) if scalar_ready_result => {}
        ArType::Ref(_) | ArType::RefMut(_)
            if returns_parameter_views(function, interner, returned_borrows)
                || makes_no_own_address(function, interner) => {}
        ArType::Ptr(_) if makes_no_own_address(function, interner) => {}
        ArType::Named(..)
        | ArType::Array(..)
        | ArType::Tuple(..)
        | ArType::Option(_)
        | ArType::Result(..)
        | ArType::Poll(_) => {
            if result_abi == ScratchResultAbi::ScalarsOnly {
                return false;
            }
        }
        ArType::Slice(_)
        | ArType::MutSlice(_)
        | ArType::Nullable(_)
        | ArType::Ptr(_)
        | ArType::Ref(_)
        | ArType::RefMut(_)
        | ArType::Coroutine(_)
        | ArType::Range(_)
        | ArType::Func(..)
        | ArType::GenRef
        | ArType::ConstArray(..)
        | ArType::FrozenConst(_)
        | ArType::Const(_)
        | ArType::ConstParam(_)
        | ArType::Err
        | ArType::Error => return false,
    }
    for block in &function.blocks {
        if matches!(block.terminator, AmirTerminator::Suspend { .. }) {
            return false;
        }
        for statement in function.block_stmts(block.id) {
            let safe = match statement {
                AmirStmt::Assign { rhs, .. } => match rhs {
                    AmirRvalue::CoroutineReady { payload_ty, .. } => {
                        scalar_ready_result
                            && (matches!(interner.resolve(*payload_ty), ArType::Void)
                                || matches!(interner.resolve(*payload_ty), ArType::Primitive(p) if p != Primitive::Str))
                    }
                    AmirRvalue::RelativeBorrow { .. }
                    | AmirRvalue::GenInsert { .. }
                    | AmirRvalue::GenGet { .. }
                    | AmirRvalue::GenSet { .. }
                    | AmirRvalue::GenUpsert { .. }
                    | AmirRvalue::GenRemove { .. } => false,
                    AmirRvalue::VectorSelect { .. }
                    | AmirRvalue::VectorCompare { .. }
                    | AmirRvalue::VectorOp { .. }
                    | AmirRvalue::VectorSplat { .. }
                    | AmirRvalue::VectorShuffle { .. }
                    | AmirRvalue::VectorExtractLane { .. }
                    | AmirRvalue::VectorInsertLane { .. }
                    | AmirRvalue::Use(_)
                    | AmirRvalue::Binary { .. }
                    | AmirRvalue::Unary { .. }
                    | AmirRvalue::FieldAccess { .. }
                    | AmirRvalue::IndexAccess { .. }
                    | AmirRvalue::StructLiteral { .. }
                    | AmirRvalue::Array { .. }
                    | AmirRvalue::Tuple { .. }
                    | AmirRvalue::Discriminant { .. }
                    | AmirRvalue::EnumPayload { .. }
                    | AmirRvalue::EnumConstruct { .. }
                    | AmirRvalue::Len(_)
                    | AmirRvalue::SliceData(_)
                    | AmirRvalue::SliceView { .. }
                    | AmirRvalue::SliceSubslice { .. }
                    | AmirRvalue::StrBytes { .. }
                    | AmirRvalue::StrView { .. }
                    | AmirRvalue::Alloc(_)
                    | AmirRvalue::Load(_)
                    | AmirRvalue::Borrow(_)
                    | AmirRvalue::BorrowMut(_)
                    | AmirRvalue::ToStr { .. }
                    | AmirRvalue::BlackBox { .. }
                    | AmirRvalue::StringInterp { .. } => true,
                },
                AmirStmt::Call {
                    lhs,
                    callee,
                    args,
                    return_borrow,
                    ..
                } => match callee {
                    // Intrinsics are compiler-defined operations that
                    // translate to loads/stores/arithmetic or to runtime
                    // helpers with fixed semantics: they cannot retain
                    // caller storage. Other externs may retain what they are
                    // handed, so only value-passed boundary types stay
                    // admitted.
                    AmirOperand::FunctionRef(symbol) => match symbols.try_get(*symbol) {
                        Some(definition) if definition.kind == SymbolKind::ExternFunc => {
                            arandu_middle::intrinsics::IntrinsicKind::from_declared_name(
                                &definition.name,
                                definition.lang_item,
                            )
                            .is_some()
                                || !extern_exposes_backing(
                                    function,
                                    *lhs,
                                    args,
                                    return_borrow.is_some(),
                                    interner,
                                )
                        }
                        Some(_) => true,
                        None => false,
                    },
                    AmirOperand::Copy(_)
                    | AmirOperand::Move(_)
                    | AmirOperand::Constant(_)
                    | AmirOperand::GlobalRef(_) => false,
                },
                // Raw allocation pointers and string buffers are
                // pointees/resources, not an aggregate's hidden backing cell.
                // `str` is never admitted backing, so `Free(str)` cannot
                // release this frame's slot. Never admit Free(aggregate).
                AmirStmt::Free(operand) => match operand {
                    AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                        function.temps.get(temp.as_usize()).is_some_and(|temp| {
                            matches!(
                                interner.resolve(temp.ty),
                                ArType::Ptr(_) | ArType::Primitive(Primitive::Str)
                            )
                        })
                    }
                    _ => false,
                },
                AmirStmt::Store { .. }
                | AmirStmt::Destroy(_)
                | AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Nop => true,
            };
            if !safe {
                return false;
            }
        }
    }
    true
}
