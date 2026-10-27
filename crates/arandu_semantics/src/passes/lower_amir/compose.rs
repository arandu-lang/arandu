//! Pure composition of independently validated bodies. Source symbols keep
//! their compound identities; only unit-local monomorphization allocations
//! are rebased. Literal/SSA handling stays in the canonical MIR visitors.

use super::*;
use arandu_middle::types::{FunctionInstance, ReturnBorrowSummary, TypeId, TypeShape};
use rustc_hash::FxHashSet;

pub struct ContextualFunctionUnit<'a> {
    pub key: &'a FunctionInstance,
    pub unit: &'a FunctionUnit,
    pub context: &'a TypeCheckResult,
    pub generated_symbols: &'a [SymbolId],
    pub instances: &'a [(SymbolId, FunctionInstance)],
    pub borrow_summary: &'a ReturnBorrowSummary,
}

pub struct ComposedUnits {
    pub program: AmirProgram,
    pub diagnostics: Vec<Diagnostic>,
    pub instances: Vec<(SymbolId, FunctionInstance)>,
}

/// The inputs must have passed final ownership validation in their own
/// contexts. The composer cannot turn a raw body into a safety certificate.
pub fn compose_function_units(
    aggregate: &mut TypeCheckResult,
    units: &[ContextualFunctionUnit<'_>],
    mut poll: impl FnMut(),
) -> Result<ComposedUnits, Vec<Diagnostic>> {
    let mut canonical = FxHashMap::<FunctionInstance, SymbolId>::default();
    let mut diagnostics = Vec::new();
    // Positions of a file's symbols are current only in contexts of its own
    // units; an importer observes imported declarations by surface and may
    // keep an older copy (parameter and local spans feed debug info).
    // Registration keeps the first copy, so own-file symbols go first.
    for input in units {
        poll();
        let own = &input.context.symbols;
        if own.file_id == aggregate.symbols.file_id || own.file_id != input.key.definition.file_id {
            continue;
        }
        for source in own
            .iter_own()
            .filter(|symbol| !input.generated_symbols.contains(&symbol.id))
        {
            poll();
            if aggregate.symbols.try_get(source.id).is_none() {
                aggregate
                    .symbols_mut()
                    .register_imported_symbol(source.clone());
            }
        }
    }
    // Register every source symbol before allocating aggregate-local symbols.
    // The entry file's source table must already be complete; importing an ID
    // from that same file cannot extend its dense source table.
    for input in units {
        poll();
        let generated = input
            .generated_symbols
            .iter()
            .copied()
            .collect::<FxHashSet<_>>();
        for source in input
            .context
            .symbols
            .iter()
            .filter(|symbol| !generated.contains(&symbol.id))
        {
            poll();
            if aggregate.symbols.try_get(source.id).is_none() {
                if source.id.file_id == aggregate.symbols.file_id {
                    if source.id.is_synthetic() {
                        aggregate.symbols_mut().register_synthetic(source.clone());
                    } else {
                        return Err(vec![ice(
                            "composition is missing an entry-file source symbol",
                            source.span,
                        )]);
                    }
                } else {
                    aggregate
                        .symbols_mut()
                        .register_imported_symbol(source.clone());
                }
            }
            if let Some(name) = input.context.symbols.host_function_names.get(&source.id) {
                aggregate
                    .symbols_mut()
                    .host_function_names
                    .insert(source.id, name.clone());
            }
            if input
                .context
                .symbols
                .tooling_entrypoints
                .contains(&source.id)
            {
                aggregate
                    .symbols_mut()
                    .tooling_entrypoints
                    .insert(source.id);
            }
            // An `@Export` of an imported module is a linker root too; the
            // flat name alone would keep it internal and hide name clashes.
            if input.context.symbols.is_exported(source.id) {
                aggregate.symbols_mut().exported_functions.insert(source.id);
            }
        }
        aggregate.symbols_mut().module_members.extend(
            input
                .context
                .symbols
                .module_members
                .iter()
                .map(|(key, value)| (key.clone(), *value)),
        );
        aggregate.symbols_mut().associated_members.extend(
            input
                .context
                .symbols
                .associated_members
                .iter()
                .map(|(key, value)| (key.clone(), *value)),
        );
        aggregate.symbols_mut().type_params.extend(
            input
                .context
                .symbols
                .type_params
                .iter()
                .map(|(key, value)| (*key, value.clone())),
        );
    }
    for input in units {
        poll();
        if input.key.arguments.is_empty() {
            canonical.insert(input.key.clone(), input.key.definition);
        }
    }
    // Allocations are scoped and structural-key deduplicated. An incidental
    // symbol number from one unit never aliases another unit's definition.
    for input in units {
        poll();
        for (symbol, key) in input.instances {
            if canonical.contains_key(key) {
                continue;
            }
            let source = input.context.symbols.try_get(*symbol).ok_or_else(|| {
                vec![ice(
                    "instance header has no symbol",
                    input
                        .unit
                        .function
                        .temps
                        .first()
                        .map_or(Span::new(0, 0, 0), |temp| temp.span),
                )]
            })?;
            let global = aggregate.symbols.global_scope();
            let scope = aggregate.symbols_mut().new_scope(global);
            let mapped = aggregate
                .symbols_mut()
                .define(scope, &source.name, source.kind, source.span)
                .map_err(|_| vec![ice("instance symbol allocation collided", source.span)])?;
            if let Some(name) = input.context.symbols.host_function_names.get(symbol) {
                aggregate
                    .symbols_mut()
                    .host_function_names
                    .insert(mapped, name.clone());
            }
            canonical.insert(key.clone(), mapped);
            if input.context.symbols.tooling_entrypoints.contains(symbol) {
                aggregate.symbols_mut().tooling_entrypoints.insert(mapped);
            }
        }
    }
    let mut program = AmirProgram {
        funcs: Vec::new(),
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: FxHashMap::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };
    let mut proven_summaries = Vec::with_capacity(units.len());
    for input in units {
        poll();
        let mut symbols = FxHashMap::default();
        for (local, key) in input.instances {
            let Some(&mapped) = canonical.get(key) else {
                return Err(vec![ice(
                    "instance has no canonical symbol",
                    Span::new(key.definition.file_id, 0, 0),
                )]);
            };
            symbols.insert(*local, mapped);
        }
        let mut generated = input.generated_symbols.to_vec();
        generated.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
        let global = aggregate.symbols.global_scope();
        let mut generated_scopes = FxHashMap::default();
        for symbol in generated {
            if symbols.contains_key(&symbol) {
                continue;
            }
            let source = input.context.symbols.try_get(symbol).ok_or_else(|| {
                vec![ice(
                    "generated symbol is missing",
                    Span::new(symbol.file_id, 0, 0),
                )]
            })?;
            if !matches!(
                source.kind,
                arandu_middle::SymbolKind::Local | arandu_middle::SymbolKind::Param
            ) {
                return Err(vec![ice(
                    "unmapped generated declaration in a function unit",
                    source.span,
                )]);
            }
            // Generated locals retain their source lexical buckets. Flattening
            // a unit into one scope aliases independent `j` bindings in two
            // selected static-loop bodies. All uses are already resolved by ID;
            // these scopes carry names/debug metadata, not a second resolver.
            let scope = *generated_scopes
                .entry(source.scope)
                .or_insert_with(|| aggregate.symbols_mut().new_scope(global));
            let mapped = aggregate
                .symbols_mut()
                .define(scope, &source.name, source.kind, source.span)
                .map_err(|_| vec![ice("generated local allocation collided", source.span)])?;
            symbols.insert(symbol, mapped);
        }
        let remap_symbol = |symbol: SymbolId| symbols.get(&symbol).copied().unwrap_or(symbol);
        // Nominal metadata is source-owned. Specialization allocates only
        // functions/parameters/locals, so it cannot rebind named type symbols.
        let include_expressions = input.key.definition.file_id == aggregate.symbols.file_id;
        aggregate.type_info_mut().merge_codegen_context(
            &input.context.type_info,
            remap_symbol,
            include_expressions,
        );
        let mut unit = input.unit.clone();
        let mut types = FxHashMap::<TypeId, TypeId>::default();
        let mut invalid = false;
        arandu_middle::amir::visit::for_each_function_context_mut(
            std::sync::Arc::make_mut(&mut unit.function),
            |ty| {
                if let Some(&mapped) = types.get(ty) {
                    *ty = mapped;
                    return;
                }
                match TypeShape::from_id(*ty, &input.context.type_info.type_interner)
                    .and_then(|shape| shape.intern(&aggregate.type_info.type_interner))
                {
                    Ok(mapped) => {
                        types.insert(*ty, mapped);
                        *ty = mapped;
                    }
                    Err(_) => invalid = true,
                }
            },
            |symbol| *symbol = remap_symbol(*symbol),
        );
        if invalid {
            return Err(vec![ice(
                "function unit contains an invalid type domain",
                Span::new(input.key.definition.file_id, 0, 0),
            )]);
        }
        for binding in &mut unit.debug_bindings {
            binding.function = remap_symbol(binding.function);
        }
        for block in &mut unit.debug_blocks {
            block.function = remap_symbol(block.function);
        }
        let function = unit.function.symbol;
        proven_summaries.push((function, input.borrow_summary));
        diagnostics.extend(append_function_unit(&mut program, unit).map_err(|error| vec![error])?);
    }
    // Later units carry declaration seeds for earlier functions. Install
    // proven summaries only after every header merge so those seeds cannot
    // overwrite a body-derived contract (including an empty least element).
    for (function, summary) in proven_summaries {
        aggregate
            .type_info_mut()
            .return_borrow_summaries
            .remove(&function);
        if !summary.dependencies.is_empty() {
            aggregate
                .type_info_mut()
                .return_borrow_summaries
                .insert(function, summary.clone());
        }
    }
    for symbol in aggregate.symbols.iter() {
        poll();
        if symbol.kind != arandu_middle::SymbolKind::ExternFunc {
            continue;
        }
        if let Some(ArType::Func(params, result)) = aggregate.type_info.decl_type(symbol.id) {
            program.extern_funcs.insert(
                symbol.id,
                (
                    aggregate
                        .type_info
                        .type_interner
                        .type_args(params)
                        .into_iter()
                        .map(|ty| aggregate.type_info.type_interner.resolve(ty))
                        .collect(),
                    aggregate.type_info.type_interner.resolve(result),
                ),
            );
        }
    }
    let mut instances = canonical
        .into_iter()
        .map(|(key, symbol)| (symbol, key))
        .collect::<Vec<_>>();
    instances.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    Ok(ComposedUnits {
        program,
        diagnostics,
        instances,
    })
}

fn ice(message: &str, span: Span) -> Diagnostic {
    Diagnostic::ice(DiagCode::ICEL001, message, span)
}
