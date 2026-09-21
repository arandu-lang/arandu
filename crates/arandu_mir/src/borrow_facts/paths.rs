//! Loan collection and carrier path extraction.

use super::propagation::{operand_temp, propagate_terminator_args};
use super::types::{HolderPath, HolderProjection, Loan, LoanKind};
use crate::BitSet;
use crate::amir::reachability::terminator_targets;
use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::types::{BorrowPath, BorrowPathSegment};
use std::collections::{BTreeSet, VecDeque};

pub(crate) fn collect_loans(func: &AmirFunc) -> (Vec<Loan>, Vec<u32>) {
    let num_temps = func.temps.len();
    let num_locals = func.locals.len();
    let mut loans = Vec::new();
    let mut borrow_site_counts = vec![0u32; func.blocks.len()];

    for block in &func.blocks {
        let bi = block.id.as_usize();
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, rhs } = stmt {
                match rhs {
                    AmirRvalue::Borrow(place) => {
                        borrow_site_counts[bi] += 1;
                        loans.push(new_loan(
                            LoanKind::Shared,
                            place.clone(),
                            *lhs,
                            block.id,
                            false,
                            num_temps,
                            num_locals,
                        ));
                    }
                    AmirRvalue::BorrowMut(place) => {
                        borrow_site_counts[bi] += 1;
                        loans.push(new_loan(
                            LoanKind::Exclusive,
                            place.clone(),
                            *lhs,
                            block.id,
                            false,
                            num_temps,
                            num_locals,
                        ));
                    }
                    // A3.4: same loan as absolute borrow of that local.
                    AmirRvalue::RelativeBorrow { local, mutable } => {
                        borrow_site_counts[bi] += 1;
                        loans.push(new_loan(
                            if *mutable {
                                LoanKind::Exclusive
                            } else {
                                LoanKind::Shared
                            },
                            crate::amir::AmirPlace {
                                local: *local,
                                projections: smallvec::SmallVec::new(),
                            },
                            *lhs,
                            block.id,
                            true,
                            num_temps,
                            num_locals,
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    // Propagate holders: copies of the reference value alias the same loan.
    let mut worklist = VecDeque::new();
    let num_blocks = func.blocks.len();
    let mut in_worklist = vec![false; num_blocks];
    for block in &func.blocks {
        worklist.push_back(block.id);
        in_worklist[block.id.as_usize()] = true;
    }

    let mut guard = 0;
    while let Some(block_id) = worklist.pop_front() {
        let index = block_id.as_usize();
        in_worklist[index] = false;
        guard += 1;
        if guard >= crate::analysis_limits::BORROW_FACTS_ITERATION_GUARD {
            // The domain is finite and monotone. Reaching this defensive bound
            // means malformed AMIR, so retain the conservative facts collected
            // so far instead of panicking in production compiler code.
            break;
        }

        let block = &func.blocks[index];
        let mut changed = false;

        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Assign { lhs, rhs } => {
                    for loan in &mut loans {
                        let produced = rvalue_holder_paths(rhs, loan);
                        changed |= merge_temp_paths(loan, *lhs, produced);
                    }
                }
                AmirStmt::Store { lhs, rhs } => {
                    if let Some(src) = operand_temp(rhs) {
                        let prefix = place_path(lhs);
                        for loan in &mut loans {
                            let source = loan
                                .holder_temp_paths
                                .get(src.as_usize())
                                .cloned()
                                .unwrap_or_default();
                            let produced = prefix_paths(&source, &prefix);
                            changed |= merge_local_paths(loan, lhs.local, produced);
                        }
                    }
                }
                AmirStmt::Call {
                    lhs: Some(lhs),
                    args,
                    return_borrow: Some(dependency),
                    ..
                } => {
                    for loan in &mut loans {
                        let mut output = BTreeSet::new();
                        for result in &dependency.dependencies {
                            for source in &result.sources {
                                let Ok(argument_index) = usize::try_from(source.parameter_index)
                                else {
                                    continue;
                                };
                                let Some(source_temp) =
                                    args.get(argument_index).and_then(operand_temp)
                                else {
                                    continue;
                                };
                                let input = loan
                                    .holder_temp_paths
                                    .get(source_temp.as_usize())
                                    .cloned()
                                    .unwrap_or_default();
                                let selected = strip_paths(
                                    &input,
                                    &holder_path_from_contract(&source.parameter_path),
                                );
                                output.extend(prefix_paths(
                                    &selected,
                                    &holder_path_from_contract(&result.result_path),
                                ));
                            }
                        }
                        changed |= merge_temp_paths(loan, *lhs, output);
                    }
                }
                _ => {}
            }
        }
        // Terminator args → successor block params (phi-like).
        match &block.terminator {
            AmirTerminator::Goto { target, args } => {
                propagate_terminator_args(func, *target, args, &mut loans, &mut changed);
            }
            AmirTerminator::Suspend { resume, args, .. } => {
                propagate_terminator_args(func, *resume, args, &mut loans, &mut changed);
            }
            AmirTerminator::Branch {
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                propagate_terminator_args(func, *if_true, true_args, &mut loans, &mut changed);
                propagate_terminator_args(func, *if_false, false_args, &mut loans, &mut changed);
            }
            AmirTerminator::SwitchInt {
                targets, otherwise, ..
            } => {
                for (_, tgt, args) in targets {
                    propagate_terminator_args(func, *tgt, args, &mut loans, &mut changed);
                }
                propagate_terminator_args(
                    func,
                    otherwise.0,
                    &otherwise.1,
                    &mut loans,
                    &mut changed,
                );
            }
            AmirTerminator::Return | AmirTerminator::Unreachable => {}
        }

        if changed {
            for successor in terminator_targets(&block.terminator) {
                let succ_index = successor.as_usize();
                if !in_worklist[succ_index] {
                    worklist.push_back(successor);
                    in_worklist[succ_index] = true;
                }
            }
        }
    }

    (loans, borrow_site_counts)
}

pub(crate) fn new_loan(
    kind: LoanKind,
    place: crate::amir::AmirPlace,
    holder: TempId,
    origin_block: BlockId,
    relative: bool,
    num_temps: usize,
    num_locals: usize,
) -> Loan {
    let crate::amir::AmirPlace {
        local: place_local,
        projections: place_projections,
    } = place;
    let mut holder_temps = BitSet::with_capacity(num_temps);
    holder_temps.insert(holder);
    let mut holder_temp_paths = vec![BTreeSet::new(); num_temps];
    if let Some(paths) = holder_temp_paths.get_mut(holder.as_usize()) {
        paths.insert(HolderPath::default());
    }
    Loan {
        kind,
        place_local,
        place_projections,
        holder_temps,
        holder_locals: BitSet::with_capacity(num_locals),
        holder_temp_paths,
        holder_local_paths: vec![BTreeSet::new(); num_locals],
        relative,
        origin_block,
    }
}

fn rvalue_holder_paths(rhs: &AmirRvalue, loan: &Loan) -> BTreeSet<HolderPath> {
    match rhs {
        AmirRvalue::Use(operand) | AmirRvalue::BlackBox { value: operand, .. } => {
            operand_holder_paths(*operand, loan)
        }
        AmirRvalue::SliceView { owner, .. } | AmirRvalue::StrView { owner } => {
            operand_holder_paths(*owner, loan)
        }
        AmirRvalue::SliceSubslice { slice, .. } | AmirRvalue::SliceData(slice) => {
            operand_holder_paths(*slice, loan)
        }
        AmirRvalue::StrBytes { source } => operand_holder_paths(*source, loan),
        AmirRvalue::Load(place) => {
            let input = loan
                .holder_local_paths
                .get(place.local.as_usize())
                .cloned()
                .unwrap_or_default();
            strip_paths(&input, &place_path(place))
        }
        AmirRvalue::Tuple { items } => aggregate_holder_paths(items, loan, HolderProjection::Slot),
        AmirRvalue::Array { items } => {
            aggregate_holder_paths(items, loan, |_| HolderProjection::Element)
        }
        AmirRvalue::StructLiteral { fields, .. } => {
            let operands = fields
                .iter()
                .map(|(_, operand)| *operand)
                .collect::<Vec<_>>();
            aggregate_holder_paths(&operands, loan, HolderProjection::Slot)
        }
        AmirRvalue::FieldAccess { base, field } => strip_paths(
            &operand_holder_paths(*base, loan),
            &HolderPath(vec![HolderProjection::Slot(
                u32::try_from(*field).unwrap_or(u32::MAX),
            )]),
        ),
        AmirRvalue::IndexAccess { base, .. } => strip_paths(
            &operand_holder_paths(*base, loan),
            &HolderPath(vec![HolderProjection::Element]),
        ),
        AmirRvalue::EnumConstruct {
            variant_tag,
            payload: Some(payload),
        } => {
            let prefix = HolderPath(vec![
                HolderProjection::Variant(u32::try_from(*variant_tag).unwrap_or(u32::MAX)),
                HolderProjection::Payload(0),
            ]);
            prefix_paths(&operand_holder_paths(*payload, loan), &prefix)
        }
        AmirRvalue::EnumPayload {
            value,
            variant: _,
            index,
        } => {
            let input = operand_holder_paths(*value, loan);
            // The tag is deliberately wildcarded here: AMIR identifies the
            // selected variant separately, while this local domain only needs
            // the payload slot to preserve the holder safely.
            strip_variant_payload(&input, u32::try_from(*index).unwrap_or(u32::MAX))
        }
        AmirRvalue::CoroutineReady { value, .. } => prefix_paths(
            &operand_holder_paths(*value, loan),
            &HolderPath(vec![HolderProjection::CoroutinePayload]),
        ),
        AmirRvalue::Borrow(_)
        | AmirRvalue::BorrowMut(_)
        | AmirRvalue::RelativeBorrow { .. }
        | AmirRvalue::Binary { .. }
        | AmirRvalue::Unary { .. }
        | AmirRvalue::Discriminant { .. }
        | AmirRvalue::Len(_)
        | AmirRvalue::Alloc(_)
        | AmirRvalue::EnumConstruct { payload: None, .. }
        | AmirRvalue::GenInsert { .. }
        | AmirRvalue::GenGet { .. }
        | AmirRvalue::GenSet { .. }
        | AmirRvalue::GenUpsert { .. }
        | AmirRvalue::GenRemove { .. }
        | AmirRvalue::StringInterp { .. }
        | AmirRvalue::ToStr { .. } => BTreeSet::new(),
    }
}

fn operand_holder_paths(operand: AmirOperand, loan: &Loan) -> BTreeSet<HolderPath> {
    operand_temp(&operand)
        .and_then(|temp| loan.holder_temp_paths.get(temp.as_usize()))
        .cloned()
        .unwrap_or_default()
}

fn aggregate_holder_paths(
    operands: &[AmirOperand],
    loan: &Loan,
    segment: impl Fn(u32) -> HolderProjection,
) -> BTreeSet<HolderPath> {
    let mut output = BTreeSet::new();
    for (index, operand) in operands.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        output.extend(prefix_paths(
            &operand_holder_paths(*operand, loan),
            &HolderPath(vec![segment(index)]),
        ));
    }
    output
}

pub(crate) fn merge_temp_paths(loan: &mut Loan, temp: TempId, paths: BTreeSet<HolderPath>) -> bool {
    let Some(target) = loan.holder_temp_paths.get_mut(temp.as_usize()) else {
        return false;
    };
    let old_len = target.len();
    target.extend(paths);
    let changed = target.len() != old_len;
    if !target.is_empty() {
        loan.holder_temps.insert(temp);
    }
    changed
}

pub(crate) fn merge_local_paths(
    loan: &mut Loan,
    local: LocalId,
    paths: BTreeSet<HolderPath>,
) -> bool {
    let Some(target) = loan.holder_local_paths.get_mut(local.as_usize()) else {
        return false;
    };
    let old_len = target.len();
    target.extend(paths);
    let changed = target.len() != old_len;
    if !target.is_empty() {
        loan.holder_locals.insert(local);
    }
    changed
}

pub(crate) fn prefix_paths(
    input: &BTreeSet<HolderPath>,
    prefix: &HolderPath,
) -> BTreeSet<HolderPath> {
    input
        .iter()
        .map(|path| {
            let mut projections = Vec::with_capacity(prefix.0.len() + path.0.len());
            projections.extend(prefix.0.iter().cloned());
            projections.extend(path.0.iter().cloned());
            HolderPath(projections)
        })
        .collect()
}

fn strip_paths(input: &BTreeSet<HolderPath>, prefix: &HolderPath) -> BTreeSet<HolderPath> {
    input
        .iter()
        .filter(|path| path.0.starts_with(&prefix.0))
        .map(|path| HolderPath(path.0[prefix.0.len()..].to_vec()))
        .collect()
}

fn strip_variant_payload(input: &BTreeSet<HolderPath>, index: u32) -> BTreeSet<HolderPath> {
    input
        .iter()
        .filter_map(|path| match path.0.as_slice() {
            [
                HolderProjection::Variant(_),
                HolderProjection::Payload(found),
                rest @ ..,
            ] if *found == index => Some(HolderPath(rest.to_vec())),
            _ => None,
        })
        .collect()
}

pub(crate) fn place_path(place: &crate::amir::AmirPlace) -> HolderPath {
    HolderPath(
        place
            .projections
            .iter()
            .map(|projection| match projection {
                crate::amir::AmirProjection::Field(symbol) => HolderProjection::NamedField {
                    file_id: symbol.file_id,
                    local_id: symbol.local_id.0,
                },
                crate::amir::AmirProjection::Index(_) => HolderProjection::Element,
                crate::amir::AmirProjection::Deref => HolderProjection::Deref,
            })
            .collect(),
    )
}

fn holder_path_from_contract(path: &BorrowPath) -> HolderPath {
    HolderPath(
        path.0
            .iter()
            .map(|segment| match segment {
                BorrowPathSegment::Tuple(index) => HolderProjection::Slot(*index),
                BorrowPathSegment::Payload(index) => HolderProjection::Payload(*index),
                BorrowPathSegment::Field(name) => {
                    // Exported contracts use names so they survive recompilation.
                    // Local AMIR field accesses use slots; a stable digest keeps
                    // named paths distinct without introducing a hash map order.
                    let mut digest = 2_166_136_261_u32;
                    for byte in name.as_bytes() {
                        digest ^= u32::from(*byte);
                        digest = digest.wrapping_mul(16_777_619);
                    }
                    HolderProjection::NamedField {
                        file_id: u32::MAX,
                        local_id: digest,
                    }
                }
                BorrowPathSegment::Variant(tag) => HolderProjection::Variant(*tag),
                BorrowPathSegment::OptionSome => HolderProjection::OptionSome,
                BorrowPathSegment::ResultOk => HolderProjection::ResultOk,
                BorrowPathSegment::ResultErr => HolderProjection::ResultErr,
                BorrowPathSegment::ArrayElement => HolderProjection::Element,
                BorrowPathSegment::NullableValue => HolderProjection::NullableValue,
                BorrowPathSegment::CoroutinePayload => HolderProjection::CoroutinePayload,
                BorrowPathSegment::PollReady => HolderProjection::PollReady,
                BorrowPathSegment::RangeElement => HolderProjection::RangeElement,
            })
            .collect(),
    )
}

pub(crate) type LocalHolderState = Vec<Vec<BTreeSet<HolderPath>>>;
