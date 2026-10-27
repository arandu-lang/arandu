//! Demand-driven executable composition. Bodies remain independent Salsa
//! producers; this query only discovers instances and rebases their artifacts.

use super::{declaration_hir, runtime_unit, Instance};
use crate::{db::HashEq, ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::amir::{AmirOperand, AmirProgram};
use arandu_middle::hir::HirDecl;
use arandu_middle::types::FunctionInstance;
use arandu_middle::{DiagCode, Diagnostic, Span, SymbolId};
use rustc_hash::FxHashSet;

#[derive(Debug)]
pub struct RuntimeProgram {
    pub artifacts: std::sync::Arc<crate::passes::LowerAmirArtifacts>,
    pub instances: Vec<(SymbolId, FunctionInstance)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl StableHash for RuntimeProgram {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"RuntimeProgram/v1");
        hash.update(self.artifacts.stable_hash().as_bytes());
        hash.update(self.diagnostics.stable_hash().as_bytes());
        for (symbol, key) in &self.instances {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash.update(key.stable_hash().as_bytes());
        }
        hash.finalize()
    }
}

/// N021 for every defined function whose native name an earlier one (in
/// symbol order) already uses. Backends would otherwise fail on the second
/// definition with an ICE or leave it to the linker.
fn duplicate_function_symbols(
    db: &dyn ArandCompilerDb,
    amir: &AmirProgram,
    symbols: &arandu_middle::SymbolTable,
) -> Vec<Diagnostic> {
    let mut by_name = std::collections::BTreeMap::<&str, Vec<&arandu_middle::Symbol>>::new();
    for function in &amir.funcs {
        if let Some(symbol) = symbols.try_get(function.symbol) {
            by_name
                .entry(symbols.host_func_name(symbol))
                .or_default()
                .push(symbol);
        }
    }
    let mut diagnostics = Vec::new();
    for (name, mut definitions) in by_name {
        definitions.sort_by_key(|symbol| (symbol.id.file_id, symbol.id.local_id.0));
        definitions.dedup_by_key(|symbol| symbol.id);
        let Some((first, rest)) = definitions.split_first() else {
            continue;
        };
        for other in rest {
            // Two `@Export`s claiming one name are T049, reported with the
            // export context by `exported_symbol_collision` consumers.
            if symbols.is_exported(first.id) && symbols.is_exported(other.id) {
                continue;
            }
            let diagnostic = Diagnostic::error(
                DiagCode::N021DuplicateFunctionSymbol,
                format!(
                    "functions `{}` and `{}` map to the same native symbol `{name}`",
                    first.name, other.name
                ),
                other.span,
            );
            // A secondary label renders against the primary file's text.
            diagnostics.push(if first.span.file_id == other.span.file_id {
                diagnostic.with_label(first.span, "the other function with this symbol")
            } else {
                diagnostic.with_note(format!(
                    "`{}` is defined in {}",
                    first.name,
                    db.file_path(first.span.file_id).display()
                ))
            });
        }
    }
    diagnostics
}

fn empty_program() -> AmirProgram {
    AmirProgram {
        funcs: Vec::new(),
        literal_pool: Default::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "runtime_program", file = ?file.file_id(db)))]
pub fn runtime_program(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<RuntimeProgram> {
    const MAX_INSTANCES: usize = 4096;
    let declarations = declaration_hir(db, file);
    let resolution = crate::passes::resolve(db, file);
    let mut aggregate = declarations.artifacts.type_check.clone();
    let mut diagnostics = declarations.artifacts.diagnostics.clone();
    let mut pending = std::collections::VecDeque::new();
    let mut queued = FxHashSet::default();
    if let Some(hir) = &declarations.artifacts.hir {
        for &decl in &hir.decls {
            let HirDecl::Func(function) = hir.pool.decl(decl) else {
                continue;
            };
            if aggregate
                .type_info
                .generic_params
                .contains_key(&function.symbol)
            {
                // Uninstantiated entry templates still receive their ordinary
                // source/body diagnostics. They are not executable units.
                diagnostics.extend(
                    crate::ctfe::item_staged_typing(db, file, function.symbol)
                        .diagnostics
                        .iter()
                        .cloned(),
                );
                continue;
            }
            let key = FunctionInstance {
                definition: function.symbol,
                arguments: Vec::new(),
            };
            if queued.insert(key.clone()) {
                if queued.len() > MAX_INSTANCES {
                    diagnostics.push(Diagnostic::error(
                        DiagCode::G002GenericInstantiationLimit,
                        "runtime instance graph exceeds its unit limit",
                        hir.span,
                    ));
                    break;
                }
                pending.push_back(Instance::new(db, file, key));
            }
        }
    }
    let mut units = Vec::new();
    while let Some(instance) = pending.pop_front() {
        db.unwind_if_revision_cancelled();
        let key = instance.key(db).clone();
        let checked = runtime_unit(db, instance);
        match &checked.result {
            Ok(unit) => {
                let mut callees = Vec::new();
                let mut references = FxHashSet::default();
                let mut visit = |operand: &AmirOperand| {
                    if let AmirOperand::FunctionRef(symbol) = operand {
                        references.insert(*symbol);
                    }
                };
                for id in unit.function.stmts.iter_ids() {
                    if let Some(statement) = unit.function.stmts.get(id) {
                        arandu_middle::amir::visit::for_each_stmt_operand(statement, &mut visit);
                    }
                }
                for block in &unit.function.blocks {
                    arandu_middle::amir::visit::for_each_terminator_operand(
                        &block.terminator,
                        &mut visit,
                    );
                }
                for symbol in references {
                    if checked
                        .context
                        .symbols
                        .try_get(symbol)
                        .is_some_and(|symbol| {
                            matches!(
                                symbol.kind,
                                arandu_middle::SymbolKind::ExternFunc
                                    | arandu_middle::SymbolKind::NamespaceMember
                            )
                        })
                    {
                        continue;
                    }
                    callees.push(
                        checked
                            .instances
                            .iter()
                            .find(|(id, _)| *id == symbol)
                            .map_or_else(
                                || FunctionInstance {
                                    definition: symbol,
                                    arguments: Vec::new(),
                                },
                                |(_, key)| key.clone(),
                            ),
                    );
                }
                // Gen payload drop thunks can reference a destructor without
                // a source/AMIR call. Its concrete signature is still a real
                // executable dependency, not a guessed backend-only recipe.
                for destructor in checked.context.type_info.destructor_instances.values() {
                    let key = checked
                        .instances
                        .iter()
                        .find(|(id, _)| id == destructor)
                        .map_or_else(
                            || FunctionInstance {
                                definition: *destructor,
                                arguments: Vec::new(),
                            },
                            |(_, key)| key.clone(),
                        );
                    if !callees.contains(&key) {
                        callees.push(key);
                    }
                }
                callees.sort_by_key(|key| {
                    (
                        db.source_file_by_id(key.definition.file_id)
                            .map(|sf| sf.path(db).clone())
                            .unwrap_or_else(|| std::sync::Arc::new(std::path::PathBuf::new())),
                        key.definition.local_id.0,
                        *key.stable_hash().as_bytes(),
                    )
                });
                for called in callees {
                    if queued.contains(&called) {
                        continue;
                    }
                    if queued.len() >= MAX_INSTANCES {
                        diagnostics.push(Diagnostic::error(
                            DiagCode::G002GenericInstantiationLimit,
                            "runtime instance graph exceeds its unit limit",
                            Span::new(called.definition.file_id, 0, 0),
                        ));
                        break;
                    }
                    let Some(source) = db.source_file_by_id(called.definition.file_id) else {
                        diagnostics.push(Diagnostic::ice(
                            DiagCode::ICEL001,
                            "called function definition is not registered",
                            Span::new(called.definition.file_id, 0, 0),
                        ));
                        continue;
                    };
                    queued.insert(called.clone());
                    pending.push_back(Instance::new(db, source, called));
                }
            }
            Err(errors) => diagnostics.extend(errors.iter().cloned()),
        }
        units.push((key, checked.clone()));
    }
    let file_path_key = |file_id: arandu_middle::db::FileId| {
        db.source_file_by_id(file_id)
            .map(|sf| sf.path(db).clone())
            .unwrap_or_else(|| std::sync::Arc::new(std::path::PathBuf::new()))
    };
    units.sort_by_key(|(key, _)| {
        (
            file_path_key(key.definition.file_id),
            key.definition.local_id.0,
            *key.stable_hash().as_bytes(),
        )
    });
    let composed = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error)
    {
        None
    } else {
        let inputs = units
            .iter()
            .filter_map(|(key, checked)| {
                let unit = checked.result.as_ref().ok()?;
                Some(arandu_semantics::ContextualFunctionUnit {
                    key,
                    unit,
                    context: &checked.context,
                    generated_symbols: &checked.generated_symbols,
                    instances: &checked.instances,
                    borrow_summary: checked.borrow_summary.as_ref()?,
                })
            })
            .collect::<Vec<_>>();
        if inputs.len() != units.len() {
            diagnostics.push(Diagnostic::ice(
                DiagCode::ICEL001,
                "validated runtime unit has no borrow summary",
                Span::new(*file.file_id(db), 0, 0),
            ));
            None
        } else {
            match arandu_semantics::compose_function_units(&mut aggregate, &inputs, || {
                db.unwind_if_revision_cancelled()
            }) {
                Ok(composed) => Some(composed),
                Err(errors) => {
                    diagnostics.extend(errors);
                    None
                }
            }
        }
    };
    let (mut amir, instances) = match composed {
        Some(composed) => {
            diagnostics.extend(composed.diagnostics);
            (composed.program, composed.instances)
        }
        None => (empty_program(), Vec::new()),
    };
    diagnostics.extend(duplicate_function_symbols(db, &amir, &aggregate.symbols));
    // The unit composition path above is what the real pipeline uses, so the
    // C status-code rewrite for `@Export` functions returning `Result<T, E>`
    // has to run here as well. The pass itself lives in `arandu_mir` and stays
    // pure; this crate only orchestrates it.
    arandu_semantics::passes::lower_amir::export_status::lower_export_status_returns(
        &mut amir,
        &aggregate,
        &mut diagnostics,
    );
    HashEq::new(RuntimeProgram {
        artifacts: std::sync::Arc::new(crate::passes::LowerAmirArtifacts {
            amir,
            type_check: aggregate,
            docs: std::sync::Arc::new(resolution.docs.clone()),
        }),
        instances,
        diagnostics,
    })
}
