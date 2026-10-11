//! Interprocedural leaf-function inlining pass on AMIR.

pub mod cost;
pub mod splice;

use crate::SymbolId;
use crate::amir::{AmirOperand, AmirProgram, AmirStmt, BlockId, InstrId, TempId};
use cost::{
    INLINE_LEAF_BUDGET, evaluate_leaf_inlining, evaluate_nonleaf_inlining, nested_call_targets,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use splice::splice_call;

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
    callee_sym: SymbolId,
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

fn expanded_caller_size(
    caller: &crate::amir::AmirFunc,
    callee: &crate::amir::AmirFunc,
    has_lhs: bool,
) -> usize {
    // splice_call removes one call, adds parameter assignments, appends the
    // callee blocks and a continuation, and maps the callee's return temp.
    caller
        .stmts
        .len()
        .saturating_sub(1)
        .saturating_add(callee.stmts.len())
        .saturating_add(callee.params.len())
        .saturating_add(caller.blocks.len())
        .saturating_add(callee.blocks.len())
        .saturating_add(1)
        .saturating_add(caller.temps.len())
        .saturating_add(callee.temps.len().saturating_sub(1))
        .saturating_add(usize::from(!has_lhs))
        .saturating_add(caller.locals.len())
        .saturating_add(callee.locals.len())
}

/// Optimizes `program` by inlining eligible small leaf functions into their callers.
///
/// Within a round each caller inlines the *cheapest* eligible callee available
/// first, so a caller full of trivial constant getters inlines those before
/// spending budget on a multi-statement helper.
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

            loop {
                if spent >= budget {
                    break;
                }

                // Find the eligible call site with the cheapest callee.
                // The caller is re-scanned after every splice because splicing
                // rebuilds the dense statement table and invalidates InstrIds.
                let caller = &program.funcs[caller_idx];
                let mut target_call: Option<InlinableCall> = None;
                let mut best_cost = usize::MAX;

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
                            && cost.max(1) <= budget.saturating_sub(spent)
                            && expanded_caller_size(
                                caller,
                                &program.funcs[callee_idx],
                                lhs.is_some(),
                            ) <= MAX_CALLER_IR_SIZE
                            && cost < best_cost
                        {
                            best_cost = cost;
                            target_call = Some(InlinableCall {
                                bid,
                                instr_id,
                                lhs: *lhs,
                                args: args.clone(),
                                callee_sym: *callee_sym,
                            });
                        }
                    }
                }

                let Some(call) = target_call else {
                    break;
                };

                let Some(&(callee_idx, cost)) = eligible.get(&call.callee_sym) else {
                    break;
                };
                // Clone the callee template so caller can be mutated in place
                let callee = program.funcs[callee_idx].clone();

                let caller_mut = &mut program.funcs[caller_idx];
                if splice_call(
                    caller_mut,
                    call.bid,
                    call.instr_id,
                    call.lhs,
                    &call.args,
                    &callee,
                ) {
                    spent += cost.max(1);
                    total_inlined += 1;
                } else {
                    // Splicing failed; don't loop endlessly on the same call
                    break;
                }
            }
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
