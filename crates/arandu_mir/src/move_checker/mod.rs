//! Intraprocedural AMIR move checking (M1).
//!
//! This pass tracks dense whole-local state plus sparse named-field move paths
//! across the AMIR CFG. Index and dereference projections remain conservative;
//! moves are recovered from `Load(place)` followed by consuming `Move(temp)`
//! operands.

use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProjection, BlockId, LocalId,
};
use crate::diagnostics::Diagnostic;
use crate::{BitSet, SymbolTable};
use smallvec::SmallVec;

#[cfg(test)]
use crate::Span;
#[cfg(test)]
use crate::amir::{AmirRvalue, AmirStmt, AmirTerminator, TempId};
#[cfg(test)]
use crate::diagnostics::DiagCode;

mod walk;

use walk::{apply_block, apply_stmt, apply_terminator, compute_move_in};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMoveState {
    Available,
    Moved,
    MaybeMoved,
}

/// Ownership paths on entry and after each block, computed from a single
/// dataflow pass so cleanup cannot diverge from the move checker.
#[must_use]
pub(crate) fn move_states_entry_and_exit(func: &AmirFunc) -> (Vec<MoveState>, Vec<MoveState>) {
    let bump = bumpalo::Bump::new();
    let Some((block_in, origins)) = compute_move_in(func, &bump) else {
        let empty = vec![MoveState::new(func.locals.len()); func.blocks.len()];
        return (empty.clone(), empty);
    };
    let mut entry = Vec::with_capacity(func.blocks.len());
    let mut exit = Vec::with_capacity(func.blocks.len());
    for (index, incoming) in block_in.into_iter().enumerate() {
        let mut state = incoming.clone();
        apply_block(BlockId::from_usize(index), func, &origins, &mut state, None);
        entry.push(incoming);
        exit.push(state);
    }
    (entry, exit)
}

/// Ownership paths after each block and immediately before every statement,
/// computed from a single dataflow pass for drop elaboration.
#[must_use]
pub(crate) fn move_states_exit_and_before_statements(
    func: &AmirFunc,
) -> (Vec<MoveState>, Vec<Vec<MoveState>>) {
    let bump = bumpalo::Bump::new();
    let Some((block_in, origins)) = compute_move_in(func, &bump) else {
        let exit = vec![MoveState::new(func.locals.len()); func.blocks.len()];
        let before = func
            .blocks
            .iter()
            .map(|block| vec![MoveState::new(func.locals.len()); block.statements.len_usize()])
            .collect();
        return (exit, before);
    };
    let mut exit = Vec::with_capacity(func.blocks.len());
    let mut before = Vec::with_capacity(func.blocks.len());
    for (block, incoming) in func.blocks.iter().zip(block_in) {
        let mut state = incoming;
        let mut snapshots = Vec::with_capacity(block.statements.len_usize());
        for stmt in func.block_stmts(block.id) {
            snapshots.push(state.clone());
            apply_stmt(stmt, func, &origins, &mut state, &mut None);
        }
        apply_terminator(block.id, func, &origins, &mut state, &mut None);
        before.push(snapshots);
        exit.push(state);
    }
    (exit, before)
}

/// Move-state snapshot immediately before every statement, used by cleanup
/// elaboration passes that only fire when indirect/dynamic drops exist.
pub(crate) fn move_states_before_statements(func: &AmirFunc) -> Vec<Vec<MoveState>> {
    move_states_exit_and_before_statements(func).1
}

#[derive(Debug, Clone, Eq, Default)]
pub(crate) struct MoveState {
    moved: BitSet<LocalId>,
    maybe_moved: BitSet<LocalId>,
    moved_fields: SmallVec<[AmirPlace; 4]>,
    maybe_moved_fields: SmallVec<[AmirPlace; 4]>,
}

impl PartialEq for MoveState {
    fn eq(&self, other: &Self) -> bool {
        self.moved == other.moved
            && self.maybe_moved == other.maybe_moved
            && same_places(&self.moved_fields, &other.moved_fields)
            && same_places(&self.maybe_moved_fields, &other.maybe_moved_fields)
    }
}

fn same_places(a: &[AmirPlace], b: &[AmirPlace]) -> bool {
    a.len() == b.len() && a.iter().all(|item| b.contains(item))
}

impl MoveState {
    fn new(num_locals: usize) -> Self {
        Self {
            moved: BitSet::with_capacity(num_locals),
            maybe_moved: BitSet::with_capacity(num_locals),
            moved_fields: SmallVec::new(),
            maybe_moved_fields: SmallVec::new(),
        }
    }

    #[tracing::instrument(level = "trace", target = "arandu_mir::move_checker", skip_all)]
    fn join_predecessors<'a>(preds: impl Iterator<Item = &'a Self>, num_locals: usize) -> Self {
        let mut preds = preds;
        let Some(first) = preds.next() else {
            return Self::new(num_locals);
        };
        let mut acc = first.clone();
        for pred in preds {
            acc = acc.join(pred, num_locals);
        }
        acc
    }

    fn join(&self, other: &Self, num_locals: usize) -> Self {
        let num_words = num_locals.div_ceil(64);
        let self_moved = self.moved.words();
        let other_moved = other.moved.words();
        let self_maybe = self.maybe_moved.words();
        let other_maybe = other.maybe_moved.words();

        let mut moved_words = Vec::with_capacity(num_words);
        let mut maybe_words = Vec::with_capacity(num_words);
        for i in 0..num_words {
            let sm = self_moved.get(i).copied().unwrap_or(0);
            let om = other_moved.get(i).copied().unwrap_or(0);
            let sy = self_maybe.get(i).copied().unwrap_or(0);
            let oy = other_maybe.get(i).copied().unwrap_or(0);
            // Truth table for join_move_state(a, b) with mutually exclusive (moved, maybe):
            //   Moved + Moved           -> Moved        (sm & om)
            //   Any mismatch or Maybe   -> MaybeMoved   (sy | oy | (sm ^ om))
            //   Available + Available   -> Available    (0, 0)
            moved_words.push(sm & om);
            maybe_words.push(sy | oy | (sm ^ om));
        }

        let mut joined = Self {
            moved: BitSet::from_words(moved_words),
            maybe_moved: BitSet::from_words(maybe_words),
            moved_fields: SmallVec::new(),
            maybe_moved_fields: SmallVec::new(),
        };

        if !self.moved_fields.is_empty()
            || !self.maybe_moved_fields.is_empty()
            || !other.moved_fields.is_empty()
            || !other.maybe_moved_fields.is_empty()
        {
            let field_places = self
                .moved_fields
                .iter()
                .chain(&self.maybe_moved_fields)
                .chain(&other.moved_fields)
                .chain(&other.maybe_moved_fields);
            for place in field_places {
                if joined.has_exact_field(place) {
                    continue;
                }
                let state =
                    join_move_state(self.field_fact_state(place), other.field_fact_state(place));
                joined.push_field_state(place.clone(), state);
            }
        }
        joined
    }

    fn root_state(&self, local: LocalId) -> LocalMoveState {
        if self.moved.contains(local) {
            LocalMoveState::Moved
        } else if self.maybe_moved.contains(local) {
            LocalMoveState::MaybeMoved
        } else {
            LocalMoveState::Available
        }
    }

    fn set(&mut self, local: LocalId, state: LocalMoveState) {
        self.moved_fields.retain(|place| place.local != local);
        self.maybe_moved_fields.retain(|place| place.local != local);
        match state {
            LocalMoveState::Available => {
                self.moved.remove(local);
                self.maybe_moved.remove(local);
            }
            LocalMoveState::Moved => {
                self.moved.insert(local);
                self.maybe_moved.remove(local);
            }
            LocalMoveState::MaybeMoved => {
                self.moved.remove(local);
                self.maybe_moved.insert(local);
            }
        }
    }

    /// Every place moved on some path into this point and not on others:
    /// whole locals first, then projected places, in a fixed order.
    pub(crate) fn maybe_moved_places(&self) -> Vec<AmirPlace> {
        let mut places: Vec<AmirPlace> = self
            .maybe_moved
            .iter()
            .map(|local| AmirPlace {
                local,
                projections: SmallVec::new(),
            })
            .collect();
        places.extend(self.maybe_moved_fields.iter().cloned());
        places
    }

    pub(crate) fn place_itself_is_available(&self, place: &AmirPlace) -> bool {
        self.field_fact_state(place) == LocalMoveState::Available
    }

    fn has_moved_descendant(&self, place: &AmirPlace) -> bool {
        let tracked = tracked_move_place(place);
        let is_descendant = |moved: &AmirPlace| {
            moved.local == tracked.local
                && moved.projections.len() > tracked.projections.len()
                && place_is_prefix(&tracked, moved)
        };
        self.moved_fields.iter().any(is_descendant)
            || self.maybe_moved_fields.iter().any(is_descendant)
    }

    pub(crate) fn moved_descendant_paths<'a>(
        &'a self,
        place: &'a AmirPlace,
    ) -> impl Iterator<Item = AmirPlace> + 'a {
        let tracked = tracked_move_place(place);
        self.moved_fields.iter().filter_map(move |moved| {
            if moved.local != tracked.local
                || moved.projections.len() <= tracked.projections.len()
                || !place_is_prefix(&tracked, moved)
            {
                return None;
            }
            // Drop elaboration needs an addressable AMIR place, not our
            // canonical ownership path: restore the caller's dereferences.
            let mut projected = place.clone();
            projected
                .projections
                .extend_from_slice(&moved.projections[tracked.projections.len()..]);
            Some(projected)
        })
    }

    pub(crate) fn has_maybe_moved_descendant(&self, place: &AmirPlace) -> bool {
        let tracked = tracked_move_place(place);
        self.maybe_moved_fields.iter().any(|moved| {
            moved.local == tracked.local
                && moved.projections.len() > tracked.projections.len()
                && place_is_prefix(&tracked, moved)
        })
    }

    fn place_state(&self, place: &AmirPlace) -> LocalMoveState {
        let tracked = tracked_move_place(place);
        let place = &tracked;
        let root = self.root_state(place.local);
        if root != LocalMoveState::Available {
            return root;
        }
        if self
            .moved_fields
            .iter()
            .any(|moved| places_overlap(moved, place))
        {
            LocalMoveState::Moved
        } else if self
            .maybe_moved_fields
            .iter()
            .any(|moved| places_overlap(moved, place))
        {
            LocalMoveState::MaybeMoved
        } else {
            LocalMoveState::Available
        }
    }

    fn field_fact_state(&self, place: &AmirPlace) -> LocalMoveState {
        // Writes and moves store canonical paths. Reads (including the
        // drop-on-assignment query) must use exactly the same representation.
        // Comparing `Deref.Field` against a stored `Field` loses the move and
        // destroys the old owner twice during e.g. HashMap rehashing.
        let tracked = tracked_move_place(place);
        let place = &tracked;
        let root = self.root_state(place.local);
        if root != LocalMoveState::Available {
            return root;
        }
        if self
            .moved_fields
            .iter()
            .any(|moved| place_is_prefix(moved, place))
        {
            LocalMoveState::Moved
        } else if self
            .maybe_moved_fields
            .iter()
            .any(|moved| place_is_prefix(moved, place))
        {
            LocalMoveState::MaybeMoved
        } else {
            LocalMoveState::Available
        }
    }

    fn place_base_state(&self, place: &AmirPlace) -> LocalMoveState {
        let tracked = tracked_move_place(place);
        let place = &tracked;
        let root = self.root_state(place.local);
        if root != LocalMoveState::Available {
            return root;
        }
        let strict_ancestor = |moved: &AmirPlace| {
            moved.projections.len() < place.projections.len() && place_is_prefix(moved, place)
        };
        if self.moved_fields.iter().any(strict_ancestor) {
            LocalMoveState::Moved
        } else if self.maybe_moved_fields.iter().any(strict_ancestor) {
            LocalMoveState::MaybeMoved
        } else {
            LocalMoveState::Available
        }
    }

    fn move_place(&mut self, place: &AmirPlace) {
        let tracked = tracked_move_place(place);
        if tracked.projections.is_empty() {
            self.set(tracked.local, LocalMoveState::Moved);
        } else {
            self.push_field_state(tracked, LocalMoveState::Moved);
        }
    }

    fn restore_place(&mut self, place: &AmirPlace) {
        let tracked = tracked_move_place(place);
        if tracked.projections.is_empty() {
            self.set(tracked.local, LocalMoveState::Available);
            return;
        }
        self.moved_fields
            .retain(|moved| !place_is_prefix(&tracked, moved));
        self.maybe_moved_fields
            .retain(|moved| !place_is_prefix(&tracked, moved));
    }

    fn has_exact_field(&self, place: &AmirPlace) -> bool {
        self.moved_fields.contains(place) || self.maybe_moved_fields.contains(place)
    }

    fn push_field_state(&mut self, place: AmirPlace, state: LocalMoveState) {
        match state {
            LocalMoveState::Available => {}
            LocalMoveState::Moved => {
                if !self.moved_fields.contains(&place) {
                    self.moved_fields.push(place);
                }
            }
            LocalMoveState::MaybeMoved => {
                if !self.maybe_moved_fields.contains(&place) {
                    self.maybe_moved_fields.push(place);
                }
            }
        }
    }
}

fn join_move_state(left: LocalMoveState, right: LocalMoveState) -> LocalMoveState {
    if left == right {
        left
    } else {
        LocalMoveState::MaybeMoved
    }
}

fn tracked_move_place(place: &AmirPlace) -> AmirPlace {
    // Dereference is an access route, not an ownership path component. Keep
    // named fields beneath `&mut self` distinct so replacing one field restores
    // just that field instead of marking the receiver binding itself moved.
    // Dynamic indices remain conservative and collapse to their root.
    let mut projections = SmallVec::new();
    for projection in &place.projections {
        match projection {
            AmirProjection::Deref => {}
            AmirProjection::Field(field) => projections.push(AmirProjection::Field(*field)),
            AmirProjection::Variant(tag) => projections.push(AmirProjection::Variant(*tag)),
            projection @ AmirProjection::Payload { .. } => projections.push(*projection),
            AmirProjection::TupleField(index) => {
                projections.push(AmirProjection::TupleField(*index));
            }
            projection @ AmirProjection::Index(AmirOperand::Constant(AmirConstant::Pool(_))) => {
                projections.push(*projection);
            }
            projection @ AmirProjection::IndexConstant(_) => projections.push(*projection),
            AmirProjection::Index(_) => {
                return AmirPlace {
                    local: place.local,
                    projections: SmallVec::new(),
                };
            }
        }
    }
    AmirPlace {
        local: place.local,
        projections,
    }
}

fn place_is_prefix(prefix: &AmirPlace, place: &AmirPlace) -> bool {
    prefix.local == place.local
        && prefix.projections.len() <= place.projections.len()
        && prefix
            .projections
            .iter()
            .zip(&place.projections)
            .all(|(left, right)| left == right)
}

fn places_overlap(left: &AmirPlace, right: &AmirPlace) -> bool {
    place_is_prefix(left, right) || place_is_prefix(right, left)
}

/// Count of locals that are `Moved` or `MaybeMoved` at each block entry.
#[must_use]
pub fn moved_in_counts(func: &AmirFunc) -> Vec<u32> {
    let bump = bumpalo::Bump::new();
    let Some((block_in, _)) = compute_move_in(func, &bump) else {
        return vec![0; func.blocks.len()];
    };
    block_in
        .iter()
        .map(|state| {
            let mut locals = state.moved.clone();
            locals.union_with(&state.maybe_moved);
            for place in state.moved_fields.iter().chain(&state.maybe_moved_fields) {
                locals.insert(place.local);
            }
            locals.len() as u32
        })
        .collect()
}

pub fn check_moves(
    func: &AmirFunc,
    symbols: &SymbolTable,
    types: &dyn arandu_middle::type_context::AmirTypeContext,
) -> Vec<Diagnostic> {
    check_moves_by_block(func, symbols, types)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_moves`], tagging each diagnostic with the AMIR block of the use.
#[must_use]
pub fn check_moves_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
    types: &dyn arandu_middle::type_context::AmirTypeContext,
) -> Vec<(crate::amir::BlockId, Diagnostic)> {
    let bump = bumpalo::Bump::new();
    let Some((mut block_in, temp_origins)) = compute_move_in(func, &bump) else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    for block in &func.blocks {
        // Take ownership of each IN set — only used once during the check walk.
        let mut state = std::mem::take(&mut block_in[block.id.as_usize()]);
        apply_block(
            block.id,
            func,
            &temp_origins,
            &mut state,
            Some(walk::MoveDiagnostics {
                symbols,
                types,
                block: block.id,
                out: &mut diagnostics,
            }),
        );
    }

    // A non-Copy value is represented as `Load(place)` followed by a consuming
    // `Move(temp)`. If the place was already moved, both instructions observe the
    // same source error. Keep one diagnostic per block/code/source expression while
    // preserving traversal order and the first (read-site) explanation.
    let mut unique = Vec::with_capacity(diagnostics.len());
    for candidate in diagnostics {
        let (candidate_block, candidate_diagnostic) = &candidate;
        if !unique
            .iter()
            .any(|(block, diagnostic): &(BlockId, Diagnostic)| {
                block == candidate_block
                    && diagnostic.code == candidate_diagnostic.code
                    && diagnostic.span == candidate_diagnostic.span
                    && diagnostic.message == candidate_diagnostic.message
            })
        {
            unique.push(candidate);
        }
    }
    unique
}

#[cfg(test)]
#[path = "../move_checker_tests.rs"]
mod tests;
