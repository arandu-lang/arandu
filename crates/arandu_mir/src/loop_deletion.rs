//! Deletion of loops that provably terminate and do nothing observable.
//!
//! The conditions follow LLVM's `LoopDeletion`: a single exit edge, no value
//! defined in the loop used after it, and no side effect inside it — no store,
//! no call with effects, no operation that may trap.
//!
//! LLVM also deleted effect-free loops it could not prove finite, which broke
//! Rust programs whose `loop {}` is defined behavior (rust#28728). This pass
//! only deletes loops whose header tests `iv < bound` against an invariant
//! bound, where `iv` is a header parameter every back edge advances by one:
//! `iv < bound <= MAX` means `iv + 1` never wraps, so the loop terminates.

use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt,
    AmirTerminator, BlockId, Dominators, TempId,
};
use crate::cfg::compute_cfg_edges;
use crate::licm::{LoopInfo, compute_invariant, discover_loops};
use crate::literal_pool::{AmirLiteralEntry, AmirLiteralPool, parse_int_literal};
use crate::ops::{BinaryOp, UnaryOp};
use crate::types::{ArType, TypeInterner};
use rustc_hash::FxHashSet;

/// How far a bound's definition chain is followed inside the loop.
const MAX_BOUND_DEPTH: usize = 8;

/// Removes one dead counted loop per call; the fixpoint repeats it.
pub(crate) fn delete_dead_loops(
    func: &mut AmirFunc,
    removable_callees: &FxHashSet<crate::SymbolId>,
    literal_pool: &AmirLiteralPool,
    interner: &TypeInterner,
) -> bool {
    if func.blocks.is_empty() {
        return false;
    }
    let doms = Dominators::new(func);
    let mut loops = discover_loops(func, &doms);
    loops.sort_by_key(|lp| lp.body.len());
    let context = Context {
        removable_callees,
        literal_pool,
        interner,
    };
    for lp in &loops {
        if let Some((exit, args)) = context.deletable_exit(func, lp, &doms) {
            retarget(
                &mut func.block_mut(lp.preheader).terminator,
                lp.body[0],
                exit,
                &args,
            );
            // The loop is now unreachable; SimplifyCFG drops its blocks.
            for &block in &lp.body {
                func.block_mut(block).terminator = AmirTerminator::Unreachable;
            }
            func.cfg = compute_cfg_edges(&func.blocks);
            return true;
        }
    }
    false
}

struct Context<'a> {
    removable_callees: &'a FxHashSet<crate::SymbolId>,
    literal_pool: &'a AmirLiteralPool,
    interner: &'a TypeInterner,
}

impl Context<'_> {
    /// The exit block and its arguments when `lp` may be skipped entirely.
    fn deletable_exit(
        &self,
        func: &AmirFunc,
        lp: &LoopInfo,
        doms: &Dominators,
    ) -> Option<(BlockId, Vec<AmirOperand>)> {
        let header = lp.body[0];
        let AmirTerminator::Branch {
            condition: AmirOperand::Copy(condition),
            if_true,
            if_false: exit,
            false_args,
            ..
        } = &func.block(header).terminator
        else {
            return None;
        };
        if !lp.body.contains(if_true) || lp.body.contains(exit) {
            return None;
        }
        let defined = defined_in(func, lp);
        let invariant = compute_invariant(func, lp, doms);
        let is_invariant = |op: &AmirOperand| match op {
            AmirOperand::Copy(temp) => invariant.contains(temp),
            AmirOperand::Constant(_) => true,
            _ => false,
        };
        if !false_args.iter().all(is_invariant) {
            return None;
        }
        // Every other edge stays inside the loop, so the header test is the
        // only way out.
        for &block in &lp.body[1..] {
            match &func.block(block).terminator {
                AmirTerminator::Goto { target, .. } if lp.body.contains(target) => {}
                AmirTerminator::Branch {
                    if_true, if_false, ..
                } if lp.body.contains(if_true) && lp.body.contains(if_false) => {}
                _ => return None,
            }
        }
        if !nested_acyclic(func, lp) {
            return None;
        }
        for &block in &lp.body {
            if !func
                .block_stmts(block)
                .all(|stmt| self.is_inert(func, stmt))
            {
                return None;
            }
        }
        if used_outside(func, lp, &defined) {
            return None;
        }
        self.counts_by_one(func, lp, *condition, &is_invariant)
            .then(|| (*exit, false_args.clone()))
    }

    /// Whether the header tests `iv < bound` on an integer header parameter
    /// that every back edge advances by one.
    fn counts_by_one(
        &self,
        func: &AmirFunc,
        lp: &LoopInfo,
        condition: TempId,
        is_invariant: &impl Fn(&AmirOperand) -> bool,
    ) -> bool {
        let header = lp.body[0];
        let (iv, bound) = match definition_in(func, &[header], condition) {
            Some(AmirRvalue::Binary {
                op: BinaryOp::Lt,
                left: AmirOperand::Copy(iv),
                right,
            }) => (*iv, right),
            Some(AmirRvalue::Binary {
                op: BinaryOp::Gt,
                left,
                right: AmirOperand::Copy(iv),
            }) => (*iv, left),
            _ => return false,
        };
        if !self.recomputes_invariant(func, lp, bound, is_invariant, MAX_BOUND_DEPTH) {
            return false;
        }
        if !matches!(
            self.interner.resolve(func.temps[iv.as_usize()].ty),
            ArType::Primitive(primitive) if primitive.is_integer()
        ) {
            return false;
        }
        let Some(position) = func
            .block_params(func.block(header).params)
            .iter()
            .position(|param| param.id == iv)
        else {
            return false;
        };
        func.predecessors(header)
            .iter()
            .filter(|latch| lp.body.contains(latch))
            .all(|&latch| {
                let AmirTerminator::Goto { target, args } = &func.block(latch).terminator else {
                    return false;
                };
                let (true, Some(AmirOperand::Copy(next))) = (*target == header, args.get(position))
                else {
                    return false;
                };
                match definition_in(func, &lp.body, *next) {
                    Some(
                        AmirRvalue::Binary {
                            op: BinaryOp::Add,
                            left: AmirOperand::Copy(base),
                            right: step,
                        }
                        | AmirRvalue::Binary {
                            op: BinaryOp::Add,
                            left: step,
                            right: AmirOperand::Copy(base),
                        },
                    ) => *base == iv && self.is_one(func, step),
                    _ => false,
                }
            })
    }

    /// A value the loop recomputes from invariant inputs. The loop writes no
    /// memory, so a dereferenceable load inside it reads the same value on
    /// every iteration.
    fn recomputes_invariant(
        &self,
        func: &AmirFunc,
        lp: &LoopInfo,
        value: &AmirOperand,
        is_invariant: &impl Fn(&AmirOperand) -> bool,
        depth: usize,
    ) -> bool {
        if is_invariant(value) {
            return true;
        }
        let AmirOperand::Copy(temp) = value else {
            return false;
        };
        if depth == 0 {
            return false;
        }
        let recurse = |operand: &AmirOperand| {
            self.recomputes_invariant(func, lp, operand, is_invariant, depth - 1)
        };
        match definition_in(func, &lp.body, *temp) {
            Some(AmirRvalue::Use(operand) | AmirRvalue::Len(operand)) => recurse(operand),
            Some(AmirRvalue::Load(place)) => self.is_dereferenceable(func, place),
            Some(AmirRvalue::Binary { op, left, right }) => {
                is_pure_binary(*op) && recurse(left) && recurse(right)
            }
            _ => false,
        }
    }

    fn is_one(&self, func: &AmirFunc, operand: &AmirOperand) -> bool {
        match operand {
            AmirOperand::Constant(AmirConstant::Pool(id)) => matches!(
                self.literal_pool.get(*id),
                AmirLiteralEntry::Int(text) if parse_int_literal(text) == Some(1)
            ),
            AmirOperand::Copy(temp) => {
                let all = (0..func.blocks.len())
                    .map(BlockId::from_usize)
                    .collect::<Vec<_>>();
                matches!(
                    definition_in(func, &all, *temp),
                    Some(AmirRvalue::Use(constant @ AmirOperand::Constant(_)))
                        if self.is_one(func, constant)
                )
            }
            _ => false,
        }
    }

    /// Whether skipping `stmt` changes nothing observable: no write, no
    /// effectful call, no trap and no ownership transfer.
    fn is_inert(&self, func: &AmirFunc, stmt: &AmirStmt) -> bool {
        let mut moves = false;
        crate::amir::visit::for_each_stmt_operand(stmt, |op| {
            moves |= matches!(op, AmirOperand::Move(_));
        });
        if moves {
            return false;
        }
        match stmt {
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => true,
            AmirStmt::Call {
                callee: AmirOperand::FunctionRef(symbol),
                ..
            } => self.removable_callees.contains(symbol),
            AmirStmt::Assign { rhs, .. } => match rhs {
                AmirRvalue::Use(_) | AmirRvalue::Len(_) => true,
                AmirRvalue::Binary { op, .. } => is_pure_binary(*op),
                AmirRvalue::Unary { op, .. } => {
                    matches!(op, UnaryOp::Not | UnaryOp::BitNot | UnaryOp::Neg)
                }
                AmirRvalue::IndexAccess { in_bounds, .. } => *in_bounds,
                AmirRvalue::Load(place) => self.is_dereferenceable(func, place),
                _ => false,
            },
            _ => false,
        }
    }

    /// A place reached only through a reference local and field projections,
    /// so reading it cannot fault.
    fn is_dereferenceable(&self, func: &AmirFunc, place: &AmirPlace) -> bool {
        let Some(local) = func.locals.get(place.local.as_usize()) else {
            return false;
        };
        let through_reference = matches!(
            self.interner.resolve(local.ty),
            ArType::Ref(_) | ArType::RefMut(_)
        );
        place
            .projections
            .iter()
            .enumerate()
            .all(|(position, projection)| match projection {
                AmirProjection::Deref => position == 0 && through_reference,
                AmirProjection::Field(_) | AmirProjection::TupleField(_) => true,
                _ => false,
            })
    }
}

/// Operators that neither trap nor have effects; integer arithmetic wraps.
fn is_pure_binary(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::LtEqual
            | BinaryOp::GtEqual
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::And
            | BinaryOp::Or
    )
}

/// Temps whose definitions sit inside the loop.
fn defined_in(func: &AmirFunc, lp: &LoopInfo) -> FxHashSet<TempId> {
    let mut defined = FxHashSet::default();
    for &block in &lp.body {
        defined.extend(
            func.block_params(func.block(block).params)
                .iter()
                .map(|param| param.id),
        );
        for stmt in func.block_stmts(block) {
            if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                defined.insert(*lhs);
            }
        }
    }
    defined
}

fn used_outside(func: &AmirFunc, lp: &LoopInfo, defined: &FxHashSet<TempId>) -> bool {
    let mut used = false;
    let mut visit = |op: &AmirOperand| {
        if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = op {
            used |= defined.contains(temp);
        }
    };
    for block in &func.blocks {
        if lp.body.contains(&block.id) {
            continue;
        }
        for stmt in func.block_stmts(block.id) {
            crate::amir::visit::for_each_stmt_operand(stmt, &mut visit);
        }
        crate::amir::visit::for_each_terminator_operand(&block.terminator, &mut visit);
    }
    used
}

/// No cycle other than the header's back edges: a nested loop could run
/// forever, which the counted header test says nothing about.
fn nested_acyclic(func: &AmirFunc, lp: &LoopInfo) -> bool {
    let header = lp.body[0];
    let mut state = vec![0u8; func.blocks.len()];
    let mut stack = vec![(header, 0usize)];
    state[header.as_usize()] = 1;
    while let Some((block, next)) = stack.pop() {
        let successors = func.successors(block);
        let Some(&successor) = successors.get(next) else {
            state[block.as_usize()] = 2;
            continue;
        };
        stack.push((block, next + 1));
        if successor == header || !lp.body.contains(&successor) {
            continue;
        }
        match state[successor.as_usize()] {
            0 => {
                state[successor.as_usize()] = 1;
                stack.push((successor, 0));
            }
            1 => return false,
            _ => {}
        }
    }
    true
}

/// The rvalue of `temp`'s single assignment within `blocks`.
fn definition_in<'f>(
    func: &'f AmirFunc,
    blocks: &[BlockId],
    temp: TempId,
) -> Option<&'f AmirRvalue> {
    let mut found = None;
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Assign { lhs, rhs } if *lhs == temp => {
                    if found.is_some() || !blocks.contains(&block.id) {
                        return None;
                    }
                    found = Some(rhs);
                }
                AmirStmt::Call { lhs: Some(lhs), .. } if *lhs == temp => return None,
                _ => {}
            }
        }
    }
    found
}

/// Sends every edge `from -> header` straight to `exit`.
fn retarget(
    terminator: &mut AmirTerminator,
    header: BlockId,
    exit: BlockId,
    exit_args: &[AmirOperand],
) {
    let redirect = |target: &mut BlockId, args: &mut Vec<AmirOperand>| {
        if *target == header {
            *target = exit;
            *args = exit_args.to_vec();
        }
    };
    match terminator {
        AmirTerminator::Goto { target, args } => redirect(target, args),
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => {
            redirect(if_true, true_args);
            redirect(if_false, false_args);
        }
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            for (_, target, args) in targets {
                redirect(target, args);
            }
            redirect(&mut otherwise.0, &mut otherwise.1);
        }
        AmirTerminator::Suspend { resume, args, .. } => redirect(resume, args),
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}
