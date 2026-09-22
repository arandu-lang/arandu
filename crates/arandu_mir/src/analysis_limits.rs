//! Canonical iteration and convergence limits for AMIR dataflow analysis.
//!
//! In monotonic dataflow frameworks (e.g. liveness, definite initialization,
//! move tracking), the theoretical upper bound for fixed-point convergence is
//! proportional to `num_blocks * num_variables`.
//!
//! The headroom constants provide defensive limits against non-terminating loops
//! in the presence of malformed IR, without using arbitrary inline magic numbers.

/// Headroom added to theoretical fixed-point iteration bounds (`O(num_blocks * num_vars)`)
/// to account for CFG join passes, SSA phi reconciliations, and initial sweeps.
pub const DATAFLOW_FIXPOINT_HEADROOM: usize = 1_000;

/// Defensive iteration bound for inter-block dataflow fact solvers (e.g. borrow facts).
/// Monotone domains on finite graphs converge in far fewer steps; reaching this limit
/// signals an IR invariant violation.
pub const BORROW_FACTS_ITERATION_GUARD: usize = 100_000;

/// Maximum fixed-point passes for local SSA temp origin resolution from loads.
pub const TEMP_ORIGIN_SOLVER_MAX_PASSES: usize = 100;

/// Maximum outer iterations for the CFG simplification fixpoint loop
/// (jump threading + block merging + unreachable removal).
/// Each pass can trigger further simplifications, but convergence is
/// guaranteed in far fewer than 64 passes for any realistic function.
/// Reaching this limit signals an IR invariant violation.
pub const CFG_SIMPLIFY_MAX_OUTER_ITERS: u32 = 64;
