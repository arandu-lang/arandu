//! Exclusive use of an existing `mut ref` while a borrow derived from it is
//! live (Rust's reborrow rule).
//!
//! A borrow taken through a `mut ref` — a slice from `r.asSlice()`, or a copy
//! of `r` itself — keeps pointing into the referent. Passing `r` to a `mut ref`
//! formal (`r.push(x)`) may reallocate or overwrite that referent, so it is a
//! conflict while any such derived borrow is still used afterwards. Loans
//! created by `&mut x` in this body already cover the local case; this rule
//! also covers references received as parameters, which carry no local loan.
//!
//! Lowering passes a `mut ref` to a `ref` formal through a `ref`-typed copy, so
//! an argument typed `mut ref` is always an exclusive use.

use crate::BitSet;
use crate::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, BlockId, TempId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::types::{ArType, TypeInterner};
use rustc_hash::FxHashSet;

pub(super) fn check_exclusive_reborrows(
    func: &AmirFunc,
    interner: &TypeInterner,
    live_before: &[Vec<BitSet<TempId>>],
) -> Vec<(BlockId, Diagnostic)> {
    let is_exclusive = |temp: TempId| {
        matches!(
            interner.resolve(func.temps[temp.as_usize()].ty),
            ArType::RefMut(_)
        )
    };
    let roots = derived_roots(func, &is_exclusive);
    let mut diagnostics = Vec::new();
    for block in &func.blocks {
        let Some(points) = live_before.get(block.id.as_usize()) else {
            continue;
        };
        for (index, stmt) in func.block_stmts(block.id).enumerate() {
            let AmirStmt::Call { lhs, args, .. } = stmt else {
                continue;
            };
            let Some(live_after) = points.get(index + 1) else {
                continue;
            };
            for arg in args {
                let (AmirOperand::Copy(reference) | AmirOperand::Move(reference)) = arg else {
                    continue;
                };
                if !is_exclusive(*reference) {
                    continue;
                }
                let reference_roots = &roots[reference.as_usize()];
                // The call's own result borrows from this argument by design.
                let derived_live = live_after.iter().any(|other| {
                    other != *reference
                        && Some(other) != *lhs
                        && (roots[other.as_usize()].contains(reference)
                            || !reference_roots.is_disjoint(&roots[other.as_usize()]))
                });
                if derived_live {
                    diagnostics.push((
                        block.id,
                        Diagnostic::error(
                            DiagCode::O003MutableBorrowConflict,
                            "mutable use of a reference while a borrow derived from it is live",
                            func.temps[reference.as_usize()].span,
                        )
                        .with_label(
                            func.temps[reference.as_usize()].span,
                            "this call may change or reallocate what the derived borrow points into",
                        )
                        .with_note(
                            "finish using the slice or reference taken through it before this call",
                        ),
                    ));
                    break;
                }
            }
        }
    }
    diagnostics
}

/// For each temp, the `mut ref` temps its value was derived from through
/// copies, slice views and calls whose return borrows from an argument.
fn derived_roots(
    func: &AmirFunc,
    is_exclusive: &impl Fn(TempId) -> bool,
) -> Vec<FxHashSet<TempId>> {
    let mut roots = vec![FxHashSet::default(); func.temps.len()];
    let sources_of = |roots: &[FxHashSet<TempId>], operand: &AmirOperand| -> FxHashSet<TempId> {
        let (AmirOperand::Copy(temp) | AmirOperand::Move(temp)) = operand else {
            return FxHashSet::default();
        };
        let mut sources = roots[temp.as_usize()].clone();
        if is_exclusive(*temp) {
            sources.insert(*temp);
        }
        sources
    };
    let mut changed = true;
    while changed {
        changed = false;
        for stmt in func.stmts.payloads.iter() {
            let (lhs, sources) = match stmt {
                AmirStmt::Assign { lhs, rhs } => {
                    let operand = match rhs {
                        AmirRvalue::Use(operand)
                        | AmirRvalue::SliceSubslice { slice: operand, .. }
                        | AmirRvalue::SliceView { owner: operand, .. } => operand,
                        _ => continue,
                    };
                    (*lhs, sources_of(&roots, operand))
                }
                AmirStmt::Call {
                    lhs: Some(lhs),
                    args,
                    return_borrow: Some(summary),
                    ..
                } => {
                    let mut sources = FxHashSet::default();
                    for dependency in &summary.dependencies {
                        for source in &dependency.sources {
                            if let Some(arg) = args.get(source.parameter_index as usize) {
                                sources.extend(sources_of(&roots, arg));
                            }
                        }
                    }
                    (*lhs, sources)
                }
                _ => continue,
            };
            let entry = &mut roots[lhs.as_usize()];
            let before = entry.len();
            entry.extend(sources);
            changed |= entry.len() != before;
        }
    }
    roots
}
