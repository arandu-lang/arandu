//! CFG propagation of holder states and reference live range tracking.

use super::paths::{merge_local_paths, merge_temp_paths, place_path, prefix_paths};
use super::types::{BorrowState, HolderPath, Loan};
use crate::BitSet;
use crate::amir::{AmirFunc, AmirOperand, AmirStmt, AmirTerminator, BlockId, LocalId, TempId};
use std::collections::{BTreeSet, VecDeque};

struct ActiveLoan {
    loan_index: usize,
    locals: Vec<LocalId>,
}

/// Forward may-analysis for stack carriers. Unlike the global holder index,
/// this state models overwrites: assigning one projection kills only that
/// subtree, while joins union the paths arriving from each predecessor.
pub(crate) fn analyze_local_holder_states(
    func: &AmirFunc,
    loans: &[Loan],
) -> (Vec<Vec<Vec<BitSet<LocalId>>>>, bool) {
    let active_loans: Vec<ActiveLoan> = loans
        .iter()
        .enumerate()
        .filter_map(|(i, loan)| {
            if loan.holder_locals.is_empty() {
                None
            } else {
                Some(ActiveLoan {
                    loan_index: i,
                    locals: loan.holder_locals.iter().collect(),
                })
            }
        })
        .collect();

    if active_loans.is_empty() {
        let empty_bits = vec![BitSet::with_capacity(func.locals.len()); loans.len()];
        let points = func
            .blocks
            .iter()
            .map(|block| vec![empty_bits.clone(); 1 + func.block_stmts(block.id).count()])
            .collect();
        return (points, false);
    }

    let empty_state = || {
        active_loans
            .iter()
            .map(|active| vec![BTreeSet::new(); active.locals.len()])
            .collect::<Vec<Vec<BTreeSet<HolderPath>>>>()
    };
    let mut block_in = vec![empty_state(); func.blocks.len()];
    let mut block_out = vec![empty_state(); func.blocks.len()];
    let mut queue = (0..func.blocks.len())
        .map(BlockId::from_usize)
        .collect::<VecDeque<_>>();
    let mut queued = vec![true; func.blocks.len()];

    let mut guard = 0;
    let mut exceeded_limit = false;
    while let Some(block_id) = queue.pop_front() {
        guard += 1;
        if guard >= crate::analysis_limits::BORROW_FACTS_ITERATION_GUARD {
            exceeded_limit = true;
            break;
        }
        let bi = block_id.as_usize();
        queued[bi] = false;
        let mut state = block_in[bi].clone();
        for stmt in func.block_stmts(block_id) {
            transfer_local_holders(stmt, &active_loans, loans, &mut state);
        }
        if state == block_out[bi] {
            continue;
        }
        block_out[bi] = state.clone();
        let block = &func.blocks[bi];
        for (target, args) in terminator_edges(&block.terminator) {
            let mut edge = state.clone();
            if let Some(successor) = func.blocks.get(target.as_usize()) {
                let succ_params = func.block_params(successor.params);
                for (parameter, argument) in succ_params.iter().zip(args) {
                    let source = operand_temp(argument);
                    for (active_idx, active) in active_loans.iter().enumerate() {
                        if let Some(pos) = active.locals.iter().position(|&l| l == parameter.local)
                        {
                            let loan = &loans[active.loan_index];
                            let paths = source
                                .and_then(|temp| loan.holder_temp_paths.get(temp.as_usize()))
                                .cloned()
                                .unwrap_or_default();
                            edge[active_idx][pos] = paths;
                        }
                    }
                }
            }
            if merge_local_state(&mut block_in[target.as_usize()], &edge)
                && !queued[target.as_usize()]
            {
                queue.push_back(target);
                queued[target.as_usize()] = true;
            }
        }
    }

    let mut points = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut state = block_in[block.id.as_usize()].clone();
        let mut block_points = Vec::with_capacity(1 + func.block_stmts(block.id).count());
        block_points.push(holder_bits(
            &state,
            &active_loans,
            loans.len(),
            func.locals.len(),
        ));
        for stmt in func.block_stmts(block.id) {
            transfer_local_holders(stmt, &active_loans, loans, &mut state);
            block_points.push(holder_bits(
                &state,
                &active_loans,
                loans.len(),
                func.locals.len(),
            ));
        }
        points.push(block_points);
    }
    (points, exceeded_limit)
}

fn transfer_local_holders(
    stmt: &AmirStmt,
    active_loans: &[ActiveLoan],
    loans: &[Loan],
    state: &mut [Vec<BTreeSet<HolderPath>>],
) {
    match stmt {
        AmirStmt::Store { lhs, rhs } => {
            let destination = place_path(lhs);
            let source = operand_temp(rhs);
            for (active_idx, active) in active_loans.iter().enumerate() {
                if let Some(pos) = active.locals.iter().position(|&l| l == lhs.local) {
                    let local = &mut state[active_idx][pos];
                    local.retain(|path| !path.0.starts_with(&destination.0));
                    if let Some(source) = source {
                        let loan = &loans[active.loan_index];
                        if let Some(paths) = loan.holder_temp_paths.get(source.as_usize())
                            && !paths.is_empty()
                        {
                            local.extend(prefix_paths(paths, &destination));
                        }
                    }
                }
            }
        }
        AmirStmt::StorageDead(local) => {
            for (active_idx, active) in active_loans.iter().enumerate() {
                if let Some(pos) = active.locals.iter().position(|&l| l == *local) {
                    state[active_idx][pos].clear();
                }
            }
        }
        AmirStmt::Assign { .. }
        | AmirStmt::Call { .. }
        | AmirStmt::Free(_)
        | AmirStmt::StorageLive(_)
        | AmirStmt::Destroy(_)
        | AmirStmt::Nop => {}
    }
}

fn merge_local_state(
    target: &mut [Vec<BTreeSet<HolderPath>>],
    source: &[Vec<BTreeSet<HolderPath>>],
) -> bool {
    let mut changed = false;
    for (target_loan, source_loan) in target.iter_mut().zip(source) {
        for (target_local, source_local) in target_loan.iter_mut().zip(source_loan) {
            let old_len = target_local.len();
            target_local.extend(source_local.iter().cloned());
            changed |= target_local.len() != old_len;
        }
    }
    changed
}

fn holder_bits(
    state: &[Vec<BTreeSet<HolderPath>>],
    active_loans: &[ActiveLoan],
    num_loans: usize,
    num_locals: usize,
) -> Vec<BitSet<LocalId>> {
    let mut bits = vec![BitSet::with_capacity(num_locals); num_loans];
    for (active_idx, active) in active_loans.iter().enumerate() {
        let loan_bits = &mut bits[active.loan_index];
        for (pos, &local) in active.locals.iter().enumerate() {
            if !state[active_idx][pos].is_empty() {
                loan_bits.insert(local);
            }
        }
    }
    bits
}

fn terminator_edges(terminator: &AmirTerminator) -> Vec<(BlockId, &[AmirOperand])> {
    match terminator {
        AmirTerminator::Goto { target, args } => vec![(*target, args)],
        AmirTerminator::Suspend { resume, args, .. } => vec![(*resume, args)],
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => vec![(*if_true, true_args), (*if_false, false_args)],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut edges = targets
                .iter()
                .map(|(_, target, args)| (*target, args.as_slice()))
                .collect::<Vec<_>>();
            edges.push((otherwise.0, otherwise.1.as_slice()));
            edges
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => Vec::new(),
    }
}

pub(crate) fn propagate_terminator_args(
    func: &AmirFunc,
    target: BlockId,
    args: &[AmirOperand],
    loans: &mut [Loan],
    changed: &mut bool,
) {
    let Some(tb) = func.blocks.get(target.as_usize()) else {
        return;
    };
    for (i, arg) in args.iter().enumerate() {
        let Some(src) = operand_temp(arg) else {
            continue;
        };
        let Some(param) = func.block_params(tb.params).get(i) else {
            continue;
        };
        for loan in loans.iter_mut() {
            let paths = loan
                .holder_temp_paths
                .get(src.as_usize())
                .cloned()
                .unwrap_or_default();
            *changed |= merge_temp_paths(loan, param.id, paths.clone());
            // Block params often alias a local.
            *changed |= merge_local_paths(loan, param.local, paths);
        }
    }
}

pub(crate) fn operand_temp(op: &AmirOperand) -> Option<TempId> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => Some(*t),
        _ => None,
    }
}

pub(crate) fn state_from_live_holders(
    loans: &[Loan],
    num_locals: usize,
    temp_live: &BitSet<TempId>,
    local_live: &BitSet<LocalId>,
) -> BorrowState {
    let mut st = BorrowState::new(num_locals);
    for loan in loans {
        let temp_active = loan.holder_temps.iter().any(|t| temp_live.contains(t));
        let local_active = loan.holder_locals.iter().any(|l| local_live.contains(l));
        if temp_active || local_active {
            st.activate(loan);
        }
    }
    st
}
