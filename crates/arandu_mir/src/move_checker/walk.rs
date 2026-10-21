use super::{LocalMoveState, MoveState};
use crate::SymbolTable;
use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator,
    BlockId, LocalId, TempId, for_each_rvalue_operand, for_each_rvalue_place,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use arandu_lexer::Span;
use smallvec::SmallVec;
use std::collections::VecDeque;

/// Sink for block-tagged move diagnostics during CFG walk.
type MoveDiagSink<'a> = Option<(&'a SymbolTable, BlockId, &'a mut Vec<(BlockId, Diagnostic)>)>;

pub(super) fn compute_move_in<'bump>(
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
    let mut edge_out = vec![Vec::<(BlockId, MoveState)>::new(); num_blocks];
    for _ in 0..num_blocks {
        block_in.push(MoveState::new(num_locals));
    }
    let mut worklist = VecDeque::new();

    for block in &func.blocks {
        if func.predecessors(block.id).is_empty() {
            worklist.push_back(block.id);
        }
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
        let incoming = func
            .predecessors(bid)
            .iter()
            .flat_map(|pred| {
                edge_out[pred.as_usize()]
                    .iter()
                    .filter(move |(target, _)| *target == bid)
                    .map(|(_, state)| state)
            })
            .collect::<Vec<_>>();
        if !func.predecessors(bid).is_empty() && incoming.is_empty() {
            continue;
        }
        let new_in = MoveState::join_predecessors(incoming.into_iter(), num_locals);
        let new_edges = block_edge_states(block, func, &temp_origins, &new_in);
        let changed = new_in != block_in[bi] || new_edges != edge_out[bi];
        if changed {
            block_in[bi] = new_in;
            edge_out[bi] = new_edges;
            for succ in successors(&block.terminator) {
                if succ.as_usize() < num_blocks {
                    worklist.push_back(succ);
                }
            }
        }
    }

    Some(block_in)
}

pub(super) fn temp_origins<'bump>(
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
                            // Keep the original projections for the borrow check
                            // below. MoveState may conservatively collapse an
                            // indexed/dereferenced path, but erasing `Deref` here
                            // would make moving an owned field through `&T` look
                            // like an ordinary local move.
                            found_origin = Some(place.clone());
                        }
                        AmirRvalue::EnumPayload {
                            value,
                            variant_tag,
                            index,
                            field_ty,
                            tuple_ty,
                            ..
                        } => {
                            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = value
                                && let Some(mut place) = origins[temp.as_usize()].clone()
                            {
                                place
                                    .projections
                                    .push(AmirProjection::Variant(*variant_tag));
                                place.projections.push(AmirProjection::Payload {
                                    variant_tag: *variant_tag,
                                    index: *index,
                                    field_ty: *field_ty,
                                    tuple_ty: *tuple_ty,
                                });
                                found_origin = Some(place);
                            }
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

/// Transfer ownership state independently to each CFG successor. Joining a
/// branch before propagation makes a move in one arm appear to have happened
/// in every arm, which is invalid for match fallthrough and conditional moves.
fn block_edge_states(
    block: &crate::amir::AmirBasicBlock,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    input: &MoveState,
) -> Vec<(BlockId, MoveState)> {
    let mut state = input.clone();
    for stmt in func.block_stmts(block.id) {
        apply_stmt(stmt, func, temp_origins, &mut state, &mut None);
    }

    let mut edges = Vec::new();
    match &block.terminator {
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
        AmirTerminator::Goto { target, args } => {
            let mut edge = state;
            for arg in args {
                consume_operand(arg, func, temp_origins, &mut edge, &mut None, false);
            }
            edges.push((*target, edge));
        }
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => {
            check_operand_read(condition, func, temp_origins, &state, &mut None);
            let mut true_state = state.clone();
            for arg in true_args {
                consume_operand(arg, func, temp_origins, &mut true_state, &mut None, false);
            }
            let mut false_state = state;
            for arg in false_args {
                consume_operand(arg, func, temp_origins, &mut false_state, &mut None, false);
            }
            edges.push((*if_true, true_state));
            edges.push((*if_false, false_state));
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            check_operand_read(discriminant, func, temp_origins, &state, &mut None);
            for (_, target, args) in targets {
                let mut edge = state.clone();
                for arg in args {
                    consume_operand(arg, func, temp_origins, &mut edge, &mut None, false);
                }
                edges.push((*target, edge));
            }
            let (target, args) = otherwise;
            let mut edge = state;
            for arg in args {
                consume_operand(arg, func, temp_origins, &mut edge, &mut None, false);
            }
            edges.push((*target, edge));
        }
        AmirTerminator::Suspend {
            future,
            resume,
            args,
        } => {
            check_operand_read(future, func, temp_origins, &state, &mut None);
            for arg in args {
                consume_operand(arg, func, temp_origins, &mut state, &mut None, false);
            }
            edges.push((*resume, state));
        }
    }
    edges
}

pub(super) fn apply_block(
    block: crate::amir::BlockId,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    state: &mut MoveState,
    mut diagnostics: MoveDiagSink<'_>,
) {
    for stmt in func.block_stmts(block) {
        apply_stmt(stmt, func, temp_origins, state, &mut diagnostics);
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

pub(super) fn apply_stmt(
    stmt: &AmirStmt,
    func: &AmirFunc,
    temp_origins: &[Option<AmirPlace>],
    state: &mut MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    match stmt {
        AmirStmt::Assign { rhs, .. } => {
            check_rvalue_reads(rhs, func, state, diagnostics);
            consume_rvalue(rhs, func, temp_origins, state, diagnostics);
        }
        AmirStmt::Store { lhs, rhs } => {
            if !lhs.projections.is_empty() {
                check_place_state(lhs, state.place_base_state(lhs), func, diagnostics);
            }
            consume_operand(rhs, func, temp_origins, state, diagnostics, false);
            state.restore_place(lhs);
        }
        AmirStmt::Call { callee, args, .. } => {
            consume_operand(callee, func, temp_origins, state, diagnostics, false);
            for arg in args {
                consume_operand(arg, func, temp_origins, state, diagnostics, false);
            }
        }
        AmirStmt::Free(op) => consume_operand(op, func, temp_origins, state, diagnostics, true),
        AmirStmt::Destroy(place) => {
            let recursive_root_cleanup = place.projections.is_empty()
                && state.root_state(place.local) == LocalMoveState::Available
                && state.has_moved_descendant(place);
            if !recursive_root_cleanup {
                check_consume_place(place, func, state, diagnostics, true);
            }
            state.move_place(place);
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
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
    match rvalue {
        AmirRvalue::Len(op)
        | AmirRvalue::SliceData(op)
        | AmirRvalue::Discriminant { value: op } => {
            check_operand_read(op, func, temp_origins, state, diagnostics);
        }
        _ => {
            for_each_rvalue_operand(rvalue, |op| {
                consume_operand(op, func, temp_origins, state, diagnostics, false);
            });
        }
    }
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
