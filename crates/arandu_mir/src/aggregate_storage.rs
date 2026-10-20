//! Shared lifetime admission for invocation-owned aggregate scratch storage.
//! Target backends supply their result ABI, not a second ownership checker.
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, TempId};
use arandu_middle::layout::{
    StructLayoutProvider, instantiated_enum_variant_payload_type, instantiated_field_type,
};
use arandu_middle::types::{ArType, TypeInterner};
use arandu_middle::types::{Primitive, TypeId};
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

fn block_in_cycle(function: &AmirFunc, start: arandu_middle::amir::BlockId) -> bool {
    let mut visited = vec![false; function.blocks.len()];
    let mut stack: Vec<arandu_middle::amir::BlockId> = function
        .cfg
        .successors
        .get(start.as_usize())
        .map_or_else(Vec::new, |succs| succs.clone());
    while let Some(current) = stack.pop() {
        if current == start {
            return true;
        }
        let Some(seen) = visited.get_mut(current.as_usize()) else {
            continue;
        };
        if *seen {
            continue;
        }
        *seen = true;
        if let Some(succs) = function.cfg.successors.get(current.as_usize()) {
            stack.extend_from_slice(succs);
        }
    }
    false
}

fn block_param_safe(ty: TypeId, interner: &TypeInterner, in_cycle: bool, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    match interner.resolve(ty) {
        ArType::Primitive(p) => p != Primitive::Str,
        ArType::Ptr(_)
        | ArType::Vector(_)
        | ArType::Void
        | ArType::GenRef
        | ArType::IntLiteral
        | ArType::FloatLiteral => true,
        ArType::Slice(inner) | ArType::MutSlice(inner) => {
            !in_cycle && depth == 0 && block_param_safe(inner, interner, in_cycle, depth + 1)
        }
        ArType::Array(_, inner) | ArType::Option(inner) | ArType::Poll(inner) => {
            !in_cycle && block_param_safe(inner, interner, in_cycle, depth + 1)
        }
        ArType::Result(ok, err) => {
            !in_cycle
                && block_param_safe(ok, interner, in_cycle, depth + 1)
                && block_param_safe(err, interner, in_cycle, depth + 1)
        }
        ArType::Tuple(args) | ArType::Named(_, args) => {
            !in_cycle
                && interner
                    .type_args(args)
                    .iter()
                    .all(|&inner| block_param_safe(inner, interner, in_cycle, depth + 1))
        }
        ArType::Nullable(_)
        | ArType::Ref(_)
        | ArType::RefMut(_)
        | ArType::Coroutine(_)
        | ArType::Range(_)
        | ArType::Func(..)
        | ArType::ConstArray(..)
        | ArType::FrozenConst(_)
        | ArType::Const(_)
        | ArType::ConstParam(_)
        | ArType::Err
        | ArType::Error => false,
    }
}

mod string_origin;
use string_origin::independent_string_result;

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use arandu_middle::Span;
    use arandu_middle::SymbolId;
    use arandu_middle::amir::{AmirBasicBlock, AmirStmtTable, AmirTemp};
    use arandu_typeck::TypeInfo;
    use smallvec::smallvec;

    fn temp(id: usize, ty: TypeId) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty,
            is_copy: true,
            is_nullable: false,
            span: Span::new(0, 10 + id as u32, 11 + id as u32),
        }
    }

    /// One-block function whose statements are built around an opaque
    /// (extern) callee, so admission rules can be exercised per call shape.
    fn shape(
        return_type: TypeId,
        temps: Vec<AmirTemp>,
        stmts: impl Fn(SymbolId) -> Vec<AmirStmt>,
    ) -> (AmirFunc, SymbolTable) {
        shape_named(return_type, "opaque", temps, stmts)
    }

    fn shape_named(
        return_type: TypeId,
        extern_name: &str,
        temps: Vec<AmirTemp>,
        stmts: impl Fn(SymbolId) -> Vec<AmirStmt>,
    ) -> (AmirFunc, SymbolTable) {
        shape_with_callee_kind(
            return_type,
            extern_name,
            SymbolKind::ExternFunc,
            temps,
            stmts,
        )
    }

    fn shape_with_callee_kind(
        return_type: TypeId,
        callee_name: &str,
        callee_kind: SymbolKind,
        temps: Vec<AmirTemp>,
        stmts: impl Fn(SymbolId) -> Vec<AmirStmt>,
    ) -> (AmirFunc, SymbolTable) {
        let mut symbols = SymbolTable::new(0);
        let scope = symbols.global_scope();
        let caller = symbols
            .define(scope, "caller", SymbolKind::Func, Span::new(0, 0, 0))
            .expect("define caller");
        let opaque = symbols
            .define(scope, callee_name, callee_kind, Span::new(0, 0, 0))
            .expect("define extern");
        let mut table = AmirStmtTable::new();
        let built = stmts(opaque);
        let count = built.len();
        for statement in built {
            table.push(statement);
        }
        let blocks = vec![AmirBasicBlock {
            id: arandu_middle::amir::BlockId::from_usize(0),
            statements: DenseRange::new(0, count),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let function = AmirFunc {
            symbol: caller,
            return_type,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps,
            blocks,
            block_params: Vec::new(),
            stmts: table,
            cfg,
        };
        (function, symbols)
    }

    fn safe(function: &AmirFunc, symbols: &SymbolTable, interner: &TypeInterner) -> bool {
        function_scratch_safe(
            function,
            symbols,
            interner,
            ScratchResultAbi::CallerOwnedAggregates,
        )
    }

    /// `Free(str)` releases a runtime buffer — never admitted backing, since
    /// `str` itself is rejected as a scratch type — and a value-passed extern
    /// boundary (`ptr` + scalars) cannot name this frame's storage. Neither
    /// may disqualify the function's aggregate scratch.
    #[test]
    fn string_frees_and_value_passed_externs_stay_admitted() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let pointer = interner.intern(ArType::Ptr(byte));
        let (function, symbols) = shape(
            int,
            vec![
                temp(0, string),
                temp(1, pointer),
                temp(2, int),
                temp(3, interner.intern(ArType::Primitive(Primitive::Bool))),
            ],
            |opaque| {
                vec![
                    AmirStmt::Free(AmirOperand::Copy(TempId::from_usize(0))),
                    AmirStmt::Call {
                        lhs: None,
                        callee: AmirOperand::FunctionRef(opaque),
                        args: smallvec![
                            AmirOperand::Copy(TempId::from_usize(1)),
                            AmirOperand::Copy(TempId::from_usize(2)),
                            AmirOperand::Copy(TempId::from_usize(3)),
                        ],
                        return_borrow: None,
                    },
                ]
            },
        );
        assert!(
            safe(&function, &symbols, &interner),
            "Free(str) and value-passed extern arguments must stay admitted"
        );
    }

    /// The flip side: anything that can carry the address of this frame's
    /// backing into opaque code — an aggregate argument, a safe borrow, or
    /// an aggregate result written through an sret pointer — keeps the
    /// function off scratch promotion.
    #[test]
    fn backing_handed_to_opaque_code_disables_scratch() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let array = interner.intern(ArType::Array(4, byte));
        let borrow = interner.intern(ArType::Ref(byte));

        let (function, symbols) = shape(int, vec![temp(0, array)], |opaque| {
            vec![AmirStmt::Call {
                lhs: None,
                callee: AmirOperand::FunctionRef(opaque),
                args: smallvec![AmirOperand::Copy(TempId::from_usize(0))],
                return_borrow: None,
            }]
        });
        assert!(
            !safe(&function, &symbols, &interner),
            "an aggregate argument hands opaque code our backing address"
        );

        let (function, symbols) = shape(int, vec![temp(0, borrow)], |opaque| {
            vec![AmirStmt::Call {
                lhs: None,
                callee: AmirOperand::FunctionRef(opaque),
                args: smallvec![AmirOperand::Copy(TempId::from_usize(0))],
                return_borrow: None,
            }]
        });
        assert!(
            !safe(&function, &symbols, &interner),
            "a safe borrow argument can be retained past slot reuse"
        );

        let (function, symbols) = shape(int, vec![temp(0, array)], |opaque| {
            vec![AmirStmt::Call {
                lhs: Some(TempId::from_usize(0)),
                callee: AmirOperand::FunctionRef(opaque),
                args: smallvec![],
                return_borrow: None,
            }]
        });
        assert!(
            !safe(&function, &symbols, &interner),
            "an aggregate result exposes the sret destination to the callee"
        );
    }

    /// The backend lowers intrinsics to fixed operations — `ptrWrite`
    /// becomes a store through the pointer, `ptrOffset` an add — so an
    /// aggregate at an intrinsic boundary never reaches opaque code. This is
    /// what keeps `vec.tryPush<T>` on scratch instead of a per-push malloc.
    #[test]
    fn intrinsic_boundaries_stay_admitted_with_aggregate_arguments() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let pointer = interner.intern(ArType::Ptr(byte));
        let array = interner.intern(ArType::Array(4, byte));
        let (function, mut symbols) = shape_named(
            int,
            "ptrWrite",
            vec![temp(0, pointer), temp(1, array)],
            |intrinsic| {
                vec![AmirStmt::Call {
                    lhs: None,
                    callee: AmirOperand::FunctionRef(intrinsic),
                    args: smallvec![
                        AmirOperand::Copy(TempId::from_usize(0)),
                        AmirOperand::Copy(TempId::from_usize(1)),
                    ],
                    return_borrow: None,
                }]
            },
        );
        symbols.set_lang_item(
            SymbolId::new(0, 1),
            arandu_middle::symbol_table::LangItem::CompilerIntrinsic,
        );
        assert!(
            safe(&function, &symbols, &interner),
            "an intrinsic boundary must not disqualify scratch"
        );
    }

    #[test]
    fn view_results_are_not_admitted_by_any_backing_abi() {
        let interner = TypeInterner::new();
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let mut function = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: byte,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: Vec::new(),
            block_params: Vec::new(),
            stmts: Default::default(),
            cfg: Default::default(),
        };
        let symbols = SymbolTable::new(0);
        for abi in [
            ScratchResultAbi::ScalarsOnly,
            ScratchResultAbi::CallerOwnedAggregates,
            ScratchResultAbi::TransferredAggregates,
        ] {
            assert!(function_scratch_safe(&function, &symbols, &interner, abi));
            for ty in [
                ArType::Primitive(Primitive::Str),
                ArType::Slice(byte),
                ArType::Ref(byte),
                ArType::RefMut(byte),
                ArType::Ptr(byte),
            ] {
                function.return_type = interner.intern(ty);
                assert!(!function_scratch_safe(&function, &symbols, &interner, abi));
            }
            function.return_type = byte;
        }
    }

    #[test]
    fn called_coroutines_with_borrowed_or_aggregate_payloads_disable_scratch() {
        let interner = TypeInterner::new();
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let array = interner.intern(ArType::Array(4, byte));
        let slice = interner.intern(ArType::Slice(byte));
        let reference = interner.intern(ArType::Ref(byte));
        for payload in [byte, array, string, slice, reference] {
            let coroutine = interner.intern(ArType::Coroutine(payload));
            let (function, symbols) = shape_with_callee_kind(
                coroutine,
                "async_helper",
                SymbolKind::Func,
                vec![temp(0, coroutine)],
                |callee| {
                    vec![AmirStmt::Call {
                        lhs: Some(TempId::from_usize(0)),
                        callee: AmirOperand::FunctionRef(callee),
                        args: smallvec![],
                        return_borrow: None,
                    }]
                },
            );
            for abi in [
                ScratchResultAbi::ScalarsOnly,
                ScratchResultAbi::CallerOwnedAggregates,
                ScratchResultAbi::TransferredAggregates,
            ] {
                assert_eq!(
                    function_scratch_safe(&function, &symbols, &interner, abi),
                    payload == byte && abi == ScratchResultAbi::CallerOwnedAggregates,
                    "only scalar caller-owned coroutine results qualify for scratch"
                );
            }
        }
    }

    #[test]
    fn nested_safe_views_are_excluded_but_raw_resource_handles_are_not() {
        let interner = TypeInterner::new();
        let info = TypeInfo::default();
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        for view in [
            ArType::Primitive(Primitive::Str),
            ArType::Slice(byte),
            ArType::Ref(byte),
            ArType::RefMut(byte),
            ArType::Coroutine(byte),
        ] {
            let inner = interner.intern(view);
            let array = interner.intern(ArType::Array(4, inner));
            let option = interner.intern(ArType::Option(array));
            assert!(!scratch_type_safe(option, &interner, &info));
        }
        let pointer = interner.intern(ArType::Ptr(byte));
        let array = interner.intern(ArType::Array(4, pointer));
        assert!(scratch_type_safe(array, &interner, &info));
    }

    #[test]
    fn string_results_require_every_definition_to_have_external_backing() {
        use arandu_middle::amir::{AmirConstant, LocalId};
        use arandu_middle::literal_pool::LiteralId;
        let interner = TypeInterner::new();
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let literal = AmirOperand::Constant(AmirConstant::Pool(LiteralId(0)));
        let (mut function, symbols) = shape(string, vec![temp(0, string), temp(1, string)], |_| {
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Use(literal),
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
                },
            ]
        });
        assert!(safe(&function, &symbols, &interner));
        function.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Load(arandu_middle::amir::AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec::SmallVec::new(),
            }),
        });
        // An additional definition with unknown provenance invalidates the
        // whole proof, even if another return path is a literal.
        assert!(!safe(&function, &symbols, &interner));
        let (mut forwarded, symbols) =
            shape(string, vec![temp(0, string), temp(1, string)], |_| {
                vec![AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
                }]
            });
        assert!(!safe(&forwarded, &symbols, &interner));
        forwarded.params.push(TempId::from_usize(1));
        assert!(safe(&forwarded, &symbols, &interner));
        forwarded.params.clear();
        forwarded.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(0))),
        });
        assert!(!safe(&forwarded, &symbols, &interner));
        let (called, symbols) = shape(string, vec![temp(0, string)], |callee| {
            vec![AmirStmt::Call {
                lhs: Some(TempId::from_usize(0)),
                callee: AmirOperand::FunctionRef(callee),
                args: smallvec::SmallVec::new(),
                return_borrow: None,
            }]
        });
        assert!(!safe(&called, &symbols, &interner));
    }

    #[test]
    fn string_backing_proof_has_a_fixed_depth_bound() {
        use arandu_middle::amir::AmirConstant;
        use arandu_middle::literal_pool::LiteralId;
        let interner = TypeInterner::new();
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        for (length, admitted) in [(64, true), (65, false)] {
            let (function, _) = shape(
                string,
                (0..length).map(|id| temp(id, string)).collect(),
                |_| {
                    (0..length)
                        .map(|id| AmirStmt::Assign {
                            lhs: TempId::from_usize(id),
                            rhs: AmirRvalue::Use(if id + 1 < length {
                                AmirOperand::Copy(TempId::from_usize(id + 1))
                            } else {
                                AmirOperand::Constant(AmirConstant::Pool(LiteralId(0)))
                            }),
                        })
                        .collect()
                },
            );
            assert_eq!(independent_string_result(&function, &interner), admitted);
        }
    }

    #[test]
    fn local_string_backing_rejects_mutable_aliases_and_unproved_stores() {
        use arandu_middle::amir::{AmirLocal, AmirPlace, LocalId};
        let interner = TypeInterner::new();
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let local = LocalId::from_usize(0);
        let place = AmirPlace {
            local,
            projections: smallvec::SmallVec::new(),
        };
        let (mut function, _symbols) = shape(
            string,
            vec![temp(0, string), temp(1, string), temp(2, string)],
            |_| {
                vec![
                    AmirStmt::Store {
                        lhs: place.clone(),
                        rhs: AmirOperand::Copy(TempId::from_usize(1)),
                    },
                    AmirStmt::Assign {
                        lhs: TempId::from_usize(0),
                        rhs: AmirRvalue::Load(place.clone()),
                    },
                ]
            },
        );
        function.params.push(TempId::from_usize(1));
        function.locals.push(AmirLocal {
            id: local,
            ty: string,
            is_memory: true,
            symbol: None,
            span: Span::new(0, 0, 0),
            use_span: None,
        });
        assert!(independent_string_result(&function, &interner));
        let mut aliased = function.clone();
        aliased.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::BorrowMut(place.clone()),
        });
        assert!(!independent_string_result(&aliased, &interner));
        let mut overwritten = function.clone();
        overwritten.stmts.push(AmirStmt::Store {
            lhs: place.clone(),
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        assert!(!independent_string_result(&overwritten, &interner));
        let mut destroyed = function.clone();
        destroyed.stmts.push(AmirStmt::Destroy(place));
        assert!(!independent_string_result(&destroyed, &interner));
        function.stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local,
                projections: smallvec::smallvec![arandu_middle::amir::AmirProjection::Index(
                    AmirOperand::Copy(TempId::from_usize(1))
                )],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(1)),
        });
        assert!(!independent_string_result(&function, &interner));
    }

    #[test]
    fn generic_enum_payloads_use_concrete_lifetime_shapes() {
        use arandu_typeck::EnumPayloadShape;
        use std::sync::Arc;

        let mut info = TypeInfo::default();
        let interner = &info.type_interner;
        let owner = SymbolId::new(0, 20);
        let parameter = SymbolId::new(0, 21);
        let unit = SymbolId::new(0, 22);
        let payload = SymbolId::new(0, 23);
        let generic = interner.intern(ArType::Named(parameter, crate::hir::IndexRange::empty()));
        info.generic_params.insert(owner, Arc::new(vec![parameter]));
        info.enum_variants
            .insert(unit, (owner, EnumPayloadShape::Unit));
        info.enum_variants
            .insert(payload, (owner, EnumPayloadShape::Tuple(vec![generic])));
        info.enum_variant_tags.insert(unit, 0);
        info.enum_variant_tags.insert(payload, 1);
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let closed = interner.intern(ArType::Array(4, int));
        for (argument, admitted) in [
            (closed, true),
            (interner.intern(ArType::Primitive(Primitive::Str)), false),
            (interner.intern(ArType::Ref(int)), false),
            (interner.intern(ArType::Slice(int)), false),
            (interner.intern(ArType::Coroutine(int)), false),
        ] {
            let enum_ty =
                interner.intern(ArType::Named(owner, interner.push_type_args(&[argument])));
            assert_eq!(scratch_type_safe(enum_ty, interner, &info), admitted);
        }
        let malformed = interner.intern(ArType::Named(
            owner,
            interner.push_type_args(&[closed, int]),
        ));
        assert!(!scratch_type_safe(malformed, interner, &info));
    }

    #[test]
    fn acyclic_aggregate_joins_and_cyclic_value_handles_stay_admitted() {
        use arandu_middle::amir::{AmirConstant, BlockId, BlockParam, LocalId};

        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let pointer = interner.intern(ArType::Ptr(byte));
        let gen_ref = interner.intern(ArType::GenRef);
        let array = interner.intern(ArType::Array(4, byte));
        let symbols = SymbolTable::new(0);

        let diamond_blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(TempId::from_usize(0))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::new(0, 1),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let diamond_cfg = compute_cfg_edges(&diamond_blocks);
        let diamond = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![temp(0, array), temp(1, array), temp(2, array)],
            blocks: diamond_blocks,
            block_params: vec![BlockParam {
                id: TempId::from_usize(2),
                local: LocalId::from_usize(0),
                ty: array,
                from: None,
                moved: false,
            }],
            stmts: Default::default(),
            cfg: diamond_cfg,
        };
        assert!(
            safe(&diamond, &symbols, &interner),
            "acyclic diamond join with aggregate block param must stay admitted"
        );

        let loop_blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(TempId::from_usize(0))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::new(0, 1),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(2),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(3),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let loop_cfg = compute_cfg_edges(&loop_blocks);
        for handle_ty in [pointer, gen_ref] {
            let handle_loop = AmirFunc {
                symbol: SymbolId::new(0, 0),
                return_type: int,
                receiver: None,
                params: Vec::new(),
                locals: Vec::new(),
                temps: vec![temp(0, handle_ty), temp(1, handle_ty)],
                blocks: loop_blocks.clone(),
                block_params: vec![BlockParam {
                    id: TempId::from_usize(1),
                    local: LocalId::from_usize(0),
                    ty: handle_ty,
                    from: None,
                    moved: false,
                }],
                stmts: Default::default(),
                cfg: loop_cfg.clone(),
            };
            assert!(
                safe(&handle_loop, &symbols, &interner),
                "cyclic value-passed handle block param must stay admitted"
            );
        }

        let cyclic_aggregate = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![temp(0, array), temp(1, array)],
            blocks: loop_blocks,
            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: array,
                from: None,
                moved: false,
            }],
            stmts: Default::default(),
            cfg: loop_cfg,
        };
        assert!(
            !safe(&cyclic_aggregate, &symbols, &interner),
            "cyclic aggregate block param must remain rejected"
        );
    }
}
