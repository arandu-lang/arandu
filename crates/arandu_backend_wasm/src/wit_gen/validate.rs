use arandu_middle::types::ArType;
use arandu_middle::{Diagnostic, SymbolId};
use rustc_hash::FxHashSet;

/// The component's export surface: `public` functions of the root module and
/// every `@Export`. Public API of imported modules and the stdlib is an
/// implementation detail, as with cargo-component's explicit world.
pub(crate) fn is_component_export(
    symbols: &arandu_middle::SymbolTable,
    function: SymbolId,
) -> bool {
    symbols.is_exported(function)
        || symbols.try_get(function).is_some_and(|symbol| {
            symbol.visibility == arandu_middle::Visibility::Public
                && symbol.id.file_id == symbols.file_id
        })
}

/// The Component Model has no v128 canonical value. Do not silently turn a
/// native vector (including one nested in an aggregate) into WIT `u32`.
pub(crate) fn validate_vector_boundaries(
    program: &arandu_middle::amir::AmirProgram,
    symbols: &arandu_middle::SymbolTable,
    interner: &arandu_middle::types::TypeInterner,
    layout_provider: &dyn arandu_middle::layout::StructLayoutProvider,
    live: Option<&FxHashSet<SymbolId>>,
) -> Result<(), Diagnostic> {
    let mut pending = Vec::new();
    for function in program
        .funcs
        .iter()
        .filter(|function| live.is_none_or(|set| set.contains(&function.symbol)))
    {
        if let Some(symbol) = symbols.try_get(function.symbol)
            && is_component_export(symbols, function.symbol)
        {
            let origin = symbol.span;
            pending.push((function.return_type, origin));
            pending.extend(
                function
                    .params
                    .iter()
                    .map(|param| (function.temps[param.as_usize()].ty, origin)),
            );
        }
    }
    let mut externs = program.extern_funcs.iter().collect::<Vec<_>>();
    externs.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    for (symbol, (params, result)) in externs {
        if live.is_none_or(|set| set.contains(symbol)) {
            let Some(declaration) = symbols.try_get(*symbol) else {
                return Err(Diagnostic::ice(
                    arandu_middle::diagnostics::DiagCode::ICEGEN002,
                    "missing external declaration in Component boundary metadata",
                    arandu_base::Span::new(symbol.file_id, 0, 0),
                ));
            };
            let origin = declaration.span;
            pending.extend(
                params
                    .iter()
                    .map(|ty| (interner.intern(ty.clone()), origin)),
            );
            pending.push((interner.intern(result.clone()), origin));
        }
    }
    let mut visited = FxHashSet::default();
    while let Some((ty, origin)) = pending.pop() {
        if !visited.insert(ty) {
            continue;
        }
        if visited.len() > 4096 {
            return Err(Diagnostic::ice(
                arandu_middle::diagnostics::DiagCode::ICEGEN002,
                "Component boundary types exceed their structural bounds",
                origin,
            ));
        }
        match interner.resolve(ty) {
            ArType::Vector(_) => {
                return Err(Diagnostic::error(
                    arandu_middle::diagnostics::DiagCode::U001FeatureNotSupported,
                    "native SIMD vectors cannot cross Component Model boundaries; expose scalar lanes or a list instead",
                    origin,
                ));
            }
            nominal @ ArType::Named(_, arguments) => {
                pending.extend(
                    interner
                        .type_args(arguments)
                        .into_iter()
                        .map(|ty| (ty, origin)),
                );
                if let Some(fields) = layout_provider.get_struct_fields_for_type(&nominal, interner)
                {
                    for field in fields.iter() {
                        if let Some(field_ty) = arandu_middle::layout::instantiated_field_type(
                            &nominal,
                            &field.name,
                            interner,
                            layout_provider,
                        ) {
                            pending.push((field_ty, origin));
                        }
                    }
                }
                if let Some(variants) =
                    layout_provider.get_enum_variants_for_type(&nominal, interner)
                {
                    for (tag, variant) in variants.iter().enumerate() {
                        if variant.payload_ty.is_some()
                            && let Some(payload) =
                                arandu_middle::layout::instantiated_enum_variant_payload_type(
                                    &nominal,
                                    tag,
                                    interner,
                                    layout_provider,
                                )
                        {
                            pending.push((payload, origin));
                        }
                    }
                }
            }
            ArType::Func(arguments, result) => {
                pending.extend(
                    interner
                        .type_args(arguments)
                        .into_iter()
                        .map(|ty| (ty, origin)),
                );
                pending.push((result, origin));
            }
            ArType::Tuple(arguments) => pending.extend(
                interner
                    .type_args(arguments)
                    .into_iter()
                    .map(|ty| (ty, origin)),
            ),
            ArType::Nullable(inner)
            | ArType::Slice(inner)
            | ArType::MutSlice(inner)
            | ArType::Array(_, inner)
            | ArType::ConstArray(_, inner)
            | ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Option(inner)
            | ArType::Coroutine(inner)
            | ArType::Poll(inner)
            | ArType::Range(inner) => pending.push((inner, origin)),
            ArType::Result(ok, error) => pending.extend([(ok, origin), (error, origin)]),
            ArType::Primitive(_)
            | ArType::FrozenConst(_)
            | ArType::Const(_)
            | ArType::ConstParam(_)
            | ArType::GenRef
            | ArType::Err
            | ArType::Void
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Error => {}
        }
    }
    Ok(())
}
