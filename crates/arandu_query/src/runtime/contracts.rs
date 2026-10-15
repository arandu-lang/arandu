//! Contract convergence uses independently lowered bodies in their own type
//! domains. Only structural instance keys and borrow paths cross those domains.

use super::{runtime_raw_unit, Instance, RuntimeUnit};
use crate::{db::HashEq, ArandCompilerDb, StableHash};
use arandu_middle::amir::{AmirOperand, AmirStmt};
use arandu_middle::types::{ArType, FunctionInstance, ReturnBorrowSummary};
use arandu_middle::{DiagCode, Diagnostic, Span, SymbolId};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Debug)]
pub struct InstanceContracts {
    pub entries: Vec<(FunctionInstance, ReturnBorrowSummary)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl StableHash for InstanceContracts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"InstanceContracts/v1");
        for (key, summary) in &self.entries {
            hash.update(key.stable_hash().as_bytes());
            crate::stable_hash::hash_return_borrow_summary(&mut hash, summary);
        }
        hash.update(self.diagnostics.stable_hash().as_bytes());
        hash.finalize()
    }
}

fn callee_key(unit: &RuntimeUnit, symbol: SymbolId) -> FunctionInstance {
    unit.instances
        .iter()
        .find(|(id, _)| *id == symbol)
        .map_or_else(
            || FunctionInstance {
                definition: symbol,
                arguments: Vec::new(),
            },
            |(_, key)| key.clone(),
        )
}

fn local_contracts(
    unit: &RuntimeUnit,
    contracts: &FxHashMap<FunctionInstance, ReturnBorrowSummary>,
) -> FxHashMap<SymbolId, ReturnBorrowSummary> {
    let mut local = unit.context.type_info.return_borrow_summaries.clone();
    if let Ok(raw) = &unit.result {
        for id in raw.function.stmts.iter_ids() {
            if let Some(AmirStmt::Call {
                callee: AmirOperand::FunctionRef(symbol),
                ..
            }) = raw.function.stmts.get(id)
            {
                if let Some(summary) = contracts.get(&callee_key(unit, *symbol)) {
                    local.insert(*symbol, summary.clone());
                } else if !unit
                    .context
                    .symbols
                    .try_get(*symbol)
                    .is_some_and(|symbol| symbol.kind == arandu_middle::SymbolKind::ExternFunc)
                {
                    // A signature-compatible formal is not evidence of flow.
                    // Internal SCCs start at the least element, not at the
                    // signature checker's conservative seed.
                    local.remove(symbol);
                }
            }
        }
    }
    local
}

/// Solve the borrow-bearing call closure without a recursive Salsa query or
/// whole-program HIR. Scalar callees cannot contribute a return dependency and
/// are not lowered here. Failed/unbounded graphs never publish guessed origins.
#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "instance_contracts")
)]
pub fn instance_contracts<'db>(
    db: &'db dyn ArandCompilerDb,
    root: Instance<'db>,
) -> HashEq<InstanceContracts> {
    const MAX_UNITS: usize = 4096;
    let mut pending = std::collections::VecDeque::from([root]);
    let mut seen = FxHashSet::from_iter([root.key(db).clone()]);
    let mut units = Vec::new();
    let mut diagnostics = Vec::new();
    while let Some(instance) = pending.pop_front() {
        db.unwind_if_revision_cancelled();
        let key = instance.key(db).clone();
        let unit = runtime_raw_unit(db, instance);
        let raw = match &unit.result {
            Ok(raw) => raw,
            Err(errors) => {
                diagnostics.extend(errors.iter().cloned());
                break;
            }
        };
        for id in raw.function.stmts.iter_ids() {
            let Some(AmirStmt::Call {
                callee: AmirOperand::FunctionRef(symbol),
                ..
            }) = raw.function.stmts.get(id)
            else {
                continue;
            };
            let Some(signature) = unit.context.type_info.decl_type_id(*symbol) else {
                continue;
            };
            let Some(ArType::Func(_, result)) =
                unit.context.type_info.type_interner.try_resolve(signature)
            else {
                continue;
            };
            if unit
                .context
                .type_info
                .borrow_paths(result)
                .is_ok_and(|paths| paths.is_empty())
            {
                continue;
            }
            let called = callee_key(unit, *symbol);
            if unit
                .context
                .symbols
                .try_get(called.definition)
                .is_some_and(|symbol| symbol.kind == arandu_middle::SymbolKind::ExternFunc)
            {
                continue;
            }
            if seen.contains(&called) {
                continue;
            }
            if seen.len() >= MAX_UNITS {
                diagnostics.push(Diagnostic::error(
                    DiagCode::G002GenericInstantiationLimit,
                    "borrow contract instance graph exceeds its unit limit",
                    Span::new(called.definition.file_id, 0, 0),
                ));
                break;
            }
            let Some(file) = db.source_file_by_id(called.definition.file_id) else {
                diagnostics.push(Diagnostic::ice(
                    DiagCode::ICEL001,
                    "borrow contract definition is not registered",
                    Span::new(called.definition.file_id, 0, 0),
                ));
                continue;
            };
            seen.insert(called.clone());
            pending.push_back(Instance::new(db, file, called));
        }
        units.push((key, HashEq::share(unit)));
    }
    if !diagnostics.is_empty() {
        return HashEq::new(InstanceContracts {
            entries: Vec::new(),
            diagnostics,
        });
    }
    // Deterministic structural ordering; hashes are not semantic identities.
    // Definitions are ordered first. For one definition, stable structural
    // hashes only order its distinct keys; equality/dedup remains structural.
    units.sort_by_key(|(key, _)| {
        (
            key.definition.file_id,
            key.definition.local_id.0,
            *key.stable_hash().as_bytes(),
        )
    });
    let mut contracts = FxHashMap::default();
    let bound = units
        .iter()
        .filter_map(|(_, unit)| unit.result.as_ref().ok())
        .map(|unit| unit.function.params.len().max(1))
        .sum::<usize>()
        .saturating_mul(arandu_typeck::TypeInfo::MAX_BORROW_PATHS)
        .max(1);
    let mut converged = false;
    for _ in 0..=bound {
        let mut changed = false;
        for (key, unit) in &units {
            db.unwind_if_revision_cancelled();
            let Ok(raw) = &unit.result else {
                continue;
            };
            let local = local_contracts(unit, &contracts);
            let summary = arandu_mir::borrow_interface::infer_function_interface(
                &raw.function,
                &unit.context.type_info,
                &local,
            )
            .0;
            if contracts.get(key) != Some(&summary) {
                contracts.insert(key.clone(), summary);
                changed = true;
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        diagnostics.push(Diagnostic::ice(
            DiagCode::ICEL001,
            "instance borrow contracts did not converge",
            Span::new(root.key(db).definition.file_id, 0, 0),
        ));
    }
    let entries = units
        .into_iter()
        .filter_map(|(key, _)| contracts.remove(&key).map(|summary| (key, summary)))
        .collect();
    HashEq::new(InstanceContracts {
        entries,
        diagnostics,
    })
}

/// Independently validated runtime body. The output retains its unit context
/// and literal pool; a backend may not reinterpret IDs using another unit.
#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "runtime_unit")
)]
pub fn runtime_unit<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
) -> HashEq<RuntimeUnit> {
    let raw = runtime_raw_unit(db, instance);
    if raw.result.is_err() {
        return HashEq::share(raw);
    }
    let contracts = instance_contracts(db, instance);
    let mut result = raw.result.clone();
    let mut borrow_summary = contracts
        .entries
        .iter()
        .find(|(key, _)| key == instance.key(db))
        .map(|(_, summary)| summary.clone());
    let mut analysis_function = None;
    if contracts.diagnostics.is_empty() {
        if let Ok(unit) = &mut result {
            let summaries = contracts
                .entries
                .iter()
                .cloned()
                .collect::<FxHashMap<_, _>>();
            let local = local_contracts(raw, &summaries);
            let validation = arandu_mir::finalize_function_unit(unit, &raw.context, &local);
            analysis_function = Some(std::sync::Arc::clone(&unit.function));
            match validation {
                Ok(summary) => borrow_summary = Some(summary),
                Err(errors) => result = Err(errors),
            }
        }
    } else {
        result = Err(contracts.diagnostics.clone());
    }
    HashEq::new(RuntimeUnit {
        result,
        context: std::sync::Arc::clone(&raw.context),
        instances: raw.instances.clone(),
        generated_symbols: raw.generated_symbols.clone(),
        borrow_summary,
        analysis_function,
    })
}
