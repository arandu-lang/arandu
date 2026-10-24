//! Bounds-check elimination from dominating guards.
//!
//! The design follows Go's `prove` pass and LLVM's `ConstraintElimination`: a
//! fact `a < b` holds in every block dominated by the edge of a branch that
//! tested it. An index access `s[i]` is proven when such a fact says
//! `i < len(s)` for the very same SSA values.
//!
//! Both of those passes had to retract proofs built from arithmetic: `+`, `-`
//! and `*` wrap in Arandu, so `pos + n <= len` says nothing about `pos + k`.
//! This pass therefore derives no fact through arithmetic and accepts only
//! unsigned comparisons, where `i < len` alone excludes every out-of-range
//! index. Values are identified through single-definition temps, following
//! copies, so a temp redefined on some path never carries a fact.

use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, Dominators, TempId,
};
use crate::ops::BinaryOp;
use crate::types::{ArType, TypeInterner};

/// Marks every slice index access proven in bounds. Returns whether any
/// access changed.
pub(crate) fn eliminate_bounds_checks(func: &mut AmirFunc, interner: &TypeInterner) -> bool {
    let values = Values::new(func);
    let guards = guard_facts(func, &values, interner);
    if guards.iter().all(Vec::is_empty) {
        return false;
    }
    let dominators = Dominators::new(func);
    let mut proven = Vec::new();
    for block in &func.blocks {
        let facts = dominating_facts(block.id, &dominators, &guards);
        if facts.is_empty() {
            continue;
        }
        for stmt_id in func.block_stmt_ids(block.id) {
            let AmirStmt::Assign {
                rhs:
                    AmirRvalue::IndexAccess {
                        base: AmirOperand::Copy(base) | AmirOperand::Move(base),
                        index: AmirOperand::Copy(index) | AmirOperand::Move(index),
                        in_bounds: false,
                    },
                ..
            } = func.stmt(stmt_id)
            else {
                continue;
            };
            if !matches!(
                interner.resolve(func.temps[base.as_usize()].ty),
                ArType::Slice(_) | ArType::MutSlice(_)
            ) || !is_unsigned(func, *index, interner)
            {
                continue;
            }
            let (Some(slice), Some(index)) = (values.root(*base), values.root(*index)) else {
                continue;
            };
            if facts
                .iter()
                .any(|&(lower, upper)| lower == index && values.len_of(upper) == Some(slice))
            {
                proven.push(stmt_id);
            }
        }
    }
    for stmt_id in &proven {
        if let AmirStmt::Assign {
            rhs: AmirRvalue::IndexAccess { in_bounds, .. },
            ..
        } = func.stmt_mut(*stmt_id)
        {
            *in_bounds = true;
        }
    }
    !proven.is_empty()
}

/// Definitions of each temp; only single-definition temps carry facts.
struct Values<'f> {
    func: &'f AmirFunc,
    definition: Vec<Option<&'f AmirRvalue>>,
    single: Vec<bool>,
}

impl<'f> Values<'f> {
    fn new(func: &'f AmirFunc) -> Self {
        let mut count = vec![0u32; func.temps.len()];
        let mut definition = vec![None; func.temps.len()];
        for &param in &func.params {
            count[param.as_usize()] += 1;
        }
        for param in &func.block_params {
            count[param.id.as_usize()] += 1;
        }
        for stmt in func.stmts.payloads.iter() {
            match stmt {
                AmirStmt::Assign { lhs, rhs } => {
                    count[lhs.as_usize()] += 1;
                    definition[lhs.as_usize()] = Some(rhs);
                }
                AmirStmt::Call { lhs: Some(lhs), .. } => count[lhs.as_usize()] += 1,
                _ => {}
            }
        }
        let single = count.iter().map(|&defs| defs == 1).collect();
        Self {
            func,
            definition,
            single,
        }
    }

    /// The value a temp holds, following single-definition copies.
    fn root(&self, temp: TempId) -> Option<TempId> {
        let mut current = temp;
        for _ in 0..=self.func.temps.len() {
            if !self.single[current.as_usize()] {
                return None;
            }
            match self.definition[current.as_usize()] {
                Some(AmirRvalue::Use(AmirOperand::Copy(source) | AmirOperand::Move(source))) => {
                    current = *source;
                }
                _ => return Some(current),
            }
        }
        None
    }

    /// The slice whose length `temp` holds.
    fn len_of(&self, temp: TempId) -> Option<TempId> {
        match self.definition[temp.as_usize()] {
            Some(AmirRvalue::Len(AmirOperand::Copy(slice) | AmirOperand::Move(slice))) => {
                self.root(*slice)
            }
            _ => None,
        }
    }
}

/// `(lower, upper)` facts meaning `lower < upper`, established on entry to
/// each block whose only predecessor branches to it on a tested comparison.
fn guard_facts(
    func: &AmirFunc,
    values: &Values<'_>,
    interner: &TypeInterner,
) -> Vec<Vec<(TempId, TempId)>> {
    let mut facts = vec![Vec::new(); func.blocks.len()];
    for block in &func.blocks {
        let AmirTerminator::Branch {
            condition: AmirOperand::Copy(condition) | AmirOperand::Move(condition),
            if_true,
            if_false,
            ..
        } = &block.terminator
        else {
            continue;
        };
        if if_true == if_false || !values.single[condition.as_usize()] {
            continue;
        }
        let Some(AmirRvalue::Binary {
            op,
            left: AmirOperand::Copy(left) | AmirOperand::Move(left),
            right: AmirOperand::Copy(right) | AmirOperand::Move(right),
        }) = values.definition[condition.as_usize()]
        else {
            continue;
        };
        if !is_unsigned(func, *left, interner) || !is_unsigned(func, *right, interner) {
            continue;
        }
        let (Some(left), Some(right)) = (values.root(*left), values.root(*right)) else {
            continue;
        };
        // `l < r` and `r > l` hold on the true edge; `l >= r` and `r <= l`
        // fail on the false edge. Each yields `l < r` for unsigned operands.
        let (target, fact) = match op {
            BinaryOp::Lt => (*if_true, (left, right)),
            BinaryOp::Gt => (*if_true, (right, left)),
            BinaryOp::GtEqual => (*if_false, (left, right)),
            BinaryOp::LtEqual => (*if_false, (right, left)),
            _ => continue,
        };
        if only_predecessor(func, target) == Some(block.id) {
            facts[target.as_usize()].push(fact);
        }
    }
    facts
}

fn only_predecessor(func: &AmirFunc, block: BlockId) -> Option<BlockId> {
    match func.cfg.predecessors.get(block.as_usize())?.as_slice() {
        [predecessor] => Some(*predecessor),
        _ => None,
    }
}

fn dominating_facts(
    block: BlockId,
    dominators: &Dominators,
    guards: &[Vec<(TempId, TempId)>],
) -> Vec<(TempId, TempId)> {
    let mut facts = Vec::new();
    let mut current = block;
    for _ in 0..guards.len() {
        facts.extend_from_slice(&guards[current.as_usize()]);
        match dominators.immediate_dominator(current) {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    facts
}

fn is_unsigned(func: &AmirFunc, temp: TempId, interner: &TypeInterner) -> bool {
    matches!(
        interner.resolve(func.temps[temp.as_usize()].ty),
        ArType::Primitive(primitive) if primitive.is_integer() && !primitive.is_signed()
    )
}
