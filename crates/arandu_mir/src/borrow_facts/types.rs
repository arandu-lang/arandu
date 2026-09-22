//! Fundamental data structures for borrow facts.

use crate::BitSet;
use crate::amir::{BlockId, LocalId, TempId};
use crate::liveness::{LocalLiveness, TempLiveness};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoanKind {
    Shared,
    Exclusive,
}

/// Stable structural location of a borrow inside a carrier value.
///
/// This is deliberately independent from source types: AMIR transfers can
/// preserve it without consulting the interner, which keeps borrow facts pure
/// and usable by Salsa. The empty path denotes the complete value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct HolderPath(pub Vec<HolderProjection>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HolderProjection {
    Slot(u32),
    NamedField { file_id: u32, local_id: u32 },
    Element,
    Deref,
    Variant(u32),
    Payload(u32),
    OptionSome,
    ResultOk,
    ResultErr,
    NullableValue,
    CoroutinePayload,
    PollReady,
    RangeElement,
}

/// One loan opened by `Borrow` / `BorrowMut` (plus propagated holders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loan {
    pub kind: LoanKind,
    /// Root local of the borrowed place (`x` in `ref x` / `ref x.f`).
    pub place_local: LocalId,
    /// Projection path retained for field-sensitive conflict checks. Index and
    /// dereference projections remain conservative because they may alias.
    pub place_projections: smallvec::SmallVec<[crate::amir::AmirProjection; 2]>,
    /// SSA temps that currently hold this reference value.
    pub holder_temps: BitSet<TempId>,
    /// Stack locals that currently hold this reference value (`let p = &x`).
    pub holder_locals: BitSet<LocalId>,
    /// Paths held by each SSA temp. Empty sets mean that temp is not a holder.
    pub holder_temp_paths: Vec<BTreeSet<HolderPath>>,
    /// Paths held by each stack local.
    pub holder_local_paths: Vec<BTreeSet<HolderPath>>,
    /// Relative coroutine borrows survive state relocation; absolute borrows do not.
    pub relative: bool,
    pub origin_block: BlockId,
}

/// Program point inside a function (block + statement index).
///
/// `stmt_index == 0` is block entry (before the first statement).
/// `stmt_index == n` (after last stmt) is just before the terminator / block exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramPoint {
    pub block: BlockId,
    pub stmt_index: usize,
}

/// May-borrowed state for all locals at one program point.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BorrowState {
    pub shared: BitSet<LocalId>,
    pub exclusive: BitSet<LocalId>,
}

impl BorrowState {
    #[must_use]
    pub fn new(num_locals: usize) -> Self {
        Self {
            shared: BitSet::with_capacity(num_locals),
            exclusive: BitSet::with_capacity(num_locals),
        }
    }

    #[must_use]
    pub fn maybe_shared(&self, local: LocalId) -> bool {
        self.shared.contains(local)
    }

    #[must_use]
    pub fn maybe_exclusive(&self, local: LocalId) -> bool {
        self.exclusive.contains(local)
    }

    #[must_use]
    pub fn maybe_borrowed(&self, local: LocalId) -> bool {
        self.maybe_shared(local) || self.maybe_exclusive(local)
    }

    pub(crate) fn activate(&mut self, loan: &Loan) {
        match loan.kind {
            LoanKind::Shared => {
                self.shared.insert(loan.place_local);
            }
            LoanKind::Exclusive => {
                self.exclusive.insert(loan.place_local);
            }
        }
    }
}

/// Full-function borrow facts (F2.1 summaries + F2.2 loans/liveness).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncBorrowFacts {
    pub block_in: Vec<BorrowState>,
    pub block_out: Vec<BorrowState>,
    pub borrow_site_counts: Vec<u32>,
    /// All loans with propagated holders (for M2 / [`FuncBorrowFacts::is_borrowed_at`]).
    pub loans: Vec<Loan>,
    pub temp_live: TempLiveness,
    pub local_live: LocalLiveness,
    /// Exact structural local holders before each statement and terminator.
    /// Indexed `[block][statement_point][loan]`.
    pub local_holders_at: Vec<Vec<Vec<BitSet<LocalId>>>>,
}

impl FuncBorrowFacts {
    #[must_use]
    pub fn maybe_shared_at_entry(&self, block: BlockId, local: LocalId) -> bool {
        self.block_in
            .get(block.as_usize())
            .is_some_and(|s| s.maybe_shared(local))
    }

    /// F2.2: is `local` under any loan whose reference holder is live at `point`?
    ///
    /// Statement-level precision walks the block from entry, tracking which
    /// temps/locals are still live (start from live-out, walk reverse once
    /// offline would be ideal; here we use entry/exit bits + “defined after
    /// point” approximation for temps defined in-block).
    #[must_use]
    pub fn is_borrowed_at(&self, local: LocalId, point: ProgramPoint) -> bool {
        self.is_borrowed_kind_at(local, point, None)
    }

    fn is_borrowed_kind_at(
        &self,
        local: LocalId,
        point: ProgramPoint,
        only: Option<LoanKind>,
    ) -> bool {
        let bi = point.block.as_usize();
        if bi >= self.block_in.len() {
            return false;
        }
        // Fast path: empty at both IN and OUT ⇒ no loan of this local in window.
        let in_b = &self.block_in[bi];
        let out_b = &self.block_out[bi];
        let relevant = |s: &BorrowState| match only {
            Some(LoanKind::Shared) => s.maybe_shared(local),
            Some(LoanKind::Exclusive) => s.maybe_exclusive(local),
            None => s.maybe_borrowed(local),
        };
        if !relevant(in_b) && !relevant(out_b) {
            // Loan may open and close entirely inside the block.
            // Fall through to loan walk.
        }

        for loan in &self.loans {
            if loan.place_local != local {
                continue;
            }
            if let Some(k) = only
                && loan.kind != k
            {
                continue;
            }
            if self.loan_active_at(loan, point) {
                return true;
            }
        }
        false
    }

    fn loan_active_at(&self, loan: &Loan, point: ProgramPoint) -> bool {
        // Holder temp live at point?
        for t in loan.holder_temps.iter() {
            if self.temp_live_at(t, point) {
                return true;
            }
        }
        for l in loan.holder_locals.iter() {
            let loan_index = self
                .loans
                .iter()
                .position(|candidate| std::ptr::eq(candidate, loan));
            let structurally_present = loan_index.is_some_and(|loan_index| {
                self.local_holders_at
                    .get(point.block.as_usize())
                    .and_then(|points| points.get(point.stmt_index))
                    .and_then(|loans| loans.get(loan_index))
                    .is_some_and(|holders| holders.contains(l))
            });
            if structurally_present && self.local_live_at(l, point) {
                return true;
            }
        }
        false
    }

    /// Holder temp live at `point`?
    /// Entry uses live-in; interior/exit uses live-in ∪ live-out (sound over-approx).
    fn temp_live_at(&self, temp: TempId, point: ProgramPoint) -> bool {
        if point.stmt_index == 0 {
            return self.temp_live.live_in(point.block).contains(temp);
        }
        self.temp_live.live_in(point.block).contains(temp)
            || self.temp_live.live_out(point.block).contains(temp)
    }

    fn local_live_at(&self, local: LocalId, point: ProgramPoint) -> bool {
        if point.stmt_index == 0 {
            return self.local_live.live_in(point.block).contains(local);
        }
        self.local_live.live_in(point.block).contains(local)
            || self.local_live.live_out(point.block).contains(local)
    }
}
