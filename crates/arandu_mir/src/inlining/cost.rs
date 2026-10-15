//! Eligibility and cost evaluation for AMIR leaf function inlining.

use crate::amir::{AmirFunc, AmirStmt, AmirTerminator, BlockId};

/// Default maximum statement/operation budget for inlining a leaf function.
pub const INLINE_LEAF_BUDGET: usize = 32;

/// Maximum number of basic blocks allowed in an inlinable leaf function.
/// Short-circuit predicates and small branch-only classification helpers can
/// lower to several blocks even though their instruction cost remains small.
/// The independent instruction budget still prevents code-size growth from
/// complex leaves.
pub const MAX_LEAF_BLOCKS: usize = 12;

/// Maximum number of basic blocks allowed in an inlinable *non-leaf* callee.
///
/// Looser than [`MAX_LEAF_BLOCKS`] because a function that calls a helper often
/// needs extra blocks to sequence the call and its result, but still bounded so a
/// large branchy body is never pasted into a hot caller.
pub const MAX_NONLEAF_BLOCKS: usize = 24;

/// Maximum number of call sites a non-leaf callee may contain.
///
/// Every surviving call keeps the caller paying a call, so a wrapper around many
/// helpers is not a good inline candidate even when its own cost is small.
pub const MAX_NONLEAF_CALLS: usize = 4;

/// Cost charged for a call left in the spliced body.
///
/// A nested call is not free, so it must weigh on the budget rather than
/// disqualify the callee outright.
const NONLEAF_CALL_PENALTY: usize = 2;

/// Evaluates whether `func` is a leaf function eligible for inlining into callers,
/// and returns its estimated cost if eligible.
#[must_use]
pub fn evaluate_leaf_inlining(func: &AmirFunc, budget: usize) -> Option<usize> {
    evaluate_inlining(func, budget, false)
}

/// Evaluates whether `func` may be inlined into its callers, returning its cost.
///
/// Unlike [`evaluate_leaf_inlining`] this accepts a callee that itself contains
/// calls, as long as it stays small. That is what lets a thin wrapper around a
/// helper be inlined instead of costing the caller a call.
///
/// **The caller is responsible for the transitive guard.** Splicing a callee that
/// still contains calls is only a win when those calls can themselves be removed;
/// otherwise the wrapper's `N` calls are replicated into every call site and the
/// program ends up with more calls than before. Use
/// [`nested_call_targets`] and require every target to
/// be inlinable too.
///
/// Cycles in the callee's own CFG still disqualify it: splicing a loop body into
/// a caller that may already be inside a loop needs loop-aware placement, which
/// this pass does not implement.
#[must_use]
pub fn evaluate_nonleaf_inlining(func: &AmirFunc, budget: usize) -> Option<usize> {
    evaluate_inlining(func, budget, true)
}

/// Symbols called from `func`'s body, in first-appearance order.
#[must_use]
pub fn nested_call_targets(func: &AmirFunc) -> Vec<crate::SymbolId> {
    let mut out = Vec::new();
    for block in &func.blocks {
        for stmt_id in block.statements.iter_ids::<crate::amir::InstrId>() {
            if let Some(AmirStmt::Call {
                callee: crate::amir::AmirOperand::FunctionRef(sym),
                ..
            }) = func.stmts.get(stmt_id)
                && !out.contains(sym)
            {
                out.push(*sym);
            }
        }
    }
    out
}

fn evaluate_inlining(func: &AmirFunc, budget: usize, allow_calls: bool) -> Option<usize> {
    let max_blocks = if allow_calls {
        MAX_NONLEAF_BLOCKS
    } else {
        MAX_LEAF_BLOCKS
    };
    if func.blocks.is_empty() || func.blocks.len() > max_blocks {
        return None;
    }

    // Check for loops via DFS cycle detection (three-color marking)
    if has_cycles(func) {
        return None;
    }

    let mut total_cost = 0usize;
    let mut call_count = 0usize;

    for block in &func.blocks {
        // Suspend terminators (coroutine frontiers) are not inlinable in Phase 1
        if matches!(block.terminator, AmirTerminator::Suspend { .. }) {
            return None;
        }

        // Cost of terminator
        match &block.terminator {
            AmirTerminator::Return => {}
            AmirTerminator::Goto { .. } => total_cost += 1,
            AmirTerminator::Branch { .. } => total_cost += 2,
            AmirTerminator::SwitchInt { targets, .. } => total_cost += 2 + targets.len(),
            AmirTerminator::Unreachable => {}
            AmirTerminator::Suspend { .. } => return None,
        }

        // Inspect statements
        for instr_id in block.statements.iter_ids::<crate::amir::InstrId>() {
            let stmt = func.stmts.get(instr_id)?;
            match stmt {
                AmirStmt::Call { .. } => {
                    if !allow_calls {
                        return None;
                    }
                    call_count += 1;
                    total_cost += NONLEAF_CALL_PENALTY;
                }
                AmirStmt::Assign { .. } => total_cost += 1,
                AmirStmt::Store { .. } => total_cost += 1,
                AmirStmt::Free(_) | AmirStmt::Destroy(_) => total_cost += 1,
                AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
            }
        }

        if total_cost > budget {
            return None;
        }
    }

    if call_count > MAX_NONLEAF_CALLS {
        return None;
    }

    Some(total_cost)
}

/// Detects if `func` contains any cycles (loops) in its control flow graph.
fn has_cycles(func: &AmirFunc) -> bool {
    let n = func.blocks.len();
    if n == 0 {
        return false;
    }

    // 0 = unvisited, 1 = on current DFS stack (gray), 2 = finished (black)
    let mut state = vec![0u8; n];

    fn dfs(u: usize, func: &AmirFunc, state: &mut [u8]) -> bool {
        state[u] = 1;
        let bid = BlockId::from_usize(u);
        for &succ in func.successors(bid) {
            let v = succ.as_usize();
            if v >= state.len() {
                continue;
            }
            if state[v] == 1 {
                return true; // Back-edge detected
            }
            if state[v] == 0 && dfs(v, func, state) {
                return true;
            }
        }
        state[u] = 2;
        false
    }

    for i in 0..n {
        if state[i] == 0 && dfs(i, func, &mut state) {
            return true;
        }
    }

    false
}
