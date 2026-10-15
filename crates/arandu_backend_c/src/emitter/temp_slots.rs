//! Global coloring for non-addressable scalar temporaries of identical types.
use arandu_middle::amir::{AmirFunc, AmirStmt, TempId};

pub(super) fn scalar_slots(func: &AmirFunc, eligible: impl Fn(TempId) -> bool) -> Vec<usize> {
    // Bound dense graph storage to 32 MiB, retaining a linear fallback.
    if func.temps.len() > 16_384 {
        return arandu_codegen::temp_slots::block_local_temp_slots(func, eligible);
    }
    let mut dedicated = vec![false; func.temps.len()];
    let mut definitions = vec![0usize; func.temps.len()];
    if let Some(ret) = dedicated.first_mut() {
        *ret = true;
    }
    for &param in func
        .params
        .iter()
        .chain(func.receiver.iter().map(|r| &r.temp))
    {
        dedicated[param.as_usize()] = true;
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            dedicated[param.id.as_usize()] = true;
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                definitions[lhs.as_usize()] += 1;
            }
        }
    }
    let graph = arandu_semantics::passes::liveness::temp_interference_graph(func);
    let mut representatives: Vec<_> = (0..func.temps.len()).collect();
    let mut slots: Vec<Vec<TempId>> = Vec::new();
    for temp in &func.temps {
        let id = temp.id.as_usize();
        if dedicated[id] || definitions[id] != 1 || !eligible(temp.id) {
            continue;
        }
        if let Some(slot) = slots.iter_mut().find(|slot| {
            func.temps[slot[0].as_usize()].ty == temp.ty
                && slot.iter().all(|other| !graph[id].contains(*other))
        }) {
            representatives[id] = slot[0].as_usize();
            slot.push(temp.id);
        } else {
            slots.push(vec![temp.id]);
        }
    }
    representatives
}
