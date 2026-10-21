use super::LatticeVal;
use super::fold::{
    const_as_i128, eval_rvalue, evaluation_safe, normalize_constant, operand_lattice,
};
use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId,
};
use crate::literal_pool::AmirLiteralPool;

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

pub(super) fn analyse<'bump>(
    func: &AmirFunc,
    pool: &mut AmirLiteralPool,
    bump: &'bump bumpalo::Bump,
    interner: Option<&crate::types::TypeInterner>,
) -> (
    bumpalo::collections::Vec<'bump, LatticeVal>,
    bumpalo::collections::Vec<'bump, bool>,
) {
    let n_temps = func.temps.len();
    let n_blocks = func.blocks.len();
    let mut lattice = bumpalo::collections::Vec::from_iter_in(
        std::iter::repeat_n(LatticeVal::Undefined, n_temps),
        bump,
    );
    for &param in &func.params {
        lattice[param.as_usize()] = LatticeVal::Overdefined;
    }
    if let Some(receiver) = &func.receiver {
        lattice[receiver.temp.as_usize()] = LatticeVal::Overdefined;
    }
    let mut reachable =
        bumpalo::collections::Vec::from_iter_in(std::iter::repeat_n(false, n_blocks), bump);
    reachable[0] = true;

    // RPO once – covers all statically reachable blocks.
    let rpo = crate::amir::reverse_post_order(func);

    // Theoretical bound on monotone lattice changes + CFG reachability:
    // each temp changes at most twice (Undefined -> Constant -> Overdefined)
    // and each block changes reachability at most once (false -> true).
    let max_iters = n_blocks
        .saturating_mul(n_temps)
        .saturating_mul(2)
        .saturating_add(crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM);
    let mut iters = 0usize;

    loop {
        iters += 1;
        if iters > max_iters {
            break;
        }
        let mut changed = false;

        for &bid in &rpo {
            if !reachable[bid.as_usize()] {
                continue;
            }

            // --- φ / block params from jump args of reachable preds ----------
            changed |= meet_block_params(func, bid, &mut lattice, &reachable, pool, interner);

            // --- statements -------------------------------------------------
            for stmt_id in func.block_stmt_ids(bid) {
                let stmt = func.stmt(stmt_id);
                if let AmirStmt::Assign { lhs, rhs } = stmt {
                    let old = lattice[lhs.as_usize()];
                    // `T?` is a null-or-pointer handle. Scalar constants like `0`
                    // are *not* the same as `nil` at runtime (they get boxed).
                    // Never fold a non-Nil constant into a Nullable temp.
                    let new = if temp_is_nullable(func, *lhs) {
                        match eval_rvalue(rhs, &lattice, pool) {
                            LatticeVal::Constant(AmirConstant::Nil) => {
                                LatticeVal::Constant(AmirConstant::Nil)
                            }
                            LatticeVal::Undefined => LatticeVal::Undefined,
                            _ => LatticeVal::Overdefined,
                        }
                    } else {
                        if evaluation_safe(
                            rhs,
                            func.temps[lhs.as_usize()].ty,
                            interner,
                            &lattice,
                            pool,
                        ) {
                            eval_rvalue(rhs, &lattice, pool)
                        } else {
                            LatticeVal::Overdefined
                        }
                    };
                    let new = match new {
                        LatticeVal::Constant(value) => {
                            normalize_constant(value, func.temps[lhs.as_usize()].ty, interner, pool)
                                .map(LatticeVal::Constant)
                                .unwrap_or(LatticeVal::Overdefined)
                        }
                        other => other,
                    };
                    let merged = meet(old, new);
                    if merged != old {
                        lattice[lhs.as_usize()] = merged;
                        changed = true;
                    }
                } else if let AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
                    // A call result is a runtime definition. Leaving it at
                    // `Undefined` makes a phi meet ignore that incoming edge,
                    // so a constant from another edge can win unsoundly.
                    let old = lattice[lhs.as_usize()];
                    let merged = meet(old, LatticeVal::Overdefined);
                    if merged != old {
                        lattice[lhs.as_usize()] = merged;
                        changed = true;
                    }
                }
            }

            // --- terminator (may mark more blocks reachable) -----------------
            let term = &func.block(bid).terminator;
            changed |= propagate_branches(term, &lattice, pool, &mut reachable);
        }

        if !changed {
            break;
        }
    }

    (lattice, reachable)
}

// ---------------------------------------------------------------------------
// Transformation
// ---------------------------------------------------------------------------

pub(super) fn apply(
    func: &mut AmirFunc,
    pool: &mut AmirLiteralPool,
    lattice: &[LatticeVal],
    reachable: &[bool],
    bump: &bumpalo::Bump,
) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        let bid = BlockId::from_usize(bi);

        // Fold every assign whose lhs is a proven constant.
        let mut stmt_ids = bumpalo::collections::Vec::new_in(bump);
        stmt_ids.extend(func.block_stmt_ids(bid));
        for stmt_id in stmt_ids {
            let stmt = func.stmt_mut(stmt_id);
            if let AmirStmt::Assign { lhs, rhs } = stmt
                && let LatticeVal::Constant(c) = lattice[lhs.as_usize()]
                && !matches!(rhs, AmirRvalue::Use(AmirOperand::Constant(c2)) if *c2 == c)
            {
                *rhs = AmirRvalue::Use(AmirOperand::Constant(c));
                changed = true;
            }
        }

        // Simplify terminators whose condition is a known constant.
        let term = &func.block(bid).terminator;
        if !matches!(
            term,
            AmirTerminator::Branch { .. } | AmirTerminator::SwitchInt { .. }
        ) {
            continue;
        }
        if let Some(new_term) = try_simplify_terminator(term, lattice, pool) {
            func.block_mut(bid).terminator = new_term;
            changed = true;
        }
    }

    // Branch folding can disconnect blocks. Keep every individual pass output
    // valid by explicitly terminating SCCP-dead blocks; SimplifyCFG removes
    // them and compacts ids later in the pipeline.
    for (index, is_reachable) in reachable.iter().copied().enumerate().skip(1) {
        if !is_reachable && !matches!(func.blocks[index].terminator, AmirTerminator::Unreachable) {
            func.blocks[index].terminator = AmirTerminator::Unreachable;
            changed = true;
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// Lattice helpers
// ---------------------------------------------------------------------------

/// Greatest-lower-bound (meet) in the constant-propagation lattice.
///
/// Undefined ⊓ x  = x
/// x ⊓ Undefined  = x
/// Overdefined ⊓ _ = Overdefined
/// _ ⊓ Overdefined = Overdefined
/// Constant(a) ⊓ Constant(b) = Constant(a) when a == b else Overdefined
fn meet(a: LatticeVal, b: LatticeVal) -> LatticeVal {
    use LatticeVal::*;
    match (a, b) {
        (Undefined, x) | (x, Undefined) => x,
        (Constant(a), Constant(b)) if a == b => Constant(a),
        _ => Overdefined,
    }
}

/// Meet jump-arg values from reachable predecessors into each block param temp.
///
/// AMIR block params are SSA φ nodes: param `i` of `bid` is defined by the
/// `i`-th argument on every edge into `bid`. Only **reachable** preds contribute
/// (unreachable edges must not pollute the meet — same as classic SCCP).
fn meet_block_params(
    func: &AmirFunc,
    bid: BlockId,
    lattice: &mut [LatticeVal],
    reachable: &[bool],
    pool: &mut AmirLiteralPool,
    interner: Option<&crate::types::TypeInterner>,
) -> bool {
    let params = func.block_params(func.block(bid).params);
    if params.is_empty() {
        return false;
    }

    let mut changed = false;
    let n_params = params.len();

    // Accumulators: one meet per param index, only from reachable preds.
    let mut acc: Vec<LatticeVal> = vec![LatticeVal::Undefined; n_params];
    let mut saw_pred = false;

    for &pred in func.predecessors(bid) {
        if pred.as_usize() >= reachable.len() || !reachable[pred.as_usize()] {
            continue;
        }
        saw_pred = true;
        let Some(args) = jump_args_to(&func.block(pred).terminator, bid) else {
            // Edge without args while params exist — treat all as Overdefined.
            acc.fill(LatticeVal::Overdefined);
            continue;
        };
        for i in 0..n_params {
            let arg_lat = if i < args.len() {
                operand_lattice(&args[i], lattice)
            } else {
                LatticeVal::Overdefined
            };
            let param_temp = params[i].id;
            let contrib = if temp_is_nullable(func, param_temp) {
                match arg_lat {
                    LatticeVal::Constant(AmirConstant::Nil) => {
                        LatticeVal::Constant(AmirConstant::Nil)
                    }
                    LatticeVal::Undefined => LatticeVal::Undefined,
                    _ => LatticeVal::Overdefined,
                }
            } else {
                arg_lat
            };
            let contrib = match contrib {
                LatticeVal::Constant(value) => {
                    normalize_constant(value, params[i].ty, interner, pool)
                        .map(LatticeVal::Constant)
                        .unwrap_or(LatticeVal::Overdefined)
                }
                other => other,
            };
            acc[i] = meet(acc[i], contrib);
        }
    }

    if !saw_pred {
        return false;
    }

    for (i, p) in params.iter().enumerate() {
        let idx = p.id.as_usize();
        if idx >= lattice.len() {
            continue;
        }
        let old = lattice[idx];
        let merged = meet(old, acc[i]);
        if merged != old {
            lattice[idx] = merged;
            changed = true;
        }
    }
    changed
}

/// Jump arguments on `term` that feed `target`'s block params, if any.
pub(super) fn jump_args_to(term: &AmirTerminator, target: BlockId) -> Option<&[AmirOperand]> {
    match term {
        AmirTerminator::Goto { target: t, args } if *t == target => Some(args.as_slice()),
        AmirTerminator::Suspend { resume, args, .. } if *resume == target => Some(args.as_slice()),
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => {
            if *if_true == target && *if_false == target && true_args != false_args {
                return None;
            }
            if *if_true == target {
                Some(true_args.as_slice())
            } else if *if_false == target {
                Some(false_args.as_slice())
            } else {
                None
            }
        }
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut incoming: Option<&[AmirOperand]> = None;
            for (_, dest, args) in targets {
                if *dest == target {
                    if incoming.is_some_and(|previous| previous != args.as_slice()) {
                        return None;
                    }
                    incoming = Some(args.as_slice());
                }
            }
            if otherwise.0 == target {
                if incoming.is_some_and(|previous| previous != otherwise.1.as_slice()) {
                    None
                } else {
                    Some(otherwise.1.as_slice())
                }
            } else {
                incoming
            }
        }
        _ => None,
    }
}

fn temp_is_nullable(func: &AmirFunc, temp: crate::amir::TempId) -> bool {
    func.temps[temp.as_usize()].is_nullable
}

// ---------------------------------------------------------------------------
// Branch propagation
// ---------------------------------------------------------------------------

/// Marks successors as reachable based on the terminator and known lattice values.
/// Returns `true` if any block was newly marked.
fn propagate_branches(
    terminator: &AmirTerminator,
    lattice: &[LatticeVal],
    pool: &AmirLiteralPool,
    reachable: &mut [bool],
) -> bool {
    let mut changed = false;
    match terminator {
        AmirTerminator::Branch {
            condition,
            if_true,
            if_false,
            ..
        } => match operand_lattice(condition, lattice) {
            LatticeVal::Constant(AmirConstant::Bool(true)) => {
                changed |= set_reachable(*if_true, reachable);
            }
            LatticeVal::Constant(AmirConstant::Bool(false)) => {
                changed |= set_reachable(*if_false, reachable);
            }
            _ => {
                changed |= set_reachable(*if_true, reachable);
                changed |= set_reachable(*if_false, reachable);
            }
        },
        AmirTerminator::Goto { target, .. } => {
            changed |= set_reachable(*target, reachable);
        }
        AmirTerminator::Suspend { resume, .. } => {
            changed |= set_reachable(*resume, reachable);
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => match operand_lattice(discriminant, lattice) {
            LatticeVal::Constant(c) => {
                if let Some(val) = const_as_i128(&c, pool) {
                    let mut matched = false;
                    for (tv, tb, _) in targets {
                        if *tv == val {
                            changed |= set_reachable(*tb, reachable);
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        changed |= set_reachable(otherwise.0, reachable);
                    }
                } else {
                    for (_, tb, _) in targets {
                        changed |= set_reachable(*tb, reachable);
                    }
                    changed |= set_reachable(otherwise.0, reachable);
                }
            }
            _ => {
                for (_, tb, _) in targets {
                    changed |= set_reachable(*tb, reachable);
                }
                changed |= set_reachable(otherwise.0, reachable);
            }
        },
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
    changed
}

fn set_reachable(block: BlockId, reachable: &mut [bool]) -> bool {
    let idx = block.as_usize();
    if idx < reachable.len() && !reachable[idx] {
        reachable[idx] = true;
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Terminator simplification
// ---------------------------------------------------------------------------

/// When a branch condition (or switch discriminant) is a known constant,
/// return a single `Goto`. Otherwise `None` (no clone of the original).
fn try_simplify_terminator(
    terminator: &AmirTerminator,
    lattice: &[LatticeVal],
    pool: &AmirLiteralPool,
) -> Option<AmirTerminator> {
    match terminator {
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => match operand_lattice(condition, lattice) {
            LatticeVal::Constant(AmirConstant::Bool(true)) => Some(AmirTerminator::Goto {
                target: *if_true,
                args: true_args.clone(),
            }),
            LatticeVal::Constant(AmirConstant::Bool(false)) => Some(AmirTerminator::Goto {
                target: *if_false,
                args: false_args.clone(),
            }),
            _ => None,
        },
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => match operand_lattice(discriminant, lattice) {
            LatticeVal::Constant(c) => {
                let val = const_as_i128(&c, pool)?;
                for (tv, tb, args) in targets {
                    if *tv == val {
                        return Some(AmirTerminator::Goto {
                            target: *tb,
                            args: args.clone(),
                        });
                    }
                }
                Some(AmirTerminator::Goto {
                    target: otherwise.0,
                    args: otherwise.1.clone(),
                })
            }
            _ => None,
        },
        _ => None,
    }
}
