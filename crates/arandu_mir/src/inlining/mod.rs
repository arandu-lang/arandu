//! Interprocedural leaf-function inlining pass on AMIR.

pub mod cost;
pub mod splice;

use crate::amir::{AmirOperand, AmirProgram, AmirStmt, BlockId, InstrId, TempId};
use cost::{
    INLINE_LEAF_BUDGET, evaluate_leaf_inlining, evaluate_nonleaf_inlining, nested_call_targets,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use splice::splice_calls;

/// Floor for the per-caller inlining budget, in inlined callee statements.
///
/// Kept so that tiny callers still get a useful number of sites inlined.
pub const MIN_INLINES_PER_CALLER: usize = 32;

/// Reconsider wrappers that become small leaves after their helpers inline.
/// The call-site budget remains cumulative across rounds, not per round.
const MAX_INLINING_ROUNDS: usize = 8;

struct InlinableCall {
    bid: BlockId,
    instr_id: InstrId,
    lhs: Option<TempId>,
    args: SmallVec<[AmirOperand; 4]>,
}

/// Maximum factor by which one caller may grow from inlining, on top of the
/// [`MIN_INLINES_PER_CALLER`] floor.
///
/// The budget is expressed in *callee statements* rather than call sites, and
/// scales with the caller. A fixed per-site cap starves exactly the large
/// functions that carry the hot loops inlining exists to accelerate — that is
/// what left SHA-256's compression body with 400 `callq` and no `ror`.
///
/// Measured against the *pristine* size so the budget cannot chase its own
/// tail: each round inlines, the caller grows, and a budget recomputed from the
/// current size would authorise unbounded growth.
const MAX_GROWTH_FACTOR: usize = 8;

/// Bound the expanded IR, including terminators and storage identities. A
/// statement-only ceiling misses wrappers with many blocks or unused temps.
/// This limits growth introduced by this pass; larger source functions remain
/// valid and are left intact. The existing relative budget still applies.
// The SHA-256 compression kernel already needs more than 16384 combined
// identities/operations after useful helper inlining. Keep headroom for that
// measured workload instead of reintroducing calls in its inner loop.
const MAX_CALLER_IR_SIZE: usize = 32 * 1024;

/// Takes `(cost, growth, site)` candidates in order while each fits both the
/// remaining statement budget and the caller size ceiling. Returns the chosen
/// sites and the budget they use.
fn choose_within_budget<T>(
    candidates: impl IntoIterator<Item = (usize, usize, T)>,
    budget_left: usize,
    mut size: usize,
) -> (Vec<T>, usize) {
    let mut used = 0usize;
    let mut chosen = Vec::new();
    for (cost, growth, site) in candidates {
        let cost = cost.max(1);
        let grown = size.saturating_add(growth);
        if cost > budget_left.saturating_sub(used) || grown > MAX_CALLER_IR_SIZE {
            continue;
        }
        used += cost;
        size = grown;
        chosen.push(site);
    }
    (chosen, used)
}

/// The caller's IR size as the growth ceiling counts it: statements, blocks,
/// temps and locals.
fn current_caller_size(caller: &crate::amir::AmirFunc) -> usize {
    caller
        .stmts
        .len()
        .saturating_add(caller.blocks.len())
        .saturating_add(caller.temps.len())
        .saturating_add(caller.locals.len())
}

/// What one splice adds: it removes the call, adds parameter assignments,
/// appends the callee blocks and a continuation, and maps the callee's return
/// temp.
fn inlining_growth(callee: &crate::amir::AmirFunc, has_lhs: bool) -> usize {
    callee
        .stmts
        .len()
        .saturating_add(callee.params.len())
        .saturating_add(callee.blocks.len())
        .saturating_add(1)
        .saturating_add(callee.temps.len().saturating_sub(1))
        .saturating_add(usize::from(!has_lhs))
        .saturating_add(callee.locals.len())
        .saturating_sub(1)
}

/// Optimizes `program` by inlining eligible small leaf functions into their callers.
///
/// Within a round each caller inlines the *cheapest* eligible callees first,
/// so a caller full of trivial constant getters inlines those before spending
/// budget on a multi-statement helper. All of a round's sites are spliced in
/// one rebuild; calls a splice exposes are considered in the next round.
///
/// Returns the total number of call sites inlined across the program.
pub fn inline_leaf_functions(program: &mut AmirProgram) -> usize {
    let mut total_inlined = 0usize;
    let mut spent_per_caller = vec![0usize; program.funcs.len()];
    // Pinned from the pristine program: every inline grows the caller, so a
    // budget recomputed from the current size would chase its own tail.
    let budgets: Vec<usize> = program
        .funcs
        .iter()
        .map(|f| {
            MIN_INLINES_PER_CALLER.saturating_add(MAX_GROWTH_FACTOR.saturating_mul(f.stmts.len()))
        })
        .collect();
    for _ in 0..MAX_INLINING_ROUNDS {
        // 1. Identify all eligible callees and map SymbolId -> (index, cost).
        //
        // Leaves are tried first at a lower cost, so a wrapper that both could
        // be inlined and contains inlinable helpers is only reached once the
        // cheaper option is unavailable.
        let mut eligible = FxHashMap::default();
        for (idx, func) in program.funcs.iter().enumerate() {
            if let Some(cost) = evaluate_leaf_inlining(func, INLINE_LEAF_BUDGET) {
                eligible.insert(func.symbol, (idx, cost));
            }
        }
        // A non-leaf callee is only worth inlining when its own calls can be
        // removed too. Splicing `beU32Buf` — which still calls `bufGet` — into
        // sixteen sites would replicate its four inner calls and leave 64 calls
        // where there were 16. Only accept a callee whose every nested target is
        // itself inlinable; recompute to a fixpoint so a wrapper around a
        // wrapper is admitted once the inner one qualifies.
        let mut changed = true;
        while changed {
            changed = false;
            for (idx, func) in program.funcs.iter().enumerate() {
                if eligible.contains_key(&func.symbol) {
                    continue;
                }
                let Some(cost) = evaluate_nonleaf_inlining(func, INLINE_LEAF_BUDGET) else {
                    continue;
                };
                let all_nested_inlinable = nested_call_targets(func)
                    .iter()
                    .all(|sym| eligible.contains_key(sym));
                if all_nested_inlinable {
                    eligible.insert(func.symbol, (idx, cost));
                    changed = true;
                }
            }
        }

        if eligible.is_empty() {
            break;
        }

        let before_round = total_inlined;

        // 2. For each function in program, inline eligible calls cheapest-first
        for (caller_idx, spent_slot) in spent_per_caller.iter_mut().enumerate() {
            let caller_sym = program.funcs[caller_idx].symbol;
            let budget = budgets[caller_idx];
            let mut spent = *spent_slot;
            if spent >= budget {
                continue;
            }

            // One scan per round: every eligible call site of this caller.
            let caller = &program.funcs[caller_idx];
            let mut candidates: Vec<(usize, usize, InlinableCall)> = Vec::new();
            for (bid_usize, block) in caller.blocks.iter().enumerate() {
                let bid = BlockId::from_usize(bid_usize);
                for instr_id in block.statements.iter_ids::<InstrId>() {
                    if let Some(AmirStmt::Call {
                        lhs,
                        callee: AmirOperand::FunctionRef(callee_sym),
                        args,
                        ..
                    }) = caller.try_stmt(instr_id)
                        && *callee_sym != caller_sym
                        && let Some(&(callee_idx, cost)) = eligible.get(callee_sym)
                    {
                        candidates.push((
                            cost,
                            callee_idx,
                            InlinableCall {
                                bid,
                                instr_id,
                                lhs: *lhs,
                                args: args.clone(),
                            },
                        ));
                    }
                }
            }
            // Cheapest callees first, as one splice at a time would choose
            // them; source order breaks ties deterministically.
            candidates.sort_by_key(|(cost, _, call)| (*cost, call.bid, call.instr_id));

            let sized = candidates.into_iter().map(|(cost, callee_idx, call)| {
                let growth = inlining_growth(&program.funcs[callee_idx], call.lhs.is_some());
                (cost, growth, (callee_idx, call))
            });
            let (chosen, used) = choose_within_budget(
                sized,
                budget.saturating_sub(spent),
                current_caller_size(caller),
            );
            spent += used;
            if chosen.is_empty() {
                *spent_slot = spent;
                continue;
            }

            // Clone each distinct callee once so the caller can be rebuilt.
            let mut templates: FxHashMap<usize, crate::amir::AmirFunc> = FxHashMap::default();
            for (callee_idx, _) in &chosen {
                templates
                    .entry(*callee_idx)
                    .or_insert_with(|| program.funcs[*callee_idx].clone());
            }
            let sites: Vec<splice::CallSite<'_>> = chosen
                .iter()
                .map(|(callee_idx, call)| splice::CallSite {
                    bid: call.bid,
                    instr_id: call.instr_id,
                    lhs: call.lhs,
                    args: &call.args,
                    callee: &templates[callee_idx],
                })
                .collect();
            total_inlined += splice_calls(&mut program.funcs[caller_idx], &sites);
            *spent_slot = spent;
        }
        if total_inlined == before_round {
            break;
        }
    }

    total_inlined
}

#[cfg(test)]
mod tests;
