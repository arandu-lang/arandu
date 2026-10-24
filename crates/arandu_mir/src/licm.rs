//! Loop-Invariant Code Motion (LICM) pass for AMIR.
//!
//! Hoists computations that are loop-invariant out of their loop, so the
//! register allocator can keep them in registers across iterations instead of
//! rematerialising them from stack slots on every pass.
//!
//! ## Speculation safety
//!
//! Only nontrapping operations on Copy values move, and their operands must
//! already be available at the preheader. Divisions, shifts, loads, addresses,
//! ownership moves and calls remain in place. Each result has one definition;
//! natural-loop headers dominate their whole body. These conditions also cover
//! zero iterations and paths that do not execute a candidate's original block.

use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirStmtTable, BlockId,
    Dominators, InstrId, TempId,
};
use rustc_hash::FxHashSet;

/// A natural loop reduced to what LICM needs.
#[derive(Debug, Clone)]
pub(crate) struct LoopInfo {
    /// The single predecessor outside the loop, where hoisted code is placed.
    pub(crate) preheader: BlockId,
    /// Blocks in the loop, header first.
    pub(crate) body: Vec<BlockId>,
}

/// Applies Loop-Invariant Code Motion to `func`.
///
/// Returns `true` if any instruction was hoisted.
pub fn licm(func: &mut AmirFunc) -> bool {
    if func.blocks.is_empty() || func.stmts.is_empty() {
        return false;
    }

    let doms = Dominators::new(func);
    let order = crate::amir::reverse_post_order(func);
    let loops = discover_loops(func, &doms);

    // Smaller loops first so deep nesting is scheduled before the loops that
    // contain it; an outer pass then sees inner-hoisted values as invariant.
    let mut worklist = loops;
    worklist.sort_by_key(|l| l.body.len());

    let mut changed = false;
    for lp in &worklist {
        // Re-derive dominators between hoists: moving statements can change
        // block shapes, and stale idom data would admit unsafe candidates.
        let fresh = Dominators::new(func);
        if hoist_loop(func, lp, &fresh, &order) {
            changed = true;
        }
    }

    changed
}

/// Finds natural loops that have a unique preheader candidate.
pub(crate) fn discover_loops(func: &AmirFunc, doms: &Dominators) -> Vec<LoopInfo> {
    let mut headers: Vec<BlockId> = Vec::new();
    let mut back_edges: Vec<(BlockId, BlockId)> = Vec::new();
    for (bid, succs) in successors_of(func) {
        for succ in succs {
            // Back edge: the successor dominates this block.
            if doms.dominates(succ, bid) {
                back_edges.push((bid, succ));
                if !headers.contains(&succ) {
                    headers.push(succ);
                }
            }
        }
    }
    if headers.is_empty() {
        return Vec::new();
    }

    let mut loops = Vec::new();
    for header in headers {
        // Natural loop body: walk predecessors starting from the *back-edge
        // sources*, never from the header itself. Seeding the worklist with the
        // header would pull the preheader into the body (it is a predecessor of
        // the header), which then makes every loop look like it has no outside
        // edge and silently disables LICM.
        let mut body = vec![header];
        let mut worklist: Vec<BlockId> = back_edges
            .iter()
            .filter(|&&(_, v)| v == header)
            .map(|&(u, _)| u)
            .filter(|&u| !body.contains(&u))
            .collect();
        for &b in &worklist {
            body.push(b);
        }
        while let Some(b) = worklist.pop() {
            for &p in func.predecessors(b) {
                // `p != header` stops the walk at the header instead of
                // escaping through the preheader.
                if p != header && !body.contains(&p) {
                    body.push(p);
                    worklist.push(p);
                }
            }
        }

        let mut outside = Vec::new();
        // An irreducible region can contain paths entering the body without
        // passing the header. Hoisting would then remove their definitions.
        if body.iter().any(|&block| !doms.dominates(header, block)) {
            continue;
        }
        for &p in func.predecessors(header) {
            if !body.contains(&p) {
                outside.push(p);
            }
        }
        // Zero outside predecessors means the header *is* the entry; several
        // means there is no single edge to split into a preheader. Either way
        // we cannot place hoisted code, so skip the loop.
        if outside.len() != 1 {
            continue;
        }
        loops.push(LoopInfo {
            preheader: outside[0],
            body,
        });
    }
    loops
}

fn successors_of(func: &AmirFunc) -> Vec<(BlockId, Vec<BlockId>)> {
    (0..func.blocks.len())
        .map(|i| {
            let b = BlockId::from_usize(i);
            let succs = func.successors(b).to_vec();
            (b, succs)
        })
        .collect()
}

/// Values available at the preheader with exactly one definition outside the loop.
/// Header parameters are not available before their incoming edge executes.
pub(crate) fn compute_invariant(
    func: &AmirFunc,
    lp: &LoopInfo,
    doms: &Dominators,
) -> FxHashSet<TempId> {
    let mut definitions = vec![Vec::new(); func.temps.len()];
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            definitions[param.id.as_usize()].push(block.id);
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                definitions[lhs.as_usize()].push(block.id);
            }
        }
    }
    let mut invariant = FxHashSet::default();
    for &param in func
        .params
        .iter()
        .chain(func.receiver.iter().map(|r| &r.temp))
    {
        if definitions[param.as_usize()].is_empty() {
            invariant.insert(param);
        }
    }
    for (index, defs) in definitions.iter().enumerate() {
        if let [block] = defs.as_slice()
            && !lp.body.contains(block)
            && doms.dominates(*block, lp.preheader)
        {
            invariant.insert(TempId::from_usize(index));
        }
    }
    invariant
}

/// Whether `rv` is safe to execute earlier than it appears.
fn is_speculatable(rv: &AmirRvalue) -> bool {
    match rv {
        AmirRvalue::Use(AmirOperand::Move(_)) => false,
        AmirRvalue::Use(_) => true,
        AmirRvalue::Binary { op, .. } => matches!(
            op,
            crate::ops::BinaryOp::Add
                | crate::ops::BinaryOp::Sub
                | crate::ops::BinaryOp::Mul
                | crate::ops::BinaryOp::Equal
                | crate::ops::BinaryOp::NotEqual
                | crate::ops::BinaryOp::Lt
                | crate::ops::BinaryOp::Gt
                | crate::ops::BinaryOp::LtEqual
                | crate::ops::BinaryOp::GtEqual
                | crate::ops::BinaryOp::BitAnd
                | crate::ops::BinaryOp::BitOr
                | crate::ops::BinaryOp::BitXor
        ),
        AmirRvalue::Unary { op, .. } => {
            matches!(op, crate::ops::UnaryOp::Not | crate::ops::UnaryOp::BitNot)
        }
        // Loads, calls, allocations and projections may trap or have effects.
        _ => false,
    }
}

fn operands(rv: &AmirRvalue) -> Vec<AmirOperand> {
    match rv {
        AmirRvalue::Use(op) => vec![*op],
        AmirRvalue::Binary { left, right, .. } => vec![*left, *right],
        AmirRvalue::Unary { operand, .. } => vec![*operand],
        _ => Vec::new(),
    }
}

fn operand_is_invariant(op: &AmirOperand, invariant: &FxHashSet<TempId>) -> bool {
    match op {
        AmirOperand::Copy(t) => invariant.contains(t),
        AmirOperand::Move(_) => false,
        AmirOperand::Constant(_) | AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => true,
    }
}

fn hoist_loop(func: &mut AmirFunc, lp: &LoopInfo, doms: &Dominators, order: &[BlockId]) -> bool {
    let invariant = compute_invariant(func, lp, doms);
    let mut definitions = vec![0usize; func.temps.len()];
    for &param in func
        .params
        .iter()
        .chain(func.receiver.iter().map(|r| &r.temp))
    {
        definitions[param.as_usize()] += 1;
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            definitions[param.id.as_usize()] += 1;
        }
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                definitions[lhs.as_usize()] += 1;
            }
        }
    }

    let mut candidates: Vec<(BlockId, InstrId, TempId)> = Vec::new();
    for &bid in &lp.body {
        for stmt_id in func.block_stmt_ids(bid) {
            let Some(AmirStmt::Assign { lhs, rhs }) = func.try_stmt(stmt_id) else {
                continue;
            };
            // The result must not already be available at the preheader, and
            // the computation must be safe to run earlier than written.
            if invariant.contains(lhs)
                || definitions[lhs.as_usize()] != 1
                || !func.temps[lhs.as_usize()].is_copy
                || !is_speculatable(rhs)
            {
                continue;
            }
            // Every operand must already hold its loop-invariant value in the
            // preheader. Combined with `is_speculatable` this is what makes the
            // move safe even when the body never executes.
            if !operands(rhs)
                .iter()
                .all(|op| operand_is_invariant(op, &invariant))
            {
                continue;
            }
            candidates.push((bid, stmt_id, *lhs));
        }
    }

    if candidates.is_empty() {
        return false;
    }

    // Deterministic order keeps the transform reproducible.
    let rank = |b: &BlockId| order.iter().position(|x| x == b).unwrap_or(usize::MAX);
    candidates.sort_by_key(|(b, s, _)| (rank(b), s.as_usize()));

    let mut hoisted: Vec<AmirStmt> = Vec::new();
    let mut blanked: Vec<InstrId> = Vec::new();
    for (_, stmt_id, lhs) in &candidates {
        if let Some(AmirStmt::Assign { rhs, .. }) = func.try_stmt(*stmt_id) {
            hoisted.push(AmirStmt::Assign {
                lhs: *lhs,
                rhs: rhs.clone(),
            });
            blanked.push(*stmt_id);
        }
    }
    if hoisted.is_empty() {
        return false;
    }

    let preheader_ids: Vec<InstrId> = func.block_stmt_ids(lp.preheader).collect();
    let mut table = AmirStmtTable::new();
    let mut blocks: Vec<AmirBasicBlock> = Vec::with_capacity(func.blocks.len());

    for (bi, block) in func.blocks.iter().enumerate() {
        let bid = BlockId::from_usize(bi);
        let start = table.len();
        if bid == lp.preheader {
            for id in &preheader_ids {
                if let Some(s) = func.try_stmt(*id) {
                    table.push(s.clone());
                }
            }
            for s in &hoisted {
                table.push(s.clone());
            }
        } else {
            for id in func.block_stmt_ids(bid) {
                if blanked.contains(&id) {
                    table.push(AmirStmt::Nop);
                } else if let Some(s) = func.try_stmt(id) {
                    table.push(s.clone());
                }
            }
        }
        let len = table.len() - start;
        blocks.push(AmirBasicBlock {
            id: block.id,
            params: block.params,
            statements: crate::layout::DenseRange::new(start, len),
            terminator: block.terminator.clone(),
        });
    }

    func.stmts = table;
    func.blocks = blocks;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
    true
}
