//! CFG → structured control flow conversion for wasm32.
//!
//! Converts an AMIR CFG (in RPO) into a flat sequence of [`Op`]s that
//! model wasm structured control: `block`, `loop`, `br`, `br_if`,
//! `br_table`, `return` and `unreachable`. The translator consumes this
//! sequence and emits actual wasm instructions.
//!
//! ## Algorithm overview
//!
//! 1. Compute RPO, dominator tree, natural loop regions (header → max rank).
//! 2. For each block T that is the target of a *forward* non-fallthrough
//!    branch, compute the nearest common dominator (NCD) of all forward
//!    predecessors of T. This is the position where the landing `block`
//!    scope begins.
//! 3. Emit blocks in RPO order. At each block:
//!    - Close loop scopes whose max rank has been passed.
//!    - Open a `loop` scope if the block is a loop header.
//!    - Open `block` scopes for targets whose NCD is the current block.
//!    - Close `block` scopes when the current block is a target.
//!    - Emit the block's content.
//!    - Emit the terminator as branch ops with computed label depths.
//!
//! ## Label depth semantics
//!
//! `br N` exits `N` scopes on the control stack. For a `block`, `br` exits
//! past its `end`. For a `loop`, `br` jumps back to the loop header.
//! The depth of a branch to target T is the number of scope frames between
//! the current position and T on the scope stack.

use arandu_middle::amir::dominators::Dominators;
use arandu_middle::amir::rpo::reverse_post_order_body_first;
use arandu_middle::amir::{AmirFunc, AmirTerminator, BlockId};
use arandu_middle::{Diagnostic, diagnostics};
use rustc_hash::{FxHashMap, FxHashSet};

/// Structured control-flow op emitted by the stackifier.
///
/// Every branch op carries the operands (operand *values* + block) needed to
/// materialize the destination's block params *before* the branch is taken,
/// since wasm block params are represented as locals set by the source edge.
#[derive(Clone, Debug)]
pub enum Op {
    /// Begin a loop. `br N` to this label jumps to the loop header.
    LoopBegin { header: BlockId },
    /// End a loop scope.
    LoopEnd,
    /// Begin a block scope (landing for a forward branch).
    /// `br N` to this label exits past the matching `BlockEnd`.
    BlockBegin { label: BlockId },
    /// End a block scope. T's body starts after this op.
    BlockEnd { label: BlockId },
    /// Placeholder for a block's instructions.
    BlockContent { block: BlockId },
    /// Unconditional branch with precomputed depth. `args` set the target's
    /// block params before branching.
    Br {
        depth: u32,
        target: BlockId,
        args: Vec<AranduOperand>,
    },
    /// Conditional branch: `condition` is loaded, then if the top of the wasm
    /// stack is nonzero (or zero when `invert` is set), branch to `target`
    /// after materializing `args`.
    BrIf {
        depth: u32,
        target: BlockId,
        args: Vec<AranduOperand>,
        condition: AranduOperand,
        invert: bool,
    },
    /// Conditional equality branch (for SwitchInt non-consecutive cases):
    /// evaluates `(discriminant == value)`, then branches to `target`
    /// after materializing `args`.
    BrIfEq {
        depth: u32,
        target: BlockId,
        args: Vec<AranduOperand>,
        discriminant: AranduOperand,
        value: i128,
    },
    /// Table branch (for SwitchInt). The discriminant is loaded, then
    /// `br_table` selects the case depth. Only valid when all targets agree on
    /// the same block-arg values (asserted during stackification); `args` set
    /// the otherwise block's params.
    BrTable {
        depths: Vec<u32>,
        default_depth: u32,
        discriminant: AranduOperand,
        otherwise: BlockId,
        args: Vec<AranduOperand>,
    },
    /// Materialize a target's block params for a *fallthrough* successor edge
    /// (the successor is the next block in RPO, so no `br` is emitted).
    ArgStore {
        target: BlockId,
        args: Vec<AranduOperand>,
    },
    /// Return from the current function.
    Return,
    /// Trap / unreachable.
    Trap,
}

/// Short alias so the ops can reference AMIR operands without import churn.
type AranduOperand = arandu_middle::amir::AmirOperand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeKind {
    Loop { max_rank: usize },
    Block,
}

#[derive(Clone, Copy, Debug)]
struct StackFrame {
    label: BlockId,
    kind: ScopeKind,
}

/// Stackify the AMIR CFG into a flat list of structured [`Op`]s.
///
/// The method never fails on well-formed AMIR; an internal invariant violation
/// (empty entry block, broken dominance/scoping) is reported as an ICE.
pub fn stackify(func: &AmirFunc) -> Result<Vec<Op>, Diagnostic> {
    let mut rpo = reverse_post_order_body_first(func);
    if rpo.is_empty() {
        return Ok(Vec::new());
    }

    hoist_unreachable_blocks_before_joins(func, &mut rpo);

    let doms = Dominators::new(func);

    let rank: FxHashMap<BlockId, usize> = rpo.iter().enumerate().map(|(i, &b)| (b, i)).collect();

    // ── Natural loop regions ────────────────────────────────────────────
    // For each loop header, compute the maximum RPO rank of any block in
    // its natural loop. The loop scope spans [header_rank .. max_rank].
    let mut loop_max_rank: FxHashMap<BlockId, usize> = FxHashMap::default();
    for &header in &rpo {
        let h_rank = rank[&header];
        for &pred in func.predecessors(header) {
            // A backedge: pred is dominated by header and pred comes *after*
            // the header in RPO (in RPO the header precedes its body).
            if doms.dominates(header, pred) && rank.get(&pred).copied().unwrap_or(0) > h_rank {
                let mut max = h_rank;
                // DFS backward through predecessors to find the natural loop body.
                let mut visited: FxHashSet<BlockId> = FxHashSet::default();
                let mut stack = vec![pred];
                visited.insert(pred);
                while let Some(b) = stack.pop() {
                    let br = rank.get(&b).copied().unwrap_or(0);
                    if br > max {
                        max = br;
                    }
                    for &p in func.predecessors(b) {
                        if p != header && visited.insert(p) {
                            stack.push(p);
                        }
                    }
                }
                loop_max_rank
                    .entry(header)
                    .and_modify(|m| *m = (*m).max(max))
                    .or_insert(max);
            }
        }
    }

    // ── Forward targets and NCDs ────────────────────────────────────────
    // A block T needs a landing `block` scope if a forward *branch* reaches it
    // from a source S that is not its immediate RPO predecessor (fallthrough
    // edges do not need a label). For each such T, the landing's open position
    // is the nearest common dominator (NCD) of *all* of T's forward
    // predecessors (fallthrough predecessors included): a fallthrough edge
    // into T executes inside sibling landing scopes, so T's landing must
    // enclose those paths too or the emitted `end`s stop nesting properly.
    let mut forward_preds: FxHashMap<BlockId, Vec<BlockId>> = FxHashMap::default();
    for &b in &rpo {
        let successors = forward_targets(b, func, &rank);
        for t in successors {
            forward_preds.entry(t).or_default().push(b);
        }
    }

    // NCD of forward predecessors for each target that needs a landing.
    let mut ncd_of: FxHashMap<BlockId, BlockId> = FxHashMap::default();
    for &t in forward_preds.keys() {
        let preds = &forward_preds[&t];
        // Single forward predecessor (e.g. a deferred loop exit): land at it.
        let mut ncd = preds[0];
        for &p in &preds[1..] {
            ncd = ncd_pair(ncd, p, &doms);
        }
        ncd_of.insert(t, ncd);
    }

    // Map: NCD block → list of targets whose NCD is this block.
    let mut ncd_targets: FxHashMap<BlockId, Vec<BlockId>> = FxHashMap::default();
    for (&t, &ncd) in &ncd_of {
        ncd_targets.entry(ncd).or_default().push(t);
    }
    // Sort targets at each NCD by rank descending (outermost scope first).
    for targets in ncd_targets.values_mut() {
        targets.sort_by(|a, b| rank[b].cmp(&rank[a]));
    }

    // ── Emission ────────────────────────────────────────────────────────
    let mut ops: Vec<Op> = Vec::with_capacity(func.blocks.len() * 3);
    let mut scope_stack: Vec<StackFrame> = Vec::new();

    for (i, &b) in rpo.iter().enumerate() {
        // 1. Close dead scopes, innermost first:
        //    - the landing `block` of the current block (its content starts now),
        //    - any landing `block` whose content has already been emitted,
        //    - any loop whose body region has been fully emitted.
        loop {
            let should_close = match scope_stack.last() {
                Some(StackFrame {
                    label,
                    kind: ScopeKind::Block,
                }) => *label == b || rank.get(label).copied().unwrap_or(0) < i,
                Some(StackFrame {
                    kind: ScopeKind::Loop { max_rank },
                    ..
                }) => *max_rank < i,
                None => false,
            };
            if !should_close {
                break;
            }
            let Some(frame) = scope_stack.pop() else {
                return Err(Diagnostic::ice(
                    diagnostics::DiagCode::ICEGEN002,
                    "stackify: close phase found an empty scope stack; \
                     dominance/scoping invariants violated"
                        .to_owned(),
                    arandu_base::Span::new(0, 0, 0),
                ));
            };
            match frame.kind {
                ScopeKind::Loop { .. } => ops.push(Op::LoopEnd),
                ScopeKind::Block => ops.push(Op::BlockEnd { label: frame.label }),
            }
        }

        // 2. Open a loop scope if this block is a loop header.
        if let Some(&max_rank) = loop_max_rank.get(&b) {
            ops.push(Op::LoopBegin { header: b });
            scope_stack.push(StackFrame {
                label: b,
                kind: ScopeKind::Loop { max_rank },
            });
        }

        // 3. Open block scopes for targets whose NCD is this block.
        if let Some(targets) = ncd_targets.get(&b) {
            for &t in targets {
                ops.push(Op::BlockBegin { label: t });
                scope_stack.push(StackFrame {
                    label: t,
                    kind: ScopeKind::Block,
                });
            }
        }

        // 4. Emit block content.
        ops.push(Op::BlockContent { block: b });

        // 5. Emit terminator as branch ops.
        let block = &func.blocks[b.as_usize()];
        emit_terminator(block, &rank, &rpo, &scope_stack, &mut ops);
    }

    // Close any remaining scopes.
    while let Some(frame) = scope_stack.pop() {
        match frame.kind {
            ScopeKind::Loop { .. } => ops.push(Op::LoopEnd),
            ScopeKind::Block => ops.push(Op::BlockEnd { label: frame.label }),
        }
    }

    Ok(ops)
}

/// Keep terminal trap arms before a merge reached by their sibling arm.
///
/// A plain RPO can put the merge before an `Unreachable` default arm. That
/// makes the Wasm label scopes cross: the trap arm's landing block encloses
/// the merge body, so a branch to the merge lands after it. Moving the trap
/// immediately before the earliest sibling-reachable merge keeps the scopes
/// properly nested without changing CFG edges.
fn hoist_unreachable_blocks_before_joins(func: &AmirFunc, rpo: &mut Vec<BlockId>) {
    let mut cursor = 0;
    while cursor < rpo.len() {
        let unreachable = rpo[cursor];
        if !matches!(
            func.blocks[unreachable.as_usize()].terminator,
            AmirTerminator::Unreachable
        ) {
            cursor += 1;
            continue;
        }

        let mut candidate = None;
        for &pred in func.predecessors(unreachable) {
            let Some(pred_index) = rpo.iter().position(|&item| item == pred) else {
                continue;
            };
            let successors = func.successors(pred);
            if successors.len() < 2 {
                continue;
            }
            for &sibling in successors
                .iter()
                .filter(|&&successor| successor != unreachable)
            {
                let mut seen = FxHashSet::default();
                let mut worklist = vec![sibling];
                while let Some(block) = worklist.pop() {
                    if !seen.insert(block) {
                        continue;
                    }
                    if block != unreachable
                        && func.predecessors(block).len() > 1
                        && let Some(index) = rpo.iter().position(|&item| item == block)
                        && index > pred_index
                        && index <= cursor
                    {
                        candidate =
                            Some(candidate.map_or(index, |current: usize| current.min(index)));
                    }
                    worklist.extend(func.successors(block).iter().copied());
                }
            }
        }

        if let Some(index) = candidate {
            let block = rpo.remove(cursor);
            rpo.insert(index, block);
        } else {
            cursor += 1;
        }
    }
}

/// Emit terminator-related branch ops.
fn emit_terminator(
    block: &arandu_middle::amir::block::AmirBasicBlock,
    rank: &FxHashMap<BlockId, usize>,
    rpo: &[BlockId],
    scope_stack: &[StackFrame],
    ops: &mut Vec<Op>,
) {
    match &block.terminator {
        AmirTerminator::Return => ops.push(Op::Return),
        AmirTerminator::Unreachable => ops.push(Op::Trap),
        AmirTerminator::Goto { target, args } => {
            let next = next_rpo(*target, block.id, rpo, rank);
            if next {
                // Fallthrough: materialize block params, no branch needed.
                ops.push(Op::ArgStore {
                    target: *target,
                    args: args.clone(),
                });
            } else {
                let depth = compute_depth(scope_stack, *target);
                ops.push(Op::Br {
                    depth,
                    target: *target,
                    args: args.clone(),
                });
            }
        }
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => {
            let next_true = next_rpo(*if_true, block.id, rpo, rank);
            let next_false = next_rpo(*if_false, block.id, rpo, rank);

            match (next_true, next_false) {
                (true, false) => {
                    // Fall through to true; br_if for false (inverted cond).
                    ops.push(Op::ArgStore {
                        target: *if_true,
                        args: true_args.clone(),
                    });
                    let depth = compute_depth(scope_stack, *if_false);
                    ops.push(Op::BrIf {
                        depth,
                        target: *if_false,
                        args: false_args.clone(),
                        condition: *condition,
                        invert: true,
                    });
                }
                (false, true) => {
                    // Fall through to false; br_if for true (direct cond).
                    ops.push(Op::ArgStore {
                        target: *if_false,
                        args: false_args.clone(),
                    });
                    let depth = compute_depth(scope_stack, *if_true);
                    ops.push(Op::BrIf {
                        depth,
                        target: *if_true,
                        args: true_args.clone(),
                        condition: *condition,
                        invert: false,
                    });
                }
                (false, false) => {
                    // Neither is next: br_if(true_target), br(false_target).
                    let d_t = compute_depth(scope_stack, *if_true);
                    ops.push(Op::BrIf {
                        depth: d_t,
                        target: *if_true,
                        args: true_args.clone(),
                        condition: *condition,
                        invert: false,
                    });
                    let d_f = compute_depth(scope_stack, *if_false);
                    ops.push(Op::Br {
                        depth: d_f,
                        target: *if_false,
                        args: false_args.clone(),
                    });
                }
                (true, true) => {
                    // Both targets are the next block (impossible for distinct).
                    debug_assert_eq!(if_true, if_false);
                    ops.push(Op::ArgStore {
                        target: *if_true,
                        args: true_args.clone(),
                    });
                }
            }
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            emit_switch_ops(discriminant, targets, otherwise, scope_stack, ops);
        }
        AmirTerminator::Suspend { resume, args, .. } => {
            let depth = compute_depth(scope_stack, *resume);
            ops.push(Op::Br {
                depth,
                target: *resume,
                args: args.clone(),
            });
        }
    }
}

/// Emit a SwitchInt as either a `br_table` (consecutive 0-based cases) or
/// a chain of `BrIf` + fallback `Br`.
fn emit_switch_ops(
    discriminant: &arandu_middle::amir::AmirOperand,
    targets: &[(i128, BlockId, Vec<arandu_middle::amir::AmirOperand>)],
    otherwise: &(BlockId, Vec<arandu_middle::amir::AmirOperand>),
    scope_stack: &[StackFrame],
    ops: &mut Vec<Op>,
) {
    // Check if cases are consecutive 0..N-1 for br_table and that all targets
    // (including otherwise) have no block arguments, since wasm br_table cannot
    // execute per-target argument stores before branching.
    let all_args_empty = targets.iter().all(|(_, _, a)| a.is_empty()) && otherwise.1.is_empty();
    let mut sorted: Vec<(i128, BlockId)> = targets.iter().map(|&(v, t, _)| (v, t)).collect();
    sorted.sort_by_key(|&(v, _)| v);

    let is_consecutive = sorted.iter().enumerate().all(|(i, &(v, _))| v == i as i128);

    if is_consecutive && sorted.len() >= 2 && all_args_empty {
        let default_depth = compute_depth(scope_stack, otherwise.0);
        let depths: Vec<u32> = sorted
            .iter()
            .map(|&(_, t)| compute_depth(scope_stack, t))
            .collect();
        ops.push(Op::BrTable {
            depths,
            default_depth,
            discriminant: *discriminant,
            otherwise: otherwise.0,
            args: otherwise.1.clone(),
        });
    } else {
        // Chain of BrIfEq + fallback Br.
        for (val, target, args) in targets {
            let depth = compute_depth(scope_stack, *target);
            ops.push(Op::BrIfEq {
                depth,
                target: *target,
                args: args.clone(),
                discriminant: *discriminant,
                value: *val,
            });
        }
        let depth = compute_depth(scope_stack, otherwise.0);
        ops.push(Op::Br {
            depth,
            target: otherwise.0,
            args: otherwise.1.clone(),
        });
    }
}

/// Compute the branch depth for a target by counting how many scope frames
/// are above it in the stack.
///
/// If the target has no matching scope, branches out of every scope (depth =
/// `stack.len()`), which is unreachable for well-formed AMIR — `validate_amir`
/// guarantees every branch target is a labeled scope.
fn compute_depth(stack: &[StackFrame], target: BlockId) -> u32 {
    stack
        .iter()
        .rev()
        .position(|f| f.label == target)
        .unwrap_or(stack.len()) as u32
}

/// Whether `target` is the immediate successor of `source` in RPO.
fn next_rpo(
    target: BlockId,
    source: BlockId,
    _rpo: &[BlockId],
    rank: &FxHashMap<BlockId, usize>,
) -> bool {
    let &sr = &rank[&source];
    let &tr = &rank[&target];
    tr == sr + 1
}

/// Forward successor blocks of `b` in the CFG (rank > rank(b)).
fn forward_targets(b: BlockId, func: &AmirFunc, rank: &FxHashMap<BlockId, usize>) -> Vec<BlockId> {
    let b_rank = rank[&b];
    func.successors(b)
        .iter()
        .copied()
        .filter(|&t| rank.get(&t).copied().unwrap_or(0) > b_rank)
        .collect()
}

/// Nearest common dominator of two blocks.
fn ncd_pair(a: BlockId, b: BlockId, doms: &Dominators) -> BlockId {
    if a == b {
        return a;
    }
    // Collect a's ancestor chain.
    let mut a_ancestors = Vec::new();
    let mut cur = a;
    a_ancestors.push(cur);
    while let Some(idom) = doms.immediate_dominator(cur) {
        if idom == cur {
            break;
        }
        a_ancestors.push(idom);
        cur = idom;
    }
    // Walk b's ancestor chain until hitting one of a's ancestors.
    let mut cur = b;
    loop {
        if a_ancestors.contains(&cur) {
            return cur;
        }
        match doms.immediate_dominator(cur) {
            Some(next) if next != cur => cur = next,
            _ => break,
        }
    }
    // Fallback — shouldn't happen for well-formed reducible CFGs.
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_func_produces_empty_ops() {
        use arandu_middle::SymbolId;
        use arandu_middle::amir::stmt::AmirStmtTable;
        use arandu_middle::cfg::compute_cfg_edges;
        use arandu_middle::types::{ArType, TypeInterner};

        let interner = TypeInterner::new();
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: interner.intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: Vec::new(),
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg: compute_cfg_edges(&[]),
        };
        let ops = stackify(&func).unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn single_block_return() {
        use arandu_middle::SymbolId;
        use arandu_middle::amir::block::{AmirBasicBlock, BlockId};
        use arandu_middle::amir::stmt::{AmirStmtTable, AmirTerminator};
        use arandu_middle::cfg::compute_cfg_edges;
        use arandu_middle::layout::DenseRange;
        use arandu_middle::types::{ArType, TypeInterner};

        let interner = TypeInterner::new();
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: interner.intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks,
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg,
        };
        let ops = stackify(&func).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], Op::BlockContent { block } if block == BlockId::from_usize(0)));
        assert!(matches!(ops[1], Op::Return));
    }
}
