use arandu_middle::amir::{AmirFunc, BlockId, Dominators};
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

/// Returns `true` if `target` can reach itself along a control-flow path that
/// stays strictly inside `target`'s dominance frontier (never passing through any
/// strict dominator of `target`).
///
/// This distinguishes cyclic merges (both natural loop headers and irreducible
/// multi-entry loops, whose block parameters may hold values across loop
/// iterations) from acyclic diamond joins nested inside an outer loop (whose
/// paths back around the outer loop must exit through a strict dominator of the
/// join, where the join's block parameters are no longer in scope). Fails closed
/// (`true`) for out-of-range or unreachable blocks.
pub(super) fn block_is_cyclic_merge(
    function: &AmirFunc,
    doms: &Dominators,
    target: BlockId,
) -> bool {
    let n = function.blocks.len();
    let target_idx = target.as_usize();
    if target_idx >= n || doms.immediate_dominator(target).is_none() {
        return true;
    }

    let mut strict_doms = vec![false; n];
    let mut cur = target;
    let mut steps = 0usize;
    while let Some(dom) = doms.immediate_dominator(cur) {
        if dom == cur {
            break;
        }
        steps += 1;
        if steps > n {
            return true;
        }
        let dom_idx = dom.as_usize();
        if dom_idx >= n {
            return true;
        }
        strict_doms[dom_idx] = true;
        cur = dom;
    }

    let mut visited = vec![false; n];
    let mut stack: Vec<BlockId> = function
        .cfg
        .successors
        .get(target_idx)
        .map_or_else(Vec::new, |succs| succs.clone());
    while let Some(current) = stack.pop() {
        if current == target {
            return true;
        }
        let idx = current.as_usize();
        if idx >= n || strict_doms[idx] || visited[idx] {
            continue;
        }
        visited[idx] = true;
        if let Some(succs) = function.cfg.successors.get(idx) {
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
        | ArType::Error
        | ArType::Infer(_) => false,
    }
}

/// Whether a loop reassigns an aggregate local that another statement also
/// assigns, so the local carries a value across the back-edge.
///
/// Backends whose aggregate moves copy the backing pointer make that local
/// name the storage of the producing site. A loop-carried local outlives the
/// next entry of that site, so per-site backing reclaimed or overwritten on
/// re-entry would be observed (use after free) through it. A local with a
/// single store inside the loop is a per-iteration binding: elaborated AMIR
/// moves or drops it before the site runs again.
#[must_use]
pub fn cyclic_aggregate_local_store(function: &AmirFunc, interner: &TypeInterner) -> bool {
    // (stores, stored inside a cycle) per aggregate local.
    let mut stores = vec![(0usize, false); function.locals.len()];
    for block in &function.blocks {
        let mut in_cycle = None;
        for statement in function.block_stmts(block.id) {
            let arandu_middle::amir::AmirStmt::Store { lhs, .. } = statement else {
                continue;
            };
            if !lhs.projections.is_empty() {
                continue;
            }
            let Some(local) = function.locals.get(lhs.local.as_usize()) else {
                continue;
            };
            if !matches!(
                interner.resolve(local.ty),
                ArType::Named(..)
                    | ArType::Tuple(_)
                    | ArType::Array(..)
                    | ArType::Option(_)
                    | ArType::Result(..)
                    | ArType::Poll(_)
            ) {
                continue;
            }
            let cyclic = *in_cycle.get_or_insert_with(|| block_in_cycle(function, block.id));
            if let Some(entry) = stores.get_mut(lhs.local.as_usize()) {
                entry.0 = entry.0.saturating_add(1);
                entry.1 |= cyclic;
            }
        }
    }
    stores.iter().any(|&(count, cyclic)| cyclic && count > 1)
}
