//! Flow-derived, hash-stable borrow interfaces for function returns.
//!
//! The solver is an abstract interpretation over the existing AMIR/OSSA.  It
//! does not decide whether a loan is live (that remains `borrow_facts`); it
//! projects the same provenance relation onto a function boundary so callers
//! can continue that analysis without inspecting the callee body.

use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::amir::{
    AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt, AmirTerminator, TempId,
};
use crate::types::{
    ArType, BorrowPath, BorrowPathSegment, BorrowSource, ReturnBorrowDependency,
    ReturnBorrowSummary,
};
use arandu_typeck::TypeInfo;

type Origins = BTreeMap<BorrowPath, BTreeSet<BorrowSource>>;

/// A borrow-bearing result path for which no formal origin was demonstrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnprovenBorrowReturn {
    pub function: crate::SymbolId,
    pub result_path: BorrowPath,
}

/// Complete deterministic result of the interprocedural solver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BorrowInterfaceSolution {
    pub summaries: FxHashMap<crate::SymbolId, ReturnBorrowSummary>,
    pub unproven: Vec<UnprovenBorrowReturn>,
}

/// Derive return dependencies for every lowered function and annotate calls.
///
/// The least fixpoint starts with only external/signature-only contracts. A
/// recursive component therefore gains a dependency only when a real path in
/// the component introduces it. The finite domain is bounded by formal
/// parameters and structural borrow paths, so convergence is deterministic.
#[must_use]
pub fn solve_borrow_interfaces(
    program: &mut AmirProgram,
    type_info: &TypeInfo,
) -> BorrowInterfaceSolution {
    let local_symbols = program
        .funcs
        .iter()
        .map(|function| function.symbol)
        .collect::<FxHashSet<_>>();
    let mut summaries = type_info
        .return_borrow_summaries
        .iter()
        .filter(|(symbol, _)| !local_symbols.contains(symbol))
        .map(|(symbol, summary)| (*symbol, summary.clone()))
        .collect::<FxHashMap<_, _>>();

    let mut order = (0..program.funcs.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| {
        let symbol = program.funcs[index].symbol;
        (symbol.file_id, symbol.local_id.0)
    });

    // Each successful iteration adds at least one element from the finite
    // (function × result path × formal source) domain.
    let source_bound = program
        .funcs
        .iter()
        .map(|function| function.params.len().max(1))
        .sum::<usize>()
        .saturating_mul(TypeInfo::MAX_BORROW_PATHS)
        .max(1);
    for _ in 0..=source_bound {
        let mut changed = false;
        for &index in &order {
            let function = &program.funcs[index];
            let summary = infer_function_interface(function, type_info, &summaries).0;
            let old = summaries.get(&function.symbol);
            if old != Some(&summary) {
                summaries.insert(function.symbol, summary);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut unproven = Vec::new();
    for &index in &order {
        let function = &program.funcs[index];
        let (summary, missing) = infer_function_interface(function, type_info, &summaries);
        summaries.insert(function.symbol, summary);
        unproven.extend(missing.into_iter().map(|result_path| UnprovenBorrowReturn {
            function: function.symbol,
            result_path,
        }));
    }
    unproven.sort_by(|left, right| {
        (
            left.function.file_id,
            left.function.local_id.0,
            &left.result_path,
        )
            .cmp(&(
                right.function.file_id,
                right.function.local_id.0,
                &right.result_path,
            ))
    });
    unproven.dedup();

    for function in &mut program.funcs {
        let statement_ids = function.stmts.iter_ids().collect::<Vec<_>>();
        for statement_id in statement_ids {
            let Some(statement) = function.stmts.get_mut(statement_id) else {
                continue;
            };
            if let AmirStmt::Call {
                callee: AmirOperand::FunctionRef(symbol),
                return_borrow,
                ..
            } = statement
            {
                *return_borrow = summaries
                    .get(symbol)
                    .filter(|summary| !summary.dependencies.is_empty())
                    .cloned();
            }
        }
    }

    summaries.retain(|_, summary| !summary.dependencies.is_empty());
    BorrowInterfaceSolution {
        summaries,
        unproven,
    }
}

fn infer_function_interface(
    function: &AmirFunc,
    type_info: &TypeInfo,
    summaries: &FxHashMap<crate::SymbolId, ReturnBorrowSummary>,
) -> (ReturnBorrowSummary, Vec<BorrowPath>) {
    let expected = match type_info.borrow_paths(function.return_type) {
        Ok(expected) => expected,
        Err(_) => return (ReturnBorrowSummary::default(), vec![BorrowPath::root()]),
    };
    if expected.is_empty() {
        return (ReturnBorrowSummary::default(), Vec::new());
    }

    let mut temps = vec![Origins::new(); function.temps.len()];
    let mut locals = vec![Origins::new(); function.locals.len()];
    for (parameter_index, &temp) in function.params.iter().enumerate() {
        let Ok(parameter_index) = u32::try_from(parameter_index) else {
            continue;
        };
        let Some(parameter) = function.temps.get(temp.as_usize()) else {
            continue;
        };
        let Ok(paths) = type_info.borrow_paths(parameter.ty) else {
            continue;
        };
        for (path, _) in paths {
            temps[temp.as_usize()]
                .entry(path.clone())
                .or_default()
                .insert(BorrowSource {
                    parameter_index,
                    parameter_path: path,
                });
        }
    }

    let bound = function
        .temps
        .len()
        .saturating_add(function.locals.len())
        .saturating_mul(TypeInfo::MAX_BORROW_PATHS)
        .max(1);
    for _ in 0..=bound {
        let mut changed = false;
        for block in &function.blocks {
            for statement in function.block_stmts(block.id) {
                transfer_statement(
                    function,
                    statement,
                    type_info,
                    summaries,
                    &mut temps,
                    &mut locals,
                    &mut changed,
                );
            }
            transfer_terminator(function, &block.terminator, &mut temps, &mut changed);
        }
        if !changed {
            break;
        }
    }

    let returned = temps.first().cloned().unwrap_or_default();
    let mut summary = ReturnBorrowSummary::default();
    let mut missing = Vec::new();
    for (result_path, kind) in expected {
        let sources = returned
            .get(&result_path)
            .map(|sources| sources.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if sources.is_empty() {
            missing.push(result_path);
        } else {
            summary.dependencies.push(ReturnBorrowDependency {
                result_path,
                sources,
                kind,
            });
        }
    }
    summary.canonicalize();
    (summary, missing)
}

#[allow(clippy::too_many_arguments)]
fn transfer_statement(
    function: &AmirFunc,
    statement: &AmirStmt,
    type_info: &TypeInfo,
    summaries: &FxHashMap<crate::SymbolId, ReturnBorrowSummary>,
    temps: &mut [Origins],
    locals: &mut [Origins],
    changed: &mut bool,
) {
    match statement {
        AmirStmt::Assign { lhs, rhs } => {
            let mut produced = match rhs {
                AmirRvalue::Use(operand) => operand_origins(*operand, temps),
                AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => locals
                    .get(place.local.as_usize())
                    .cloned()
                    .unwrap_or_default(),
                AmirRvalue::RelativeBorrow { local, .. }
                | AmirRvalue::Load(crate::amir::AmirPlace { local, .. }) => {
                    locals.get(local.as_usize()).cloned().unwrap_or_default()
                }
                AmirRvalue::Tuple { items } => {
                    aggregate_origins(items, temps, BorrowPathSegment::Tuple)
                }
                AmirRvalue::Array { items } => {
                    let mut output = Origins::new();
                    for operand in items {
                        prefix_origins(
                            &mut output,
                            &operand_origins(*operand, temps),
                            BorrowPathSegment::ArrayElement,
                        );
                    }
                    output
                }
                AmirRvalue::StructLiteral { fields, .. } => {
                    let mut output = Origins::new();
                    for (name, operand) in fields {
                        prefix_origins(
                            &mut output,
                            &operand_origins(*operand, temps),
                            BorrowPathSegment::Field(name.clone()),
                        );
                    }
                    output
                }
                AmirRvalue::EnumConstruct {
                    variant_tag,
                    payload: Some(payload),
                } => {
                    let segment = function
                        .temps
                        .get(lhs.as_usize())
                        .map(|temp| type_info.type_interner.resolve(temp.ty))
                        .and_then(|ty| match ty {
                            ArType::Option(_) if *variant_tag == 1 => {
                                Some(BorrowPathSegment::OptionSome)
                            }
                            ArType::Result(_, _) if *variant_tag == 0 => {
                                Some(BorrowPathSegment::ResultOk)
                            }
                            ArType::Result(_, _) if *variant_tag == 1 => {
                                Some(BorrowPathSegment::ResultErr)
                            }
                            _ => u32::try_from(*variant_tag)
                                .ok()
                                .map(BorrowPathSegment::Variant),
                        });
                    let mut output = Origins::new();
                    if let Some(segment) = segment {
                        prefix_origins(&mut output, &operand_origins(*payload, temps), segment);
                    }
                    output
                }
                AmirRvalue::FieldAccess { base, field } => {
                    let name = operand_temp(*base)
                        .and_then(|temp| function.temps.get(temp.as_usize()))
                        .and_then(|temp| field_name(type_info, temp.ty, *field));
                    name.map_or_else(Origins::new, |name| {
                        strip_prefix(
                            &operand_origins(*base, temps),
                            &BorrowPathSegment::Field(name),
                        )
                    })
                }
                AmirRvalue::EnumPayload {
                    value,
                    variant,
                    index,
                } => {
                    let tag = type_info
                        .enum_variant_tags
                        .get(variant)
                        .copied()
                        .unwrap_or(0);
                    let base_ty = operand_temp(*value)
                        .and_then(|temp| function.temps.get(temp.as_usize()))
                        .map(|temp| type_info.type_interner.resolve(temp.ty));
                    let first = match base_ty {
                        Some(ArType::Option(_)) => BorrowPathSegment::OptionSome,
                        Some(ArType::Result(_, _)) if tag == 0 => BorrowPathSegment::ResultOk,
                        Some(ArType::Result(_, _)) => BorrowPathSegment::ResultErr,
                        _ => BorrowPathSegment::Variant(u32::try_from(tag).unwrap_or(u32::MAX)),
                    };
                    let origins = strip_prefix(&operand_origins(*value, temps), &first);
                    if matches!(base_ty, Some(ArType::Option(_) | ArType::Result(_, _))) {
                        origins
                    } else {
                        strip_prefix(
                            &origins,
                            &BorrowPathSegment::Payload(u32::try_from(*index).unwrap_or(u32::MAX)),
                        )
                    }
                }
                AmirRvalue::SliceView { owner, .. } | AmirRvalue::StrView { owner } => {
                    operand_origins(*owner, temps)
                }
                AmirRvalue::SliceSubslice { slice, .. } | AmirRvalue::SliceData(slice) => {
                    operand_origins(*slice, temps)
                }
                AmirRvalue::StrBytes { source } => operand_origins(*source, temps),
                _ => Origins::new(),
            };
            if let Some(target) = temps.get_mut(lhs.as_usize()) {
                *changed |= merge_origins(target, &mut produced);
            }
        }
        AmirStmt::Store { lhs, rhs } if lhs.projections.is_empty() => {
            let mut source = operand_origins(*rhs, temps);
            if let Some(target) = locals.get_mut(lhs.local.as_usize()) {
                *changed |= merge_origins(target, &mut source);
            }
        }
        AmirStmt::Call {
            lhs: Some(lhs),
            callee: AmirOperand::FunctionRef(symbol),
            args,
            ..
        } => {
            let Some(summary) = summaries.get(symbol) else {
                return;
            };
            let mut output = Origins::new();
            for dependency in &summary.dependencies {
                for source in &dependency.sources {
                    let Ok(index) = usize::try_from(source.parameter_index) else {
                        continue;
                    };
                    let Some(argument) = args.get(index) else {
                        continue;
                    };
                    let argument_origins = operand_origins(*argument, temps);
                    let inherited = argument_origins
                        .get(&source.parameter_path)
                        .cloned()
                        .unwrap_or_default();
                    output
                        .entry(dependency.result_path.clone())
                        .or_default()
                        .extend(inherited);
                }
            }
            if let Some(target) = temps.get_mut(lhs.as_usize()) {
                *changed |= merge_origins(target, &mut output);
            }
        }
        _ => {}
    }
}

fn transfer_terminator(
    function: &AmirFunc,
    terminator: &AmirTerminator,
    temps: &mut [Origins],
    changed: &mut bool,
) {
    let mut transfer = |target: crate::amir::BlockId, args: &[AmirOperand]| {
        let Some(block) = function.blocks.get(target.as_usize()) else {
            return;
        };
        let block_params = function.block_params(block.params);
        for (parameter, argument) in block_params.iter().zip(args) {
            let mut source = operand_origins(*argument, temps);
            if let Some(target) = temps.get_mut(parameter.id.as_usize()) {
                *changed |= merge_origins(target, &mut source);
            }
        }
    };
    match terminator {
        AmirTerminator::Goto { target, args } => transfer(*target, args),
        AmirTerminator::Suspend { resume, args, .. } => transfer(*resume, args),
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => {
            transfer(*if_true, true_args);
            transfer(*if_false, false_args);
        }
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            for (_, target, args) in targets {
                transfer(*target, args);
            }
            transfer(otherwise.0, &otherwise.1);
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}

fn operand_temp(operand: AmirOperand) -> Option<TempId> {
    match operand {
        AmirOperand::Copy(temp) | AmirOperand::Move(temp) => Some(temp),
        _ => None,
    }
}

fn operand_origins(operand: AmirOperand, temps: &[Origins]) -> Origins {
    operand_temp(operand)
        .and_then(|temp| temps.get(temp.as_usize()))
        .cloned()
        .unwrap_or_default()
}

fn aggregate_origins(
    operands: &[AmirOperand],
    temps: &[Origins],
    segment: impl Fn(u32) -> BorrowPathSegment,
) -> Origins {
    let mut output = Origins::new();
    for (index, operand) in operands.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        prefix_origins(
            &mut output,
            &operand_origins(*operand, temps),
            segment(index),
        );
    }
    output
}

fn prefix_origins(output: &mut Origins, input: &Origins, segment: BorrowPathSegment) {
    for (path, origins) in input {
        let mut prefixed = smallvec::SmallVec::with_capacity(path.0.len() + 1);
        prefixed.push(segment.clone());
        prefixed.extend(path.0.iter().cloned());
        output
            .entry(BorrowPath(prefixed))
            .or_default()
            .extend(origins.iter().cloned());
    }
}

fn strip_prefix(input: &Origins, segment: &BorrowPathSegment) -> Origins {
    let mut output = Origins::new();
    for (path, origins) in input {
        if path.0.first() == Some(segment) {
            output
                .entry(BorrowPath(path.0[1..].into()))
                .or_default()
                .extend(origins.iter().cloned());
        } else if path.0.is_empty() {
            output
                .entry(BorrowPath::root())
                .or_default()
                .extend(origins.iter().cloned());
        }
    }
    output
}

fn merge_origins(target: &mut Origins, source: &mut Origins) -> bool {
    let mut changed = false;
    for (path, origins) in source.iter() {
        let target_origins = target.entry(path.clone()).or_default();
        let old_len = target_origins.len();
        target_origins.extend(origins.iter().cloned());
        changed |= target_origins.len() != old_len;
    }
    changed
}

fn field_name(
    type_info: &TypeInfo,
    type_id: crate::types::TypeId,
    index: usize,
) -> Option<smol_str::SmolStr> {
    let mut resolved = type_info.type_interner.resolve(type_id);
    while let ArType::Ref(inner) | ArType::RefMut(inner) = resolved {
        resolved = type_info.type_interner.resolve(inner);
    }
    let ArType::Named(symbol, _) = resolved else {
        return None;
    };
    type_info
        .struct_fields
        .get(&symbol)?
        .iter()
        .find(|f| f.index == index)
        .map(|f| f.name.clone())
}
