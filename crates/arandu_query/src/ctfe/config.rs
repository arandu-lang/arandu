//! Driver-supplied deterministic limits for public compile-time evaluation.
//! Low-level CTFE requests retain their explicitly supplied budgets.

use arandu_mir::ctfe::Budget;
use salsa::Accumulator;

/// A validated per-obligation-family CTFE envelope. This does not limit RSS
/// or native query nesting, whose separate conservative ceiling stays fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CtfeLimits(Budget);

impl Default for CtfeLimits {
    fn default() -> Self {
        Self(super::PUBLIC_BUDGET)
    }
}

impl CtfeLimits {
    /// All ceilings must be positive. Drivers reject invalid configuration
    /// before changing analysis inputs rather than producing source errors.
    pub fn new(fuel: u64, frames: u32, values: u64) -> Result<Self, &'static str> {
        if fuel == 0 || frames == 0 || values == 0 {
            return Err("CTFE fuel, frames and values must be positive integers");
        }
        Ok(Self(Budget {
            fuel,
            frames,
            values,
        }))
    }

    #[must_use]
    pub fn budget(self) -> Budget {
        self.0
    }
}

#[salsa::input]
pub struct CtfeConfig {
    pub limits: CtfeLimits,
}

pub(crate) fn public_budget(db: &dyn crate::ArandCompilerDb) -> Budget {
    if let Some(config) = db.ctfe_config() {
        return config.limits(db).budget();
    }
    arandu_middle::db::DiagnosticsAccumulator(arandu_middle::Diagnostic::ice(
        arandu_middle::DiagCode::ICET001,
        "public CTFE policy input is missing from the analysis database",
        arandu_middle::Span::new(0, 0, 0),
    ))
    .accumulate(db);
    // Fail closed: a missing driver input must not silently permit evaluation.
    Budget {
        fuel: 0,
        frames: 0,
        values: 0,
    }
}
