use arandu_middle::amir::AmirFunc;
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};

pub(super) fn block_in_cycle(function: &AmirFunc, start: arandu_middle::amir::BlockId) -> bool {
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

pub(super) fn block_param_safe(
    ty: TypeId,
    interner: &TypeInterner,
    in_cycle: bool,
    depth: usize,
) -> bool {
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
