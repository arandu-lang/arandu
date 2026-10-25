use crate::BitSet;
use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, AmirTerminator, TempId,
    for_each_rvalue_operand, for_each_rvalue_place,
};
// for_each_rvalue_place used in reverse_transfer_stmt
use crate::liveness::TempLiveness;

/// Per-block: `live_before[si]` = temps live immediately before statement `si`.
/// Extra slot `live_before[n]` = live before terminator (= live-out after reverse walk).
pub(super) fn compute_temp_live_before(
    func: &AmirFunc,
    temp_live: &TempLiveness,
) -> Vec<Vec<BitSet<TempId>>> {
    let num_temps = func.temps.len();
    let mut result = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let stmts: Vec<&AmirStmt> = func.block_stmts(block.id).collect();
        let n = stmts.len();
        let mut live_before = vec![BitSet::with_capacity(num_temps); n + 1];

        let mut live = temp_live.live_out(block.id).clone();
        // `Return` consumes the conventional return register (`TempId(0)`).
        // The terminator has no explicit operand in AMIR, so seed it here or a
        // returned borrow appears dead before epilogue `Destroy` statements.
        if matches!(block.terminator, AmirTerminator::Return) && !func.temps.is_empty() {
            live.insert(TempId::from_usize(0));
        }
        // Jump arguments and branch conditions are uses: a borrow passed to
        // a successor's block parameter is live until the jump, past any
        // `Destroy` of its owner earlier in the block.
        crate::amir::visit::for_each_terminator_operand(&block.terminator, |op| {
            mark_use(op, &mut live);
        });
        // Before terminator:
        live_before[n] = live.clone();
        // Reverse statements.
        for si in (0..n).rev() {
            // live = live after stmt si, before killing def / adding uses of si
            // After reverse transfer, live = live before stmt si.
            reverse_transfer_stmt(stmts[si], &mut live, num_temps);
            live_before[si] = live.clone();
        }
        // Optionally union with live_in for safety (phi / incomplete use info).
        live_before[0].union_with(temp_live.live_in(block.id));

        result.push(live_before);
    }
    result
}

fn reverse_transfer_stmt(stmt: &AmirStmt, live: &mut BitSet<TempId>, _num_temps: usize) {
    // Reverse: first undo def, then add uses (backward dataflow step).
    match stmt {
        AmirStmt::Assign { lhs, rhs } => {
            live.remove(*lhs);
            for_each_rvalue_operand(rhs, |op| mark_use(op, live));
            for_each_rvalue_place(rhs, |place| mark_place_index_uses(place, live));
        }
        AmirStmt::Store { lhs, rhs } => {
            mark_use(rhs, live);
            mark_place_index_uses(lhs, live);
        }
        AmirStmt::Call {
            lhs, callee, args, ..
        } => {
            if let Some(t) = lhs {
                live.remove(*t);
            }
            mark_use(callee, live);
            for a in args {
                mark_use(a, live);
            }
        }
        AmirStmt::Free(op) => mark_use(op, live),
        AmirStmt::Destroy(place) => mark_place_index_uses(place, live),
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn mark_use(op: &AmirOperand, live: &mut BitSet<TempId>) {
    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
        live.insert(*t);
    }
}

fn mark_place_index_uses(place: &AmirPlace, live: &mut BitSet<TempId>) {
    for proj in &place.projections {
        if let crate::amir::AmirProjection::Index(op) = proj {
            mark_use(op, live);
        }
    }
}

/// Map each temp to a stack local origin when it is a Load of that local (move path).
#[derive(Clone, PartialEq, Eq)]
enum TempOrigin {
    Unset,
    Unique(AmirPlace),
    Ambiguous,
}

impl TempOrigin {
    fn join(&mut self, place: &AmirPlace) -> bool {
        match self {
            Self::Unset => {
                *self = Self::Unique(place.clone());
                true
            }
            Self::Unique(current) => {
                if current == place {
                    false
                } else {
                    *self = Self::Ambiguous;
                    true
                }
            }
            Self::Ambiguous => false,
        }
    }

    fn join_origin(&mut self, other: &Self) -> bool {
        match other {
            Self::Unset => false,
            Self::Unique(place) => self.join(place),
            Self::Ambiguous => {
                if matches!(self, Self::Ambiguous) {
                    false
                } else {
                    *self = Self::Ambiguous;
                    true
                }
            }
        }
    }
}

pub(super) fn temp_origins_from_loads(func: &AmirFunc) -> (Vec<Option<AmirPlace>>, bool) {
    let mut origins = vec![TempOrigin::Unset; func.temps.len()];
    let mut changed = true;
    let mut iterations = 0;
    let limit =
        (func.temps.len() * 2 + 10).max(crate::analysis_limits::TEMP_ORIGIN_SOLVER_MAX_PASSES);
    while changed && iterations < limit {
        changed = false;
        iterations += 1;
        for block in &func.blocks {
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Load(place),
                } = stmt
                    && origins[lhs.as_usize()].join(place)
                {
                    changed = true;
                }
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Use(AmirOperand::Copy(t) | AmirOperand::Move(t)),
                } = stmt
                {
                    let src = origins[t.as_usize()].clone();
                    if origins[lhs.as_usize()].join_origin(&src) {
                        changed = true;
                    }
                }
            }
        }
    }
    let result = origins
        .into_iter()
        .map(|origin| match origin {
            TempOrigin::Unique(place) => Some(place),
            _ => None,
        })
        .collect();
    (result, changed)
}
