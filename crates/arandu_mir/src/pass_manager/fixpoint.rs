//! Bounded fixpoint driver shared by every [`super::PassManager`] pipeline.
//!
//! Runs the full pass sequence repeatedly until no pass reports a change, or
//! fails with the `ICE-O-001` guardrail instead of looping forever.

use super::{FunctionPass, PassResources, PassStats};
use crate::amir::AmirFunc;
use crate::literal_pool::AmirLiteralPool;
use crate::{DiagCode, Diagnostic, Span};

/// Hard cap on full-pipeline iterations per function (guardrail; mirrors the
/// previous inline limit in `optimize.rs`).
pub(super) const DEFAULT_MAX_FIXPOINT_ITERATIONS: usize = 100;

pub(super) fn run_pipeline_to_fixpoint<F>(
    passes: &[Box<dyn FunctionPass>],
    func: &mut AmirFunc,
    literal_pool: &mut AmirLiteralPool,
    interner: Option<&crate::types::TypeInterner>,
    max_iterations: usize,
    validate: &mut F,
) -> Result<PassStats, Diagnostic>
where
    F: FnMut(&AmirFunc, &'static str) -> Result<(), Diagnostic>,
{
    let mut stats = PassStats::default();
    // One scratch arena per function; reset between mutating rounds so
    // SCCP/SimplifyCFG allocations stay O(function size) overall.
    let mut bump = bumpalo::Bump::new();
    for iteration in 1..=max_iterations {
        stats.iterations = iteration;
        let mut changed = false;
        {
            let mut resources = PassResources {
                interner,
                literal_pool: &mut *literal_pool,
                bump: &mut bump,
            };
            for pass in passes {
                if pass.run(func, &mut resources)? {
                    stats.note_change(pass.name());
                    changed = true;
                }
                validate(func, pass.name())?;
            }
        }
        if !changed {
            return Ok(stats);
        }
        bump.reset();
    }
    Err(non_convergence_ice(func, max_iterations))
}

fn non_convergence_ice(func: &AmirFunc, max_iterations: usize) -> Diagnostic {
    let span = func
        .temps
        .first()
        .map(|temp| temp.span)
        .or_else(|| func.locals.first().map(|local| local.span))
        .unwrap_or_else(|| Span::new(func.symbol.file_id, 0, 0));
    Diagnostic::ice(
        DiagCode::ICEO001,
        format!("AMIR optimization did not converge after {max_iterations} iterations"),
        span,
    )
}
