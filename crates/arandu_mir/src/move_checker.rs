//! Intraprocedural AMIR move checking (M1).
//!
//! This pass tracks dense whole-local state plus sparse named-field move paths
//! across the AMIR CFG. Index and dereference projections remain conservative;
//! moves are recovered from `Load(place)` followed by consuming `Move(temp)`
//! operands.

use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator,
    BlockId, LocalId, TempId, for_each_rvalue_operand, for_each_rvalue_place,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::{BitSet, SymbolTable};
use arandu_lexer::Span;
use smallvec::SmallVec;
use std::collections::VecDeque;

/// Sink for block-tagged move diagnostics during CFG walk.
type MoveDiagSink<'a> = Option<(&'a SymbolTable, BlockId, &'a mut Vec<(BlockId, Diagnostic)>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMoveState {
    Available,
    Moved,
    MaybeMoved,
}

/// Ownership paths after each block, computed by the same transfer function as
/// diagnostics so cleanup cannot diverge from the move checker.
#[must_use]
pub(crate) fn move_states_at_block_exit(func: &AmirFunc) -> Vec<MoveState> {
    let bump = bumpalo::Bump::new();
    let Some(block_in) = compute_move_in(func, &bump) else {
        return vec![MoveState::new(func.locals.len()); func.blocks.len()];
    };
    let origins = temp_origins(func, &bump);
    block_in
        .iter()
        .enumerate()
        .map(|(index, incoming)| {
            let mut state = incoming.clone();
            apply_block(BlockId::from_usize(index), func, &origins, &mut state, None);
            state
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MoveState {
    moved: BitSet<LocalId>,
    maybe_moved: BitSet<LocalId>,
    moved_fields: SmallVec<[AmirPlace; 4]>,
    maybe_moved_fields: SmallVec<[AmirPlace; 4]>,
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
        let mut joined = Self::new(num_locals);
        for index in 0..num_locals {
            let local = LocalId::from_usize(index);
            joined.set(
                local,
                join_move_state(self.root_state(local), other.root_state(local)),
            );
        }

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

    pub(crate) fn place_itself_is_available(&self, place: &AmirPlace) -> bool {
        self.field_fact_state(place) == LocalMoveState::Available
    }

    fn has_moved_descendant(&self, place: &AmirPlace) -> bool {
        let is_descendant = |moved: &AmirPlace| {
            moved.local == place.local
                && moved.projections.len() > place.projections.len()
                && place_is_prefix(place, moved)
        };
        self.moved_fields.iter().any(is_descendant)
            || self.maybe_moved_fields.iter().any(is_descendant)
    }

    fn place_state(&self, place: &AmirPlace) -> LocalMoveState {
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

    fn is_monotonic_from(&self, old: &Self) -> bool {
        if !self.moved.is_superset_of(&old.moved) {
            return false;
        }
        for id in old.maybe_moved.iter() {
            if !self.maybe_moved.contains(id) && !self.moved.contains(id) {
                return false;
            }
        }
        old.moved_fields
            .iter()
            .chain(&old.maybe_moved_fields)
            .all(|place| self.place_state(place) != LocalMoveState::Available)
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
    let projections = if !place.projections.is_empty()
        && place
            .projections
            .iter()
            .all(|projection| matches!(projection, AmirProjection::Field(_)))
    {
        place.projections.clone()
    } else {
        SmallVec::new()
    };
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
    let Some(block_in) = compute_move_in(func, &bump) else {
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

pub fn check_moves(func: &AmirFunc, symbols: &SymbolTable) -> Vec<Diagnostic> {
    check_moves_by_block(func, symbols)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_moves`], tagging each diagnostic with the AMIR block of the use.
#[must_use]
pub fn check_moves_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
) -> Vec<(crate::amir::BlockId, Diagnostic)> {
    let bump = bumpalo::Bump::new();
    let Some(block_in) = compute_move_in(func, &bump) else {
        return Vec::new();
    };

    let temp_origins = temp_origins(func, &bump);
    let mut diagnostics = Vec::new();
    let mut block_in = block_in;
    for block in &func.blocks {
        // Take ownership of each IN set — only used once during the check walk.
        let mut state = std::mem::take(&mut block_in[block.id.as_usize()]);
        apply_block(
            block.id,
            func,
            &temp_origins,
            &mut state,
            Some((symbols, block.id, &mut diagnostics)),
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

fn compute_move_in<'bump>(
    func: &AmirFunc,
    bump: &'bump bumpalo::Bump,
) -> Option<bumpalo::collections::Vec<'bump, MoveState>> {
    let num_locals = func.locals.len();
    let num_blocks = func.blocks.len();

    if num_locals == 0 || num_blocks == 0 {
        return None;
    }

    let temp_origins = temp_origins(func, bump);
    let mut block_in = bumpalo::collections::Vec::with_capacity_in(num_blocks, bump);
    let mut block_out = bumpalo::collections::Vec::with_capacity_in(num_blocks, bump);
    for _ in 0..num_blocks {
        block_in.push(MoveState::new(num_locals));
        block_out.push(MoveState::new(num_locals));
    }
    let mut worklist = VecDeque::new();

    for block in &func.blocks {
        worklist.push_back(block.id);
    }

    let mut iterations = 0;
    let sanity_limit =
        num_blocks * num_locals * 2 + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;

    while let Some(bid) = worklist.pop_front() {
        iterations += 1;
        assert!(
            iterations <= sanity_limit,
            "move checker failed to converge within theoretical limit: {iterations} > {sanity_limit} ({num_blocks} blocks) — possível bug de monotonicidade no dataflow"
        );

        let bi = bid.as_usize();
        let block = &func.blocks[bi];
        let new_in = MoveState::join_predecessors(
            func.predecessors(bid)
                .iter()
                .map(|pred| &block_out[pred.as_usize()]),
            num_locals,
        );
        let mut new_out = new_in.clone();
        apply_block(block.id, func, &temp_origins, &mut new_out, None);

        debug_assert!(
            new_out.is_monotonic_from(&block_out[bi]),
            "Move checker dataflow is not monotonic at block {bi}"
        );

        if new_in != block_in[bi] || new_out != block_out[bi] {
            block_in[bi] = new_in;
            block_out[bi] = new_out;
            for succ in successors(&block.terminator) {
                worklist.push_back(succ);
            }
        }
    }

    Some(block_in)
}

fn temp_origins<'bump>(
    func: &AmirFunc,
    bump: &'bump bumpalo::Bump,
) -> bumpalo::collections::Vec<'bump, Option<AmirPlace>> {
    let mut origins =
        bumpalo::collections::Vec::from_iter_in(std::iter::repeat_n(None, func.temps.len()), bump);
    for (i, &param_temp) in func.params.iter().enumerate() {
        origins[param_temp.as_usize()] = Some(AmirPlace {
            local: LocalId::from_usize(i),
            projections: SmallVec::new(),
        });
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            origins[param.id.as_usize()] = Some(AmirPlace {
                local: param.local,
                projections: SmallVec::new(),
            });
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign { lhs, rhs } = stmt {
                    let mut found_origin = None;
                    match rhs {
                        // Named fields remain sparse move paths; dynamic index and
                        // dereference projections collapse to their root conservatively.
                        AmirRvalue::Load(place) => {
                            found_origin = Some(tracked_move_place(place));
                        }
                        AmirRvalue::Use(AmirOperand::Copy(t) | AmirOperand::Move(t)) => {
                            found_origin = origins[t.as_usize()].clone();
                        }
                        _ => {}
                    }
                    if let Some(loc) = found_origin
                        && origins[lhs.as_usize()].is_none()
                    {
                        origins[lhs.as_usize()] = Some(loc);
                        changed = true;
                    }
                }
            }
        }
    }
    origins
}

fn apply_block(
    block: crate::amir::BlockId,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    state: &mut MoveState,
    mut diagnostics: MoveDiagSink<'_>,
) {
    for stmt in func.block_stmts(block) {
        match stmt {
            AmirStmt::Assign { rhs, .. } => {
                check_rvalue_reads(rhs, func, state, &mut diagnostics);
                consume_rvalue(rhs, func, temp_origins, state, &mut diagnostics);
            }
            AmirStmt::Store { lhs, rhs } => {
                if !lhs.projections.is_empty() {
                    check_place_state(lhs, state.place_base_state(lhs), func, &mut diagnostics);
                }
                consume_operand(rhs, func, temp_origins, state, &mut diagnostics, false);
                state.restore_place(lhs);
            }
            AmirStmt::Call { callee, args, .. } => {
                consume_operand(callee, func, temp_origins, state, &mut diagnostics, false);
                for arg in args {
                    consume_operand(arg, func, temp_origins, state, &mut diagnostics, false);
                }
            }
            AmirStmt::Free(op) => {
                consume_operand(op, func, temp_origins, state, &mut diagnostics, true);
            }
            AmirStmt::Destroy(place) => {
                // Drop elaboration destroys owned fields before emitting a
                // final root Destroy for heap-backed aggregate storage. The
                // root cleanup must not be diagnosed as a second destruction
                // of an already-dropped field. Explicit destructors reject
                // partial field moves during lowering, so this exception is
                // limited to a root with descendant cleanup facts.
                let recursive_root_cleanup = place.projections.is_empty()
                    && state.root_state(place.local) == LocalMoveState::Available
                    && state.has_moved_descendant(place);
                if !recursive_root_cleanup {
                    check_consume_place(place, func, state, &mut diagnostics, true);
                }
                state.move_place(place);
            }
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
        }
    }

    match &func.block(block).terminator {
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            check_operand_read(condition, func, temp_origins, state, &mut diagnostics);
            let mut true_state = state.clone();
            let mut false_state = state.clone();

            for arg in true_args {
                consume_operand(
                    arg,
                    func,
                    temp_origins,
                    &mut true_state,
                    &mut diagnostics,
                    false,
                );
            }
            for arg in false_args {
                consume_operand(
                    arg,
                    func,
                    temp_origins,
                    &mut false_state,
                    &mut diagnostics,
                    false,
                );
            }
            *state = MoveState::join_predecessors(
                [&true_state, &false_state].into_iter(),
                func.locals.len(),
            );
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
            ..
        } => {
            check_operand_read(discriminant, func, temp_origins, state, &mut diagnostics);
            let mut arm_states = Vec::with_capacity(targets.len() + 1);

            for (_, _, args) in targets {
                let mut arm_state = state.clone();
                for arg in args {
                    consume_operand(
                        arg,
                        func,
                        temp_origins,
                        &mut arm_state,
                        &mut diagnostics,
                        false,
                    );
                }
                arm_states.push(arm_state);
            }
            let mut otherwise_state = state.clone();
            for arg in &otherwise.1 {
                consume_operand(
                    arg,
                    func,
                    temp_origins,
                    &mut otherwise_state,
                    &mut diagnostics,
                    false,
                );
            }
            arm_states.push(otherwise_state);

            *state = MoveState::join_predecessors(arm_states.iter(), func.locals.len());
        }
        AmirTerminator::Goto { args, .. } => {
            for arg in args {
                consume_operand(arg, func, temp_origins, state, &mut diagnostics, false);
            }
        }
        AmirTerminator::Suspend { future, args, .. } => {
            check_operand_read(future, func, temp_origins, state, &mut diagnostics);
            for arg in args {
                consume_operand(arg, func, temp_origins, state, &mut diagnostics, false);
            }
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}

fn check_rvalue_reads(
    rvalue: &AmirRvalue,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    for_each_rvalue_place(rvalue, |place| {
        check_place_read(place, func, state, diagnostics);
    });
}

fn consume_rvalue(
    rvalue: &AmirRvalue,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    state: &mut MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    // Shared visitor covers all operand-bearing rvalues (RC-ANALYSIS-LOAD).
    // Load/Borrow/BorrowMut only contribute Index projection operands, not the base place.
    for_each_rvalue_operand(rvalue, |op| {
        consume_operand(op, func, temp_origins, state, diagnostics, false);
    });
}

fn check_operand_read(
    op: &AmirOperand,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    let (AmirOperand::Copy(temp) | AmirOperand::Move(temp)) = op else {
        return;
    };
    if let Some(place) = origin_for(*temp, temp_origins) {
        check_place_read(place, func, state, diagnostics);
    }
}

fn consume_operand(
    op: &AmirOperand,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    state: &mut MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
    double_free: bool,
) {
    let AmirOperand::Move(temp) = op else {
        check_operand_read(op, func, temp_origins, state, diagnostics);
        return;
    };

    if func.temps[temp.as_usize()].is_copy {
        return;
    }
    let Some(place) = origin_for(*temp, temp_origins) else {
        return;
    };
    check_consume_place(place, func, state, diagnostics, double_free);
    state.move_place(place);
}

fn check_place_read(
    place: &AmirPlace,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    check_place_state(place, state.place_state(place), func, diagnostics);
}

fn check_place_state(
    place: &AmirPlace,
    state: LocalMoveState,
    func: &AmirFunc,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    let Some((symbols, block, diagnostics)) = diagnostics.as_mut() else {
        return;
    };
    let block = *block;
    match state {
        LocalMoveState::Available => {}
        LocalMoveState::Moved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O001UseAfterMove,
                place.local,
                func,
                symbols,
                "use of moved value",
                "value was moved before this use",
            ),
        )),
        LocalMoveState::MaybeMoved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O007InconsistentMoveBetweenBranches,
                place.local,
                func,
                symbols,
                "value may have been moved on some control-flow paths",
                "ensure all branches leave the value in a consistent ownership state",
            ),
        )),
    }
}

fn check_consume_place(
    place: &AmirPlace,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
    double_free: bool,
) {
    let Some((symbols, block, diagnostics)) = diagnostics.as_mut() else {
        return;
    };
    let block = *block;
    match state.place_state(place) {
        LocalMoveState::Available => {}
        LocalMoveState::Moved if double_free => diagnostics.push((
            block,
            move_diag(
                DiagCode::O005DoubleFree,
                place.local,
                func,
                symbols,
                "double free/drop of moved value",
                "value was already consumed on this path",
            ),
        )),
        LocalMoveState::Moved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O001UseAfterMove,
                place.local,
                func,
                symbols,
                "use of moved value",
                "value was already consumed on this path",
            ),
        )),
        LocalMoveState::MaybeMoved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O007InconsistentMoveBetweenBranches,
                place.local,
                func,
                symbols,
                "value may have been moved on some control-flow paths",
                "ensure all branches leave the value in a consistent ownership state",
            ),
        )),
    }
}

fn origin_for(temp: TempId, temp_origins: &[Option<AmirPlace>]) -> Option<&AmirPlace> {
    temp_origins.get(temp.as_usize()).and_then(Option::as_ref)
}

#[cold]
#[inline(never)]
fn move_diag(
    code: DiagCode,
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    prefix: &str,
    note: &str,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_diag_span(local, func, symbols);
    Diagnostic::error(code, format!("{prefix} `{name}`"), span).with_note(note)
}

/// Prefer use site → declaration → symbol span → zero (S-SPAN-THREAD).
fn local_diag_span(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> Span {
    let Some(l) = func.locals.get(local.as_usize()) else {
        return Span::new(0, 0, 0);
    };
    if let Some(u) = l.use_span
        && u.start != u.end
    {
        return u;
    }
    if l.span.start != l.span.end {
        return l.span;
    }
    if let Some(sym) = l.symbol {
        let s = symbols.get(sym).span;
        if s.start != s.end {
            return s;
        }
    }
    Span::new(0, 0, 0)
}

fn local_name(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> String {
    func.locals
        .get(local.as_usize())
        .and_then(|local| local.symbol)
        .map_or_else(
            || format!("s{}", local.as_usize()),
            |symbol| symbols.get(symbol).name.to_string(),
        )
}

fn successors(term: &AmirTerminator) -> Vec<crate::amir::BlockId> {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => Vec::new(),
        AmirTerminator::Goto { target, .. } => vec![*target],
        AmirTerminator::Suspend { resume, .. } => vec![*resume],
        AmirTerminator::Branch {
            if_true, if_false, ..
        } => vec![*if_true, *if_false],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut out: Vec<_> = targets.iter().map(|(_, block, _)| *block).collect();
            out.push(otherwise.0);
            out
        }
    }
}

#[cfg(test)]
#[path = "move_checker_tests.rs"]
mod tests;
