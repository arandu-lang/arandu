use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId,
};
use crate::literal_pool::{AmirLiteralEntry, AmirLiteralPool};
use crate::ops::{BinaryOp, UnaryOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LatticeVal {
    Undefined,
    Constant(AmirConstant),
    Overdefined,
}

/// Sparse Conditional Constant Propagation (Wegman & Zadeck 1991).
///
/// Propagates constants across basic blocks and eliminates dead branches
/// by jointly modeling value constness and CFG reachability in a single
/// fixpoint iteration.  This subsumes intra-block constant folding and
/// adds cross-block propagation + branch pruning.
///
/// **Block params (SSA φ):** for each param temp, lattice is the meet of the
/// corresponding jump-arg operands from **reachable** predecessors. Without
/// this, loop headers and join blocks stay `Undefined` forever and never
/// refine conditions on block-param values.
pub(super) fn sccp(
    func: &mut AmirFunc,
    pool: &mut AmirLiteralPool,
    bump: &bumpalo::Bump,
    interner: Option<&crate::types::TypeInterner>,
) -> bool {
    let n_temps = func.temps.len();
    let n_blocks = func.blocks.len();
    if n_temps == 0 || n_blocks == 0 {
        return false;
    }

    // Phase 1 – analyse lattice + reachability to fixpoint
    let (lattice, sccp_reachable) = analyse(func, pool, bump, interner);

    // Phase 2 – apply results to the function
    apply(func, pool, &lattice, &sccp_reachable, bump)
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

fn analyse<'bump>(
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

fn apply(
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
fn jump_args_to(term: &AmirTerminator, target: BlockId) -> Option<&[AmirOperand]> {
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

// ---------------------------------------------------------------------------
// Rvalue evaluation
// ---------------------------------------------------------------------------

fn temp_is_nullable(func: &AmirFunc, temp: crate::amir::TempId) -> bool {
    func.temps[temp.as_usize()].is_nullable
}

fn eval_rvalue(
    rvalue: &AmirRvalue,
    lattice: &[LatticeVal],
    pool: &mut AmirLiteralPool,
) -> LatticeVal {
    match rvalue {
        AmirRvalue::Use(op) => operand_lattice(op, lattice),
        AmirRvalue::Unary { op, operand } => match operand_lattice(operand, lattice) {
            LatticeVal::Constant(c) => fold_unary(*op, c, pool)
                .map(LatticeVal::Constant)
                .unwrap_or(LatticeVal::Overdefined),
            LatticeVal::Overdefined => LatticeVal::Overdefined,
            LatticeVal::Undefined => LatticeVal::Undefined,
        },
        AmirRvalue::Binary { op, left, right } => {
            let l = operand_lattice(left, lattice);
            let r = operand_lattice(right, lattice);
            match (l, r) {
                (LatticeVal::Constant(lc), LatticeVal::Constant(rc)) => {
                    fold_binary(*op, lc, rc, pool)
                        .map(LatticeVal::Constant)
                        .unwrap_or(LatticeVal::Overdefined)
                }
                (LatticeVal::Overdefined, _) | (_, LatticeVal::Overdefined) => {
                    LatticeVal::Overdefined
                }
                _ => LatticeVal::Undefined,
            }
        }
        // Everything else (Load, Borrow, FieldAccess, Alloc, Call, etc.)
        // is treated as non-foldable.
        _ => LatticeVal::Overdefined,
    }
}

fn operand_lattice(op: &AmirOperand, lattice: &[LatticeVal]) -> LatticeVal {
    match op {
        AmirOperand::Constant(c) => LatticeVal::Constant(*c),
        AmirOperand::Copy(t) | AmirOperand::Move(t) => lattice[t.as_usize()],
        AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => LatticeVal::Overdefined,
    }
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

// ---------------------------------------------------------------------------
// Constant-folding helpers (adapted from optimize.rs)
// ---------------------------------------------------------------------------

fn fold_unary(
    op: UnaryOp,
    value: AmirConstant,
    pool: &mut AmirLiteralPool,
) -> Option<AmirConstant> {
    match (op, value) {
        (UnaryOp::Not, AmirConstant::Bool(b)) => Some(AmirConstant::Bool(!b)),
        (UnaryOp::Neg, v) => {
            let val = const_as_i128(&v, pool)?;
            checked_int(val.checked_neg(), pool)
        }
        (UnaryOp::BitNot, v) => {
            let val = const_as_i128(&v, pool)?;
            Some(AmirConstant::Pool(pool.intern_int((!val).to_string())))
        }
        _ => None,
    }
}

fn fold_binary(
    op: BinaryOp,
    left: AmirConstant,
    right: AmirConstant,
    pool: &mut AmirLiteralPool,
) -> Option<AmirConstant> {
    match (left, right) {
        (AmirConstant::Bool(a), AmirConstant::Bool(b)) => match op {
            BinaryOp::And => Some(AmirConstant::Bool(a && b)),
            BinaryOp::Or => Some(AmirConstant::Bool(a || b)),
            BinaryOp::Equal => Some(AmirConstant::Bool(a == b)),
            BinaryOp::NotEqual => Some(AmirConstant::Bool(a != b)),
            _ => None,
        },
        (l, r) => {
            let lv = const_as_i128(&l, pool)?;
            let rv = const_as_i128(&r, pool)?;
            match op {
                BinaryOp::Add => checked_int(lv.checked_add(rv), pool),
                BinaryOp::Sub => checked_int(lv.checked_sub(rv), pool),
                BinaryOp::Mul => checked_int(lv.checked_mul(rv), pool),
                BinaryOp::Div if rv != 0 => checked_int(lv.checked_div(rv), pool),
                BinaryOp::Mod if rv != 0 => checked_int(lv.checked_rem(rv), pool),
                BinaryOp::BitOr => Some(int_const(lv | rv, pool)),
                BinaryOp::BitXor => Some(int_const(lv ^ rv, pool)),
                BinaryOp::BitAnd => Some(int_const(lv & rv, pool)),
                BinaryOp::ShiftLeft if (0..128).contains(&rv) => {
                    checked_int(lv.checked_shl(rv as u32), pool)
                }
                // Constants do not carry their Arandu signedness here. A
                // negative i128 bit pattern may represent either a negative
                // `int` or a high-bit `uint`; arithmetic and logical right
                // shifts differ for that case, so leave it for the backend.
                BinaryOp::ShiftRight if lv >= 0 && (0..128).contains(&rv) => {
                    checked_int(lv.checked_shr(rv as u32), pool)
                }
                BinaryOp::Equal => Some(AmirConstant::Bool(lv == rv)),
                BinaryOp::NotEqual => Some(AmirConstant::Bool(lv != rv)),
                BinaryOp::Lt => Some(AmirConstant::Bool(lv < rv)),
                BinaryOp::Gt => Some(AmirConstant::Bool(lv > rv)),
                BinaryOp::LtEqual => Some(AmirConstant::Bool(lv <= rv)),
                BinaryOp::GtEqual => Some(AmirConstant::Bool(lv >= rv)),
                _ => None,
            }
        }
    }
}

fn const_as_i128(c: &AmirConstant, pool: &AmirLiteralPool) -> Option<i128> {
    match c {
        AmirConstant::Pool(id) => match pool.get(*id) {
            AmirLiteralEntry::Int(val) => arandu_middle::literal_pool::parse_int_literal(val),
            AmirLiteralEntry::Float(_)
            | AmirLiteralEntry::FloatBits(_)
            | AmirLiteralEntry::Str(_)
            | AmirLiteralEntry::Char(_) => None,
        },
        AmirConstant::Bool(_) | AmirConstant::Nil => None,
    }
}

fn integer_shape(
    ty: crate::types::TypeId,
    interner: &crate::types::TypeInterner,
) -> Option<(u32, bool)> {
    use crate::types::{ArType, Primitive};
    let primitive = match interner.resolve(ty) {
        ArType::IntLiteral => Primitive::Int,
        ArType::Primitive(Primitive::Char) => return Some((32, false)),
        ArType::Primitive(Primitive::ISize | Primitive::USize) => return None,
        ArType::Primitive(primitive) => primitive,
        _ => return None,
    };
    // Pointer-sized integers were excluded above. For these fixed-width types,
    // IntegerType's canonical width is independent of this explicit layout.
    let integer =
        arandu_middle::ctfe::IntegerType::new(primitive, crate::layout::DataLayout::ptr_width(4))
            .ok()?;
    Some((u32::from(integer.bit_width()), integer.is_signed()))
}

fn normalize_constant(
    value: AmirConstant,
    ty: crate::types::TypeId,
    interner: Option<&crate::types::TypeInterner>,
    pool: &mut AmirLiteralPool,
) -> Option<AmirConstant> {
    use crate::types::{ArType, Primitive};
    if let Some(interner) = interner {
        if matches!(interner.resolve(ty), ArType::Primitive(Primitive::Bool)) {
            return match value {
                AmirConstant::Bool(_) => Some(value),
                _ => const_as_i128(&value, pool).map(|number| AmirConstant::Bool(number != 0)),
            };
        }
        if let Some((width, signed)) = integer_shape(ty, interner) {
            let number = match value {
                AmirConstant::Bool(boolean) => i128::from(boolean),
                _ => const_as_i128(&value, pool)?,
            };
            let modulus = 1i128 << width;
            let bits = number.rem_euclid(modulus);
            let normalized = if signed && bits >= modulus / 2 {
                bits - modulus
            } else {
                bits
            };
            if normalized == number && matches!(value, AmirConstant::Pool(_)) {
                return Some(value);
            }
            return Some(int_const(normalized, pool));
        }
    }
    match value {
        AmirConstant::Pool(id) => match pool.get(id) {
            AmirLiteralEntry::Int(_)
            | AmirLiteralEntry::Float(_)
            | AmirLiteralEntry::FloatBits(_) => None,
            // These constants have no arithmetic-width-dependent conversion.
            AmirLiteralEntry::Str(_) | AmirLiteralEntry::Char(_) => Some(value),
        },
        AmirConstant::Bool(_) | AmirConstant::Nil => Some(value),
    }
}

fn evaluation_safe(
    rhs: &AmirRvalue,
    ty: crate::types::TypeId,
    interner: Option<&crate::types::TypeInterner>,
    lattice: &[LatticeVal],
    pool: &AmirLiteralPool,
) -> bool {
    if let AmirRvalue::Binary { op, left, right } = rhs
        && matches!(
            op,
            BinaryOp::Div | BinaryOp::Mod | BinaryOp::ShiftLeft | BinaryOp::ShiftRight
        )
    {
        let Some((width, signed)) = interner.and_then(|interner| integer_shape(ty, interner))
        else {
            return false;
        };
        if let (LatticeVal::Constant(left), LatticeVal::Constant(right)) = (
            operand_lattice(left, lattice),
            operand_lattice(right, lattice),
        ) && let (Some(left), Some(right)) =
            (const_as_i128(&left, pool), const_as_i128(&right, pool))
        {
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) {
                return right != 0 && !(signed && left == -(1i128 << (width - 1)) && right == -1);
            }
            return right >= 0 && right < i128::from(width);
        }
    }
    true
}

fn int_const(val: i128, pool: &mut AmirLiteralPool) -> AmirConstant {
    AmirConstant::Pool(pool.intern_int(val.to_string()))
}

fn checked_int(val: Option<i128>, pool: &mut AmirLiteralPool) -> Option<AmirConstant> {
    val.map(|v| int_const(v, pool))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::program::extend_block_range;
    use crate::amir::{
        AmirBasicBlock, AmirLocal, AmirPlace, AmirStmt, AmirStmtTable, AmirTemp, BlockId,
        BlockParam, LocalId, TempId,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::passes::type_checker::types::{ArType, Primitive};

    fn intern_ty(ty: ArType) -> crate::types::TypeId {
        crate::types::TypeInterner::new().intern(ty)
    }

    fn int_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        }
    }

    fn bool_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Bool)),
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        }
    }

    #[test]
    fn negative_right_shift_is_not_folded_without_signedness() {
        let mut pool = AmirLiteralPool::default();
        let minus_one = AmirConstant::Pool(pool.intern_int("-1"));
        let one = AmirConstant::Pool(pool.intern_int("1"));

        assert_eq!(
            fold_binary(BinaryOp::ShiftRight, minus_one, one, &mut pool),
            None
        );
    }

    #[test]
    fn fixed_width_constants_keep_assignment_conversion_and_unknown_layout_fails_closed() {
        let interner = crate::types::TypeInterner::new();
        let mut pool = AmirLiteralPool::default();
        for (primitive, input, expected) in [
            (crate::types::Primitive::U8, "260", 4),
            (crate::types::Primitive::I8, "255", -1),
            (crate::types::Primitive::Int, "2147483648", -2147483648),
        ] {
            let value = AmirConstant::Pool(pool.intern_int(input));
            let ty = interner.intern(crate::types::ArType::Primitive(primitive));
            let normalized = normalize_constant(value, ty, Some(&interner), &mut pool).unwrap();
            assert_eq!(const_as_i128(&normalized, &pool), Some(expected));
            assert!(normalize_constant(value, ty, None, &mut pool).is_none());
        }
        let value = AmirConstant::Pool(pool.intern_int("1"));
        let pointer_sized = interner.intern(crate::types::ArType::Primitive(
            crate::types::Primitive::USize,
        ));
        assert!(normalize_constant(value, pointer_sized, Some(&interner), &mut pool).is_none());
    }

    #[test]
    fn duplicate_edges_with_different_arguments_never_select_just_the_first() {
        let target = BlockId(1);
        let yes = vec![AmirOperand::Constant(AmirConstant::Bool(true))];
        let no = vec![AmirOperand::Constant(AmirConstant::Bool(false))];
        let branch = AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId(0)),
            if_true: target,
            true_args: yes.clone(),
            if_false: target,
            false_args: no.clone(),
        };
        assert!(jump_args_to(&branch, target).is_none());
        let switch = AmirTerminator::SwitchInt {
            discriminant: AmirOperand::Copy(TempId(0)),
            targets: vec![(0, target, yes.clone())],
            otherwise: (target, no),
        };
        assert!(jump_args_to(&switch, target).is_none());
        let equal = AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId(0)),
            if_true: target,
            true_args: yes.clone(),
            if_false: target,
            false_args: yes.clone(),
        };
        assert_eq!(jump_args_to(&equal, target), Some(yes.as_slice()));
    }

    #[test]
    fn call_result_contribution_keeps_join_overdefined() {
        let mut pool = AmirLiteralPool::default();
        let bool_ty = intern_ty(ArType::Primitive(Primitive::Bool));
        let mut stmts = AmirStmtTable::new();
        let call = stmts.push(AmirStmt::Call {
            lhs: Some(TempId::from_usize(1)),
            callee: AmirOperand::FunctionRef(crate::SymbolId::new(0, 1)),
            args: smallvec::smallvec![],
            return_borrow: None,
        });
        let mut call_range = DenseRange::empty();
        extend_block_range(&mut call_range, call);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(0)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: call_range,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Constant(AmirConstant::Bool(false))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: DenseRange::empty(),
                params: DenseRange::new(0, 1),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(2)),
                    if_true: BlockId::from_usize(4),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(5),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(4),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
            AmirBasicBlock {
                id: BlockId::from_usize(5),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: bool_ty,
                is_memory: false,
                symbol: None,
                span: arandu_lexer::Span::new(0, 0, 0),
                use_span: None,
            }],
            temps: vec![bool_temp(0), bool_temp(1), bool_temp(2)],
            blocks,
            block_params: vec![BlockParam {
                id: TempId::from_usize(2),
                local: LocalId::from_usize(0),
                ty: bool_ty,
                from: None,
                moved: false,
            }],
            stmts,
            cfg,
        };

        let bump = bumpalo::Bump::new();
        let _ = sccp(
            &mut func,
            &mut pool,
            &bump,
            Some(&crate::types::TypeInterner::new()),
        );
        assert!(matches!(
            func.block(BlockId::from_usize(3)).terminator,
            AmirTerminator::Branch { .. }
        ));
    }

    /// Join: both preds pass the same constant into a block param → param is constant.
    #[test]
    fn block_param_meet_same_constant_from_two_preds() {
        let mut pool = AmirLiteralPool::default();
        let five = AmirConstant::Pool(pool.intern_int("5"));

        let mut stmts = AmirStmtTable::new();
        // bb2: t2 = use(param t1)  — should fold to 5 after phi meet
        let a = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let mut range2 = DenseRange::empty();
        extend_block_range(&mut range2, a);

        let int_ty = intern_ty(ArType::Primitive(Primitive::Int));
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: vec![AmirOperand::Constant(five)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(2),
                    args: vec![AmirOperand::Constant(five)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: range2,
                params: DenseRange::new(0, 1),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int_ty,
                is_memory: false,
                symbol: None,
                span: arandu_lexer::Span::new(0, 0, 0),
                use_span: None,
            }],
            temps: vec![int_temp(0), int_temp(1), int_temp(2)],
            blocks,

            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: int_ty,
                from: None,
                moved: false,
            }],

            stmts,
            cfg,
        };

        let bump = bumpalo::Bump::new();
        assert!(sccp(
            &mut func,
            &mut pool,
            &bump,
            Some(&crate::types::TypeInterner::new())
        ));
        let stmt = func.block_stmts(BlockId::from_usize(2)).next().unwrap();
        match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(c)),
                ..
            } => {
                assert_eq!(const_as_i128(c, &pool), Some(5));
            }
            other => panic!("expected folded assign, got {other:?}"),
        }
    }

    /// Different constants from two reachable preds → Overdefined (not a wrong fold).
    #[test]
    fn block_param_meet_different_constants_is_overdefined() {
        let mut pool = AmirLiteralPool::default();
        let one = AmirConstant::Pool(pool.intern_int("1"));
        let two = AmirConstant::Pool(pool.intern_int("2"));
        let int_ty = intern_ty(ArType::Primitive(Primitive::Int));

        let mut stmts = AmirStmtTable::new();
        let a = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
        });
        let s = stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        let mut range_join = DenseRange::empty();
        extend_block_range(&mut range_join, a);
        extend_block_range(&mut range_join, s);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    // Undefined condition → both arms reachable
                    condition: AmirOperand::Copy(TempId::from_usize(0)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Constant(one)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Constant(two)],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: range_join,
                params: DenseRange::new(0, 1),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int_ty,
                is_memory: false,
                symbol: None,
                span: arandu_lexer::Span::new(0, 0, 0),
                use_span: None,
            }],
            temps: vec![int_temp(0), int_temp(1), int_temp(2)],
            blocks,

            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: int_ty,
                from: None,
                moved: false,
            }],

            stmts,
            cfg,
        };
        let bump = bumpalo::Bump::new();
        let _ = sccp(
            &mut func,
            &mut pool,
            &bump,
            Some(&crate::types::TypeInterner::new()),
        );
        let stmt = func.block_stmts(BlockId::from_usize(3)).next().unwrap();
        match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(_)),
                ..
            } => panic!("phi with disagreeing preds must not fold to a constant"),
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Copy(t)),
                ..
            } => assert_eq!(t.as_usize(), 1),
            other => panic!("unexpected {other:?}"),
        }
    }
}
