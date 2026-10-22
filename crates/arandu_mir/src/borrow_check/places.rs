use crate::BitSet;
use crate::amir::{AmirPlace, AmirProjection, LocalId, TempId};
use crate::borrow_facts::{FuncBorrowFacts, HolderProjection, Loan, LoanKind, ProgramPoint};

pub(super) fn place_borrowed_at(
    place: &AmirPlace,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    point: ProgramPoint,
) -> bool {
    active_kind_at(place, LoanKind::Shared, live, facts, point)
        || active_kind_at(place, LoanKind::Exclusive, live, facts, point)
}

pub(super) fn active_kind_at(
    place: &AmirPlace,
    kind: LoanKind,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    point: ProgramPoint,
) -> bool {
    facts.loans.iter().enumerate().any(|(loan_index, loan)| {
        loan.kind == kind
            && places_may_overlap(
                place.local,
                &place.projections,
                loan.place_local,
                &loan.place_projections,
            )
            && loan_holders_live(loan_index, loan, live, facts, point)
    })
}

pub(super) fn places_may_overlap(
    left_local: LocalId,
    left: &[AmirProjection],
    right_local: LocalId,
    right: &[AmirProjection],
) -> bool {
    if left_local != right_local {
        return false;
    }
    for (left, right) in left.iter().zip(right) {
        match (left, right) {
            (AmirProjection::Field(left), AmirProjection::Field(right)) if left != right => {
                return false;
            }
            (AmirProjection::Field(_), AmirProjection::Field(_)) => {}
            (AmirProjection::Variant(left), AmirProjection::Variant(right)) if left != right => {
                return false;
            }
            (AmirProjection::Variant(_), AmirProjection::Variant(_)) => {}
            (
                AmirProjection::Payload { index: left, .. },
                AmirProjection::Payload { index: right, .. },
            ) if left != right => {
                return false;
            }
            (AmirProjection::Payload { .. }, AmirProjection::Payload { .. }) => {}
            (AmirProjection::TupleField(left), AmirProjection::TupleField(right))
                if left != right =>
            {
                return false;
            }
            (AmirProjection::TupleField(_), AmirProjection::TupleField(_)) => {}
            (AmirProjection::Deref, AmirProjection::Deref) => {}
            // Index operands and dereferences may refer to the same storage.
            _ => return true,
        }
    }
    // Equal paths and prefix paths overlap (`pair` overlaps `pair.left`).
    true
}

pub(super) fn loan_holders_live(
    loan_index: usize,
    loan: &Loan,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    point: ProgramPoint,
) -> bool {
    for t in loan.holder_temps.iter() {
        if live.contains(t) {
            return true;
        }
    }
    for l in loan.holder_locals.iter() {
        let structurally_present = facts
            .local_holders_at
            .get(point.block.as_usize())
            .and_then(|points| points.get(point.stmt_index))
            .and_then(|loans| loans.get(loan_index))
            .is_some_and(|holders| holders.contains(l));
        let aggregate_carrier = loan
            .holder_local_paths
            .get(l.as_usize())
            .is_some_and(|paths| {
                paths.iter().any(|path| {
                    !path.0.is_empty()
                        && path.0.iter().all(|projection| {
                            matches!(
                                projection,
                                HolderProjection::Slot(_) | HolderProjection::NamedField { .. }
                            )
                        })
                })
            });
        // A guard/carrier can be both initialized and consumed inside one
        // block. Such a local is absent at both block boundaries, while its
        // embedded loan remains live between the store and the later use/drop.
        let holder_is_live = facts
            .local_live
            .live_before(point.block, point.stmt_index)
            .map_or_else(
                || {
                    aggregate_carrier
                        && (facts.local_live.live_in(point.block).contains(l)
                            || facts.local_live.live_out(point.block).contains(l))
                },
                |live| live.contains(l),
            );
        if structurally_present && holder_is_live {
            return true;
        }
    }
    false
}
