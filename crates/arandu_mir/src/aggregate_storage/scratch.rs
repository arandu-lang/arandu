use super::cycles::{block_in_cycle, block_param_safe};
use super::string_origin::independent_string_result;
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, TempId};
use arandu_middle::layout::{
    StructLayoutProvider, instantiated_enum_variant_payload_type, instantiated_field_type,
};
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
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
    fn visit(
        ty: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        match interner.resolve(ty) {
            ArType::Primitive(p) => p != Primitive::Str,
            ArType::Void | ArType::Ptr(_) | ArType::Vector(_) => true,
            ArType::Array(_, inner) | ArType::Option(inner) | ArType::Poll(inner) => {
                visit(inner, interner, provider, depth + 1)
            }
            ArType::Result(ok, err) => {
                visit(ok, interner, provider, depth + 1)
                    && visit(err, interner, provider, depth + 1)
            }
            ArType::Tuple(args) => interner
                .type_args(args)
                .iter()
                .all(|&inner| visit(inner, interner, provider, depth + 1)),
            owner @ ArType::Named(_, _) => {
                if let Some(fields) = provider.get_struct_fields_for_type(&owner, interner) {
                    fields.iter().all(|field| {
                        instantiated_field_type(&owner, field.name.as_str(), interner, provider)
                            .is_some_and(|inner| visit(inner, interner, provider, depth + 1))
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
                                    .is_some_and(|inner| {
                                        visit(inner, interner, provider, depth + 1)
                                    })
                            })
                        })
                }
            }
            ArType::Slice(_)
            | ArType::MutSlice(_)
            | ArType::Nullable(_)
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
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Err
            | ArType::Error => false,
        }
    }
    visit(ty, interner, provider, 0)
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

#[must_use]
pub fn function_scratch_safe(
    function: &AmirFunc,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    result_abi: ScratchResultAbi,
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
