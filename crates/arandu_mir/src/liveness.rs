//! Intraprocedural liveness analysis (locals + SSA temps).
//!
//! - [`analyze_local_liveness`]: stack locals (register allocation / OSSA).
//! - [`analyze_temp_liveness`]: SSA temps — **F2.2** reuses this so a loan's
//!   window equals the live range of the reference value that holds it.

use std::collections::VecDeque;

use crate::amir::reachability::terminator_targets;
use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator,
    BlockId, LocalId, TempId, for_each_rvalue_operand, for_each_rvalue_place,
    for_each_terminator_operand,
};
use crate::{BitMatrix, BitSet};

/// Temporary interference, including simultaneous successor arguments.
/// Backends choose representation classes and reserve addressable/ABI storage.
#[must_use]
pub fn temp_interference_graph(func: &AmirFunc) -> Vec<BitSet<TempId>> {
    let num_temps = func.temps.len();
    let mut interference = vec![BitSet::<TempId>::with_capacity(num_temps); num_temps];
    if num_temps == 0 {
        return interference;
    }

    let temp_liveness = analyze_temp_liveness(func);

    for block in &func.blocks {
        let mut live = temp_liveness.live_out(block.id).clone();

        // 1. All variables live at block exit interfere with one another.
        let live_vec: Vec<TempId> = live.iter().collect();
        for i in 0..live_vec.len() {
            let u = live_vec[i];
            let u_idx = u.as_usize();
            if u_idx >= num_temps {
                continue;
            }
            for &v in &live_vec[(i + 1)..] {
                let v_idx = v.as_usize();
                if v_idx < num_temps {
                    interference[u_idx].insert(v);
                    interference[v_idx].insert(u);
                }
            }
        }

        // 2. Successor block parameters are materialized before jumping; they
        // interfere with all values surviving across the edge.
        for succ in terminator_targets(&block.terminator) {
            let succ_params = func.block_params(func.blocks[succ.as_usize()].params);
            for p in succ_params {
                let p_idx = p.id.as_usize();
                if p_idx < num_temps {
                    for u in live.iter() {
                        let u_idx = u.as_usize();
                        if u_idx != p_idx && u_idx < num_temps {
                            interference[p_idx].insert(u);
                            interference[u_idx].insert(p.id);
                        }
                    }
                }
            }
        }

        // 3. Terminator operands are live uses at the terminator point.
        for_each_terminator_operand(&block.terminator, |op| {
            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                let t_idx = t.as_usize();
                if t_idx < num_temps {
                    for u in live.iter() {
                        let u_idx = u.as_usize();
                        if u_idx != t_idx && u_idx < num_temps {
                            interference[t_idx].insert(u);
                            interference[u_idx].insert(*t);
                        }
                    }
                    live.insert(*t);
                }
            }
        });

        // 4. Traverse statements in reverse order.
        let stmts: Vec<&AmirStmt> = func.block_stmts(block.id).collect();
        for stmt in stmts.into_iter().rev() {
            let def = match stmt {
                AmirStmt::Assign { lhs, .. } => Some(*lhs),
                AmirStmt::Call { lhs: Some(t), .. } => Some(*t),
                _ => None,
            };

            if let Some(def_temp) = def {
                let def_idx = def_temp.as_usize();
                if def_idx < num_temps {
                    for u in live.iter() {
                        let u_idx = u.as_usize();
                        if u_idx != def_idx && u_idx < num_temps {
                            interference[def_idx].insert(u);
                            interference[u_idx].insert(def_temp);
                        }
                    }
                    live.remove(def_temp);
                }
            }

            crate::amir::visit::for_each_stmt_operand(stmt, |op| {
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                    let t_idx = t.as_usize();
                    if t_idx < num_temps {
                        for u in live.iter() {
                            let u_idx = u.as_usize();
                            if u_idx != t_idx && u_idx < num_temps {
                                interference[t_idx].insert(u);
                                interference[u_idx].insert(*t);
                            }
                        }
                        live.insert(*t);
                    }
                }
            });
        }

        // 5. Block parameters are defined at block entry; they interfere with
        // one another and with any value that remains live across the entry.
        let block_params: Vec<TempId> = func
            .block_params(block.params)
            .iter()
            .map(|p| p.id)
            .collect();
        for i in 0..block_params.len() {
            let p = block_params[i];
            let p_idx = p.as_usize();
            if p_idx >= num_temps {
                continue;
            }
            for u in live.iter() {
                let u_idx = u.as_usize();
                if u_idx != p_idx && u_idx < num_temps {
                    interference[p_idx].insert(u);
                    interference[u_idx].insert(p);
                }
            }
            for &q in &block_params[(i + 1)..] {
                let q_idx = q.as_usize();
                if q_idx < num_temps {
                    interference[p_idx].insert(q);
                    interference[q_idx].insert(p);
                }
            }
            live.remove(p);
        }
    }

    interference
}

/// Liveness query results for all local variables within a single function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLiveness {
    live_in: Vec<BitSet<LocalId>>,
    live_out: Vec<BitSet<LocalId>>,
    /// Locals live immediately before each statement and the terminator.
    /// The final slot in each block is the point before its terminator.
    live_before: Vec<Vec<BitSet<LocalId>>>,
}

impl LocalLiveness {
    /// Returns the set of local variables that are live at the entry of the given block.
    #[must_use]
    pub fn live_in(&self, block: BlockId) -> &BitSet<LocalId> {
        &self.live_in[block.as_usize()]
    }

    /// Returns the set of local variables that are live at the exit of the given block.
    #[must_use]
    pub fn live_out(&self, block: BlockId) -> &BitSet<LocalId> {
        &self.live_out[block.as_usize()]
    }

    /// Returns the locals live immediately before a statement, or before the
    /// terminator when `stmt_index` equals the block's statement count.
    #[must_use]
    pub fn live_before(&self, block: BlockId, stmt_index: usize) -> Option<&BitSet<LocalId>> {
        self.live_before
            .get(block.as_usize())
            .and_then(|points| points.get(stmt_index))
    }
}

/// Liveness of SSA temps (per-block live-in / live-out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempLiveness {
    live_in: Vec<BitSet<TempId>>,
    live_out: Vec<BitSet<TempId>>,
}

impl TempLiveness {
    #[must_use]
    pub fn live_in(&self, block: BlockId) -> &BitSet<TempId> {
        &self.live_in[block.as_usize()]
    }

    #[must_use]
    pub fn live_out(&self, block: BlockId) -> &BitSet<TempId> {
        &self.live_out[block.as_usize()]
    }
}

/// Runs intraprocedural liveness analysis for all local variables in the function.
///
/// Uses a backward dataflow analysis over the CFG.
#[must_use]
pub fn analyze_local_liveness(func: &AmirFunc) -> LocalLiveness {
    let num_blocks = func.blocks.len();
    let num_locals = func.locals.len();
    // The empty variable domain has exactly one solution. Walking a large
    // CFG cannot change it and used to exhaust the convergence guard even
    // for valid functions with no locals.
    if num_locals == 0 {
        return LocalLiveness {
            live_in: vec![BitSet::new(); num_blocks],
            live_out: vec![BitSet::new(); num_blocks],
            live_before: func
                .blocks
                .iter()
                .map(|block| vec![BitSet::new(); func.block_stmts(block.id).count() + 1])
                .collect(),
        };
    }
    let mut block_uses = BitMatrix::<BlockId, LocalId>::new(num_blocks, num_locals);
    let mut block_defs = BitMatrix::<BlockId, LocalId>::new(num_blocks, num_locals);

    for block in &func.blocks {
        let mut defined = BitSet::<LocalId>::with_capacity(num_locals);
        for stmt in func.block_stmts(block.id) {
            collect_stmt_uses(stmt, &defined, &mut block_uses, block.id);
            collect_stmt_defs(stmt, &mut defined, &mut block_defs, block.id);
        }
        collect_terminator_uses(&block.terminator, &defined, &mut block_uses, block.id);
    }

    let mut live_in = vec![BitSet::<LocalId>::with_capacity(num_locals); num_blocks];
    let mut live_out = vec![BitSet::<LocalId>::with_capacity(num_locals); num_blocks];

    let rpo = crate::amir::reverse_post_order(func);

    let mut in_worklist = vec![false; num_blocks];
    let mut worklist = VecDeque::new();
    for &block_id in rpo.iter().rev() {
        worklist.push_back(block_id);
        in_worklist[block_id.as_usize()] = true;
    }

    let mut new_out = BitSet::<LocalId>::with_capacity(num_locals);
    let mut new_in = BitSet::<LocalId>::with_capacity(num_locals);

    let max_iterations =
        num_blocks * num_locals + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;
    let mut iterations = 0;

    while let Some(block_id) = worklist.pop_front() {
        iterations += 1;
        let index = block_id.as_usize();
        in_worklist[index] = false;

        assert!(
            iterations <= max_iterations,
            "liveness analysis failed to converge within theoretical limit: {iterations} > {max_iterations}"
        );

        let block = &func.blocks[index];

        new_out.clear();
        for successor in terminator_targets(&block.terminator) {
            new_out.union_with(&live_in[successor.as_usize()]);
        }

        new_in.clone_from(&new_out);
        new_in.difference_with(&block_defs.row_set(block_id));
        new_in.union_with(&block_uses.row_set(block_id));

        if new_in != live_in[index] || new_out != live_out[index] {
            live_in[index].clone_from(&new_in);
            live_out[index].clone_from(&new_out);

            for &pred in func.predecessors(block_id) {
                let pred_index = pred.as_usize();
                if !in_worklist[pred_index] {
                    worklist.push_back(pred);
                    in_worklist[pred_index] = true;
                }
            }
        }
    }

    let mut live_before = Vec::with_capacity(num_blocks);
    for block in &func.blocks {
        let stmts = func.block_stmts(block.id).collect::<Vec<_>>();
        let mut points = vec![BitSet::<LocalId>::with_capacity(num_locals); stmts.len() + 1];
        let mut live = live_out[block.id.as_usize()].clone();
        let empty_defined = BitSet::<LocalId>::with_capacity(num_locals);
        let mut uses = BitMatrix::<BlockId, LocalId>::new(1, num_locals);
        let mut defs = BitMatrix::<BlockId, LocalId>::new(1, num_locals);
        let mut stmt_defs = BitSet::<LocalId>::with_capacity(num_locals);
        collect_terminator_uses(
            &block.terminator,
            &empty_defined,
            &mut uses,
            BlockId::from_usize(0),
        );
        live.union_with(&uses.row_set(BlockId::from_usize(0)));
        points[stmts.len()].clone_from(&live);
        for (index, stmt) in stmts.iter().enumerate().rev() {
            uses.clear_row(BlockId::from_usize(0));
            defs.clear_row(BlockId::from_usize(0));
            stmt_defs.clear();
            collect_stmt_uses(stmt, &empty_defined, &mut uses, BlockId::from_usize(0));
            collect_stmt_defs(stmt, &mut stmt_defs, &mut defs, BlockId::from_usize(0));
            live.difference_with(&defs.row_set(BlockId::from_usize(0)));
            live.union_with(&uses.row_set(BlockId::from_usize(0)));
            points[index].clone_from(&live);
        }
        live_before.push(points);
    }

    LocalLiveness {
        live_in,
        live_out,
        live_before,
    }
}

/// Backward dataflow: which SSA temps are live-in / live-out per block (F2.2).
#[must_use]
pub fn analyze_temp_liveness(func: &AmirFunc) -> TempLiveness {
    let num_blocks = func.blocks.len();
    let num_temps = func.temps.len();
    if num_temps == 0 {
        return TempLiveness {
            live_in: vec![BitSet::new(); num_blocks],
            live_out: vec![BitSet::new(); num_blocks],
        };
    }
    let mut block_uses = BitMatrix::<BlockId, TempId>::new(num_blocks, num_temps);
    let mut block_defs = BitMatrix::<BlockId, TempId>::new(num_blocks, num_temps);

    for block in &func.blocks {
        let mut defined = BitSet::<TempId>::with_capacity(num_temps);
        // Block params are defs at entry (before body uses).
        for param in func.block_params(block.params) {
            defined.insert(param.id);
            block_defs.insert(block.id, param.id);
        }
        for stmt in func.block_stmts(block.id) {
            collect_stmt_temp_uses(stmt, &defined, &mut block_uses, block.id);
            collect_stmt_temp_defs(stmt, &mut defined, &mut block_defs, block.id);
        }
        collect_terminator_temp_uses(&block.terminator, &defined, &mut block_uses, block.id);
    }

    let mut live_in = vec![BitSet::<TempId>::with_capacity(num_temps); num_blocks];
    let mut live_out = vec![BitSet::<TempId>::with_capacity(num_temps); num_blocks];

    let rpo = crate::amir::reverse_post_order(func);

    let mut in_worklist = vec![false; num_blocks];
    let mut worklist = VecDeque::new();
    for &block_id in rpo.iter().rev() {
        worklist.push_back(block_id);
        in_worklist[block_id.as_usize()] = true;
    }

    let mut new_out = BitSet::<TempId>::with_capacity(num_temps);
    let mut new_in = BitSet::<TempId>::with_capacity(num_temps);

    let max_iterations =
        num_blocks * num_temps + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;
    let mut iterations = 0;

    while let Some(block_id) = worklist.pop_front() {
        iterations += 1;
        let index = block_id.as_usize();
        in_worklist[index] = false;

        assert!(
            iterations <= max_iterations,
            "liveness analysis failed to converge within theoretical limit: {iterations} > {max_iterations}"
        );

        let block = &func.blocks[index];

        new_out.clear();
        for successor in terminator_targets(&block.terminator) {
            new_out.union_with(&live_in[successor.as_usize()]);
        }

        new_in.clone_from(&new_out);
        new_in.difference_with(&block_defs.row_set(block_id));
        new_in.union_with(&block_uses.row_set(block_id));

        if new_in != live_in[index] || new_out != live_out[index] {
            live_in[index].clone_from(&new_in);
            live_out[index].clone_from(&new_out);

            for &pred in func.predecessors(block_id) {
                let pred_index = pred.as_usize();
                if !in_worklist[pred_index] {
                    worklist.push_back(pred);
                    in_worklist[pred_index] = true;
                }
            }
        }
    }

    TempLiveness { live_in, live_out }
}

fn collect_stmt_temp_uses(
    stmt: &AmirStmt,
    defined: &BitSet<TempId>,
    uses: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    match stmt {
        AmirStmt::Assign { rhs, .. } => {
            for_each_rvalue_operand(rhs, |op| mark_temp_use(op, defined, uses, block));
            for_each_rvalue_place(rhs, |place| {
                for proj in &place.projections {
                    if let AmirProjection::Index(op) = proj {
                        mark_temp_use(op, defined, uses, block);
                    }
                }
            });
        }
        AmirStmt::Store { lhs, rhs } => {
            mark_temp_use(rhs, defined, uses, block);
            for proj in &lhs.projections {
                if let AmirProjection::Index(op) = proj {
                    mark_temp_use(op, defined, uses, block);
                }
            }
        }
        AmirStmt::Call { callee, args, .. } => {
            mark_temp_use(callee, defined, uses, block);
            for arg in args {
                mark_temp_use(arg, defined, uses, block);
            }
        }
        AmirStmt::Free(op) => mark_temp_use(op, defined, uses, block),
        AmirStmt::Destroy(place) => {
            for proj in &place.projections {
                if let AmirProjection::Index(op) = proj {
                    mark_temp_use(op, defined, uses, block);
                }
            }
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn collect_stmt_temp_defs(
    stmt: &AmirStmt,
    defined: &mut BitSet<TempId>,
    defs: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    match stmt {
        AmirStmt::Assign { lhs, .. } => {
            defined.insert(*lhs);
            defs.insert(block, *lhs);
        }
        AmirStmt::Call { lhs: Some(t), .. } => {
            defined.insert(*t);
            defs.insert(block, *t);
        }
        _ => {}
    }
}

fn collect_terminator_temp_uses(
    term: &AmirTerminator,
    defined: &BitSet<TempId>,
    uses: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    // Shared visitor: conditions + all jump args (same contract as DCE).
    for_each_terminator_operand(term, |op| mark_temp_use(op, defined, uses, block));
}

fn mark_temp_use(
    op: &AmirOperand,
    defined: &BitSet<TempId>,
    uses: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
        && !defined.contains(*t)
    {
        uses.insert(block, *t);
    }
}

fn collect_stmt_uses(
    stmt: &AmirStmt,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    match stmt {
        AmirStmt::Assign { rhs, .. } => collect_rvalue_uses(rhs, defined, uses, block),
        AmirStmt::Store { lhs, rhs } => {
            if !lhs.projections.is_empty() {
                collect_place_use(lhs, defined, uses, block);
            } else {
                collect_projection_uses(lhs, defined, uses, block);
            }
            collect_operand_uses(rhs, defined, uses, block);
        }
        AmirStmt::Call { callee, args, .. } => {
            collect_operand_uses(callee, defined, uses, block);
            for arg in args {
                collect_operand_uses(arg, defined, uses, block);
            }
        }
        AmirStmt::Free(op) => collect_operand_uses(op, defined, uses, block),
        AmirStmt::Destroy(place) => collect_place_use(place, defined, uses, block),
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn collect_stmt_defs(
    stmt: &AmirStmt,
    defined: &mut BitSet<LocalId>,
    defs: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    if let AmirStmt::Store { lhs, .. } = stmt
        && lhs.projections.is_empty()
    {
        defined.insert(lhs.local);
        defs.insert(block, lhs.local);
    } else if let AmirStmt::StorageDead(local) = stmt {
        defined.insert(*local);
        defs.insert(block, *local);
    }
}

fn collect_terminator_uses(
    term: &AmirTerminator,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    match term {
        AmirTerminator::Branch { condition, .. } => {
            collect_operand_uses(condition, defined, uses, block);
        }
        AmirTerminator::SwitchInt { discriminant, .. } => {
            collect_operand_uses(discriminant, defined, uses, block);
        }
        AmirTerminator::Suspend { future, args, .. } => {
            collect_operand_uses(future, defined, uses, block);
            for a in args {
                collect_operand_uses(a, defined, uses, block);
            }
        }
        AmirTerminator::Return | AmirTerminator::Goto { .. } | AmirTerminator::Unreachable => {}
    }
}

fn collect_rvalue_uses(
    rvalue: &AmirRvalue,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    // Shared visitor: places (Load/Borrow) and any nested operands (RC-ANALYSIS-LOAD).
    for_each_rvalue_place(rvalue, |place| {
        collect_place_use(place, defined, uses, block);
    });
    for_each_rvalue_operand(rvalue, |op| {
        collect_operand_uses(op, defined, uses, block);
    });
}

fn collect_place_use(
    place: &AmirPlace,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    if !defined.contains(place.local) {
        uses.insert(block, place.local);
    }
    collect_projection_uses(place, defined, uses, block);
}

fn collect_projection_uses(
    place: &AmirPlace,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    for projection in &place.projections {
        if let AmirProjection::Index(op) = projection {
            collect_operand_uses(op, defined, uses, block);
        }
    }
}

fn collect_operand_uses(
    _op: &AmirOperand,
    _defined: &BitSet<LocalId>,
    _uses: &mut BitMatrix<BlockId, LocalId>,
    _block: BlockId,
) {
}

#[cfg(test)]
#[path = "liveness_tests.rs"]
mod tests;
