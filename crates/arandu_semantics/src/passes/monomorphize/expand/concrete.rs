use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::{ArType, TypeId};
use arandu_typeck::TypeCheckResult;

pub(super) fn is_identity_instantiation(
    tc: &TypeCheckResult,
    symbol: SymbolId,
    type_args: &[TypeId],
) -> bool {
    let Some(params) = tc.type_info.generic_params.get(&symbol) else {
        return false;
    };
    if params.len() != type_args.len() {
        return false;
    }
    let interner = &tc.type_info.type_interner;
    params.iter().zip(type_args.iter()).all(|(&param, &tid)| {
        matches!(
            interner.resolve(tid),
            ArType::Named(id, ref args) if id == param && args.is_empty()
        ) || matches!(interner.resolve(tid), ArType::ConstParam(id) if id == param)
    })
}

/// True if `tid` is still a free type-parameter (not a concrete type).
fn type_arg_still_param(tc: &TypeCheckResult, tid: TypeId) -> bool {
    match tc.type_info.type_interner.resolve(tid) {
        ArType::Named(id, ref args) if args.is_empty() => tc
            .type_info
            .generic_params
            .values()
            .any(|params| params.contains(&id)),
        ArType::ConstParam(id) => tc
            .type_info
            .generic_params
            .values()
            .any(|params| params.contains(&id)),
        _ => false,
    }
}

/// Independent units must not publish free parameters or unresolved numeric
/// pseudo-types as concrete keys. Inferred literals are defaulted before this
/// check; the legacy pure producer retains its existing admission policy.
pub(super) fn instance_args_fully_concrete(tc: &TypeCheckResult, type_args: &[TypeId]) -> bool {
    use arandu_middle::types::TypeShape;
    fn unresolved(tc: &TypeCheckResult, shape: &TypeShape) -> bool {
        match shape {
            TypeShape::Named(id, args) => {
                tc.symbols.try_get(*id).is_none()
                    || tc
                        .type_info
                        .generic_params
                        .values()
                        .any(|params| params.contains(id))
                    || args.iter().any(|arg| unresolved(tc, arg))
            }
            TypeShape::ConstArray(_, _)
            | TypeShape::ConstParam(_)
            | TypeShape::IntLiteral
            | TypeShape::FloatLiteral
            | TypeShape::Error => true,
            TypeShape::Func(args, ret) => {
                args.iter().any(|arg| unresolved(tc, arg)) || unresolved(tc, ret)
            }
            TypeShape::Tuple(args) => args.iter().any(|arg| unresolved(tc, arg)),
            TypeShape::Nullable(inner)
            | TypeShape::Slice(inner)
            | TypeShape::MutSlice(inner)
            | TypeShape::Array(_, inner)
            | TypeShape::Ptr(inner)
            | TypeShape::Ref(inner)
            | TypeShape::RefMut(inner)
            | TypeShape::Option(inner)
            | TypeShape::Coroutine(inner)
            | TypeShape::Poll(inner)
            | TypeShape::Range(inner) => unresolved(tc, inner),
            TypeShape::Result(ok, error) => unresolved(tc, ok) || unresolved(tc, error),
            TypeShape::Primitive(_)
            | TypeShape::Vector(_)
            | TypeShape::FrozenConst(_)
            | TypeShape::Const(_)
            | TypeShape::GenRef
            | TypeShape::Err
            | TypeShape::Void => false,
        }
    }
    type_args.iter().all(|&tid| {
        arandu_middle::types::TypeShape::from_id(tid, &tc.type_info.type_interner)
            .is_ok_and(|shape| !unresolved(tc, &shape))
    })
}

pub(super) fn type_args_fully_concrete(tc: &TypeCheckResult, type_args: &[TypeId]) -> bool {
    type_args.iter().all(|&tid| !type_arg_still_param(tc, tid))
}
