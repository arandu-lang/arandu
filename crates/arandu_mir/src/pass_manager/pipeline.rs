//! Standard pipelines per [`OptLevel`].
//!
//! New passes join these lists at named positions. Keep O0 empty (LLVM-style:
//! only semantically required work belongs at O0 — here, nothing).

use super::{FunctionPass, OptLevel, PassResources};
use crate::amir::AmirFunc;
use crate::{
    Diagnostic, dce::mark_sweep_dce, gvn::gvn, licm::licm, sccp::sccp, simplify_cfg::simplify_cfg,
    sroa::sroa,
};

/// Sparse conditional constant propagation + branch folding.
struct SccpPass;

impl FunctionPass for SccpPass {
    fn name(&self) -> &'static str {
        "sccp"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(sccp(
            func,
            resources.literal_pool,
            resources.bump,
            resources.interner,
        ))
    }
}

/// Scalar Replacement of Aggregates — limited to aggregate field forwarding.
///
/// Propagates scalar operands from `StructLiteral`/`Tuple` assignments into
/// subsequent `FieldAccess` projections. Does **not** decompose stack slots,
/// model escapes, or handle stores through projections.
struct SroaPass;

impl FunctionPass for SroaPass {
    fn name(&self) -> &'static str {
        "sroa"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(sroa(func))
    }
}

/// Global Value Numbering (GVN).
struct GvnPass;

impl FunctionPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(gvn(func))
    }
}

/// Mark-sweep dead code elimination (keeps side effects and jump arguments).
struct MarkSweepDcePass;

impl FunctionPass for MarkSweepDcePass {
    fn name(&self) -> &'static str {
        "mark_sweep_dce"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        mark_sweep_dce(func)
    }
}

/// Loop-Invariant Code Motion.
///
/// Hoists pure, dominating computations out of natural loops so the register
/// allocator can keep them in registers across iterations.
struct LicmPass;

impl FunctionPass for LicmPass {
    fn name(&self) -> &'static str {
        "licm"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(licm(func))
    }
}

/// CFG simplification: block merging, unreachable removal, jump threading.
struct SimplifyCfgPass;

struct TailMergePass;
impl FunctionPass for TailMergePass {
    fn name(&self) -> &'static str {
        "tail_merge"
    }
    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(crate::tail_merge::merge_return_tails(func))
    }
}

impl FunctionPass for SimplifyCfgPass {
    fn name(&self) -> &'static str {
        "simplify_cfg"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        simplify_cfg(func, resources.bump)
    }
}

/// Builds the standard pass sequence for `level`.
///
/// O1 runs baseline simplification (SCCP → DCE → SimplifyCFG).
/// O2 adds SROA, GVN and LICM before the O1 core.
///
/// LICM runs after SCCP so folded constants are visible to the invariance
/// analysis, and before DCE so hoisted copies that turn out dead are collected
/// in the same round.
///
/// The checked O2 entrypoint also scalarizes nonescaping Copy local fields and
/// eliminates scalar self-tail recursion before this type-independent fixpoint.
pub fn pipeline_for_level(level: OptLevel) -> Vec<Box<dyn FunctionPass>> {
    match level {
        OptLevel::O0 => Vec::new(),
        OptLevel::O1 => vec![
            Box::new(SccpPass),
            Box::new(MarkSweepDcePass),
            Box::new(SimplifyCfgPass),
        ],
        OptLevel::Os => vec![
            Box::new(SccpPass),
            Box::new(MarkSweepDcePass),
            Box::new(TailMergePass),
            Box::new(SimplifyCfgPass),
        ],
        OptLevel::O2 => vec![
            Box::new(SroaPass),
            Box::new(GvnPass),
            Box::new(SccpPass),
            Box::new(LicmPass),
            Box::new(MarkSweepDcePass),
            Box::new(TailMergePass),
            Box::new(SimplifyCfgPass),
        ],
    }
}
