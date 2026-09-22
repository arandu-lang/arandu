//! F2.1 + F2.2 — May-borrow facts refined by reference live ranges.

pub mod paths;
pub mod propagation;
pub mod types;

pub(crate) use paths::*;
pub(crate) use propagation::*;
pub use types::*;

use crate::amir::{AmirFunc, BlockId, LocalId};
use crate::liveness::{analyze_local_liveness, analyze_temp_liveness};

/// F2.2-aware borrow facts: block IN/OUT = loans whose holders are live there.
#[must_use]
pub fn analyze_borrow_facts(func: &AmirFunc) -> FuncBorrowFacts {
    let num_locals = func.locals.len();
    let num_blocks = func.blocks.len();

    if num_blocks == 0 {
        return FuncBorrowFacts {
            block_in: vec![],
            block_out: vec![],
            borrow_site_counts: vec![],
            loans: vec![],
            temp_live: analyze_temp_liveness(func),
            local_live: analyze_local_liveness(func),
            local_holders_at: vec![],
        };
    }

    let (loans, borrow_site_counts) = collect_loans(func);
    let temp_live = analyze_temp_liveness(func);
    let local_live = analyze_local_liveness(func);
    let local_holders_at = analyze_local_holder_states(func, &loans);

    let mut block_in = Vec::with_capacity(num_blocks);
    let mut block_out = Vec::with_capacity(num_blocks);
    for bi in 0..num_blocks {
        let bid = BlockId::from_usize(bi);
        block_in.push(state_from_live_holders(
            &loans,
            num_locals,
            temp_live.live_in(bid),
            local_live.live_in(bid),
        ));
        block_out.push(state_from_live_holders(
            &loans,
            num_locals,
            temp_live.live_out(bid),
            local_live.live_out(bid),
        ));
    }

    FuncBorrowFacts {
        block_in,
        block_out,
        borrow_site_counts,
        loans,
        temp_live,
        local_live,
        local_holders_at,
    }
}

/// Free function for M2 / Salsa consumers (same as [`FuncBorrowFacts::is_borrowed_at`]).
#[must_use]
pub fn is_borrowed_at(facts: &FuncBorrowFacts, local: LocalId, point: ProgramPoint) -> bool {
    facts.is_borrowed_at(local, point)
}

/// Compact per-block borrow summary for memoization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockBorrowSummary {
    pub shared_in: u32,
    pub exclusive_in: u32,
    pub borrow_sites: u32,
    /// Locals still may-borrowed at block **exit** (F2.2: after live-range kill).
    pub shared_out: u32,
    pub exclusive_out: u32,
}

/// Summaries for all blocks in one pure call.
#[must_use]
pub fn block_borrow_summaries(func: &AmirFunc) -> Vec<BlockBorrowSummary> {
    let facts = analyze_borrow_facts(func);
    facts
        .block_in
        .iter()
        .zip(facts.block_out.iter())
        .zip(facts.borrow_site_counts.iter())
        .map(|((inn, out), &sites)| BlockBorrowSummary {
            shared_in: inn.shared.len() as u32,
            exclusive_in: inn.exclusive.len() as u32,
            borrow_sites: sites,
            shared_out: out.shared.len() as u32,
            exclusive_out: out.exclusive.len() as u32,
        })
        .collect()
}

#[cfg(test)]
#[path = "../borrow_facts_tests.rs"]
mod tests;
