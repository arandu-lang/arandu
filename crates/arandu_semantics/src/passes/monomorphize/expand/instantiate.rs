use super::super::graph::InstantiationKey;
use super::concrete::{instance_args_fully_concrete, is_identity_instantiation};
use super::func::specialize_func;
use super::rewrite::rewrite_block_calls;
use arandu_diagnostics::{DiagCode, Diagnostic};
use arandu_middle::hir::{HirDecl, HirProgram};
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::{ArType, build_subst_ids, substitute_type_id};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

/// Concrete root plus unit-local symbols for the signatures of its callees.
/// Their structural keys, not those incidental symbols, cross unit boundaries.
#[derive(Debug)]
pub struct InstantiatedFunction {
    pub function: SymbolId,
    pub instances: Vec<(SymbolId, arandu_middle::types::FunctionInstance)>,
    /// Unit-local allocations, separate from every source symbol identity.
    pub generated_symbols: Vec<SymbolId>,
}

/// Specialize only the selected body. Callees are represented by concrete HIR
/// signatures; their bodies are never read or expanded by this producer.
pub fn instantiate_function(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    instance: &arandu_middle::types::FunctionInstance,
) -> Result<InstantiatedFunction, Vec<Diagnostic>> {
    use arandu_middle::types::TypeShape;
    let source_symbols = tc
        .symbols
        .iter()
        .map(|symbol| symbol.id)
        .collect::<rustc_hash::FxHashSet<_>>();
    let unit_span = hir.span;
    let failure = |message: &str| {
        vec![Diagnostic::error(
            DiagCode::G002GenericInstantiationLimit,
            message,
            unit_span,
        )]
    };
    let arguments = instance
        .arguments
        .iter()
        .map(|shape| shape.intern(&tc.type_info.type_interner))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| failure("instance type arguments exceed the structural type bounds"))?;
    if !instance_args_fully_concrete(tc, &arguments) {
        return Err(failure(
            "function instance retains unresolved type arguments",
        ));
    }
    let bump = bumpalo::Bump::new();
    let root = hir
        .decls
        .iter()
        .enumerate()
        .find_map(|(index, &id)| match hir.pool.decl(id) {
            HirDecl::Func(function)
                if function.symbol == instance.definition && function.body.is_some() =>
            {
                Some((index, function.body))
            }
            _ => None,
        })
        .ok_or_else(|| {
            vec![Diagnostic::ice(
                DiagCode::ICEL001,
                "instance definition has no typed body",
                hir.span,
            )]
        })?;
    // Preserve the original static generic-cycle guard before removing the
    // template body from the instance's executable declaration list.
    super::super::collect::analyze_instantiations(tc, hir, &bump)?;
    let key = InstantiationKey {
        symbol: instance.definition,
        type_args: bump.alloc_slice_copy(&arguments),
    };
    let mut specialized = FxHashMap::default();
    let (function, body) = if tc
        .type_info
        .generic_params
        .contains_key(&instance.definition)
    {
        let templates = FxHashMap::from_iter([(instance.definition, root.0)]);
        let (symbol, body) =
            specialize_func(tc, hir, &key, &templates, true, true).map_err(|error| vec![error])?;
        let Some(body) = body else {
            return Err(vec![Diagnostic::ice(
                DiagCode::ICEL001,
                "concrete instance lost its body",
                hir.span,
            )]);
        };
        specialized.insert(key, symbol);
        for &id in &hir.decls {
            if let Some(HirDecl::Func(template)) = hir.pool.decls.get_mut(id)
                && template.symbol == instance.definition
            {
                template.body = None;
            }
        }
        (symbol, body)
    } else {
        if !arguments.is_empty() {
            return Err(vec![Diagnostic::error(
                DiagCode::G002GenericInstantiationLimit,
                "non-generic function cannot have instance arguments",
                hir.span,
            )]);
        }
        let Some(body) = root.1 else {
            return Err(vec![Diagnostic::ice(
                DiagCode::ICEL001,
                "typed function lost its body",
                hir.span,
            )]);
        };
        (instance.definition, body)
    };
    let templates = hir
        .decls
        .iter()
        .enumerate()
        .filter_map(|(index, &id)| match hir.pool.decl(id) {
            HirDecl::Func(function)
                if tc.type_info.generic_params.contains_key(&function.symbol) =>
            {
                Some((function.symbol, index))
            }
            _ => None,
        })
        .collect::<FxHashMap<_, _>>();
    let graph = super::super::collect::analyze_instantiations(tc, hir, &bump)?;
    let mut keys = graph.iter().map(|node| node.key).collect::<Vec<_>>();
    let mut destructor_receivers = Vec::new();
    // Compiler-inserted drops have no source Call. Discover concrete nominal
    // receiver types from this unit's own typed HIR, never another body.
    let mut observed = hir
        .pool
        .exprs
        .iter()
        .map(|expr| expr.ty)
        .chain(
            hir.decls
                .iter()
                .filter_map(|&id| match hir.pool.decl(id) {
                    HirDecl::Func(root) if root.symbol == function => Some(root.params),
                    _ => None,
                })
                .flat_map(|params| {
                    hir.pool
                        .params_list(params)
                        .iter()
                        .map(|parameter| parameter.ty)
                }),
        )
        .collect::<Vec<_>>();
    let mut visited_types = rustc_hash::FxHashSet::default();
    while let Some(ty) = observed.pop() {
        if !visited_types.insert(ty) {
            continue;
        }
        if visited_types.len() > 4096 {
            return Err(failure(
                "instance cleanup type graph exceeds its structural bounds",
            ));
        }
        TypeShape::from_id(ty, &tc.type_info.type_interner)
            .map_err(|_| failure("instance cleanup types exceed the structural type bounds"))?;
        let resolved = tc.type_info.type_interner.resolve(ty);
        // A returned wrapper can own a generic field without mentioning that
        // field's concrete type in this body (BitSet owns Vec<u64>). Discover
        // the structural closure from declaration metadata, not callee bodies.
        match &resolved {
            ArType::Named(nominal, arguments) => {
                let args = tc.type_info.type_interner.type_args(*arguments);
                observed.extend_from_slice(&args);
                if let Some(fields) = tc.type_info.struct_fields.get(nominal) {
                    for field in fields.iter() {
                        if let Some(field_ty) = arandu_middle::layout::instantiated_field_type(
                            &resolved,
                            &field.name,
                            &tc.type_info.type_interner,
                            tc.type_info.as_ref(),
                        ) {
                            observed.push(field_ty);
                        }
                    }
                }
                let subst = tc
                    .type_info
                    .generic_params
                    .get(nominal)
                    .map(|params| build_subst_ids(params, &args, &tc.type_info.type_interner))
                    .unwrap_or_default();
                let mut variants = tc
                    .type_info
                    .enum_variants
                    .iter()
                    .filter(|(_, (owner, _))| owner == nominal)
                    .collect::<Vec<_>>();
                variants.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
                for (_, (_, payload)) in variants {
                    if let arandu_typeck::type_checker::EnumPayloadShape::Tuple(fields) = payload {
                        observed.extend(fields.iter().map(|&field| {
                            substitute_type_id(field, &subst, &tc.type_info.type_interner)
                        }));
                    }
                }
            }
            ArType::Func(args, result) => {
                observed.extend_from_slice(&tc.type_info.type_interner.type_args(*args));
                observed.push(*result);
            }
            ArType::Tuple(args) => {
                observed.extend_from_slice(&tc.type_info.type_interner.type_args(*args))
            }
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
            | ArType::Range(inner) => observed.push(*inner),
            ArType::Result(ok, error) => observed.extend([*ok, *error]),
            ArType::Primitive(_)
            | ArType::Vector(_)
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
        let ArType::Named(nominal, arguments) = resolved else {
            continue;
        };
        let Some(&destructor) = tc.type_info.destructors.get(&nominal) else {
            continue;
        };
        if arguments.is_empty() || !templates.contains_key(&destructor) {
            continue;
        }
        let arguments = tc.type_info.type_interner.type_args(arguments);
        let normalized = arguments
            .iter()
            .map(|&ty| {
                let mut shape = TypeShape::from_id(ty, &tc.type_info.type_interner)?;
                shape.default_numeric_literals()?;
                shape.intern(&tc.type_info.type_interner)
            })
            .collect::<Result<Vec<_>, _>>();
        if !normalized.is_ok_and(|arguments| instance_args_fully_concrete(tc, &arguments)) {
            continue;
        }
        let key = InstantiationKey {
            symbol: destructor,
            type_args: bump.alloc_slice_copy(&arguments),
        };
        destructor_receivers.push((ty, key));
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let mut instances = Vec::new();
    if function != instance.definition {
        instances.push((function, instance.clone()));
    }
    for original_key in keys {
        let arguments = original_key
            .type_args
            .iter()
            .map(|&ty| {
                let mut shape = TypeShape::from_id(ty, &tc.type_info.type_interner)?;
                shape.default_numeric_literals()?;
                shape.intern(&tc.type_info.type_interner)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| failure("callee type arguments exceed the structural type bounds"))?;
        let key = InstantiationKey {
            symbol: original_key.symbol,
            type_args: bump.alloc_slice_copy(&arguments),
        };
        if let Some(&callee) = specialized.get(&key) {
            specialized.insert(original_key, callee);
            continue;
        }
        if specialized.contains_key(&key)
            || !templates.contains_key(&key.symbol)
            || is_identity_instantiation(tc, key.symbol, key.type_args)
        {
            continue;
        }
        if !instance_args_fully_concrete(tc, key.type_args) {
            return Err(vec![Diagnostic::error(
                DiagCode::G002GenericInstantiationLimit,
                "function instance retains unresolved type arguments",
                hir.span,
            )]);
        }
        let (callee, _) =
            specialize_func(tc, hir, &key, &templates, false, true).map_err(|error| vec![error])?;
        let arguments = key
            .type_args
            .iter()
            .map(|&ty| TypeShape::from_id(ty, &tc.type_info.type_interner))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                vec![Diagnostic::error(
                    DiagCode::G002GenericInstantiationLimit,
                    "callee instance type arguments exceed the structural type bounds",
                    hir.span,
                )]
            })?;
        instances.push((
            callee,
            arandu_middle::types::FunctionInstance {
                definition: key.symbol,
                arguments,
            },
        ));
        specialized.insert(key, callee);
        specialized.insert(original_key, callee);
    }
    for (receiver, key) in destructor_receivers {
        if let Some(&destructor) = specialized.get(&key) {
            // Source HIR can retain defaulted numeric pseudo-types in nominal
            // arguments. Preserve cleanup for that equivalent receiver shape.
            tc.type_info_mut()
                .destructor_instances
                .insert(receiver, destructor);
        }
    }
    rewrite_block_calls(hir, body, &specialized, tc, &bump);
    Ok(InstantiatedFunction {
        function,
        instances,
        generated_symbols: tc
            .symbols
            .iter()
            .filter(|symbol| !source_symbols.contains(&symbol.id))
            .map(|symbol| symbol.id)
            .collect(),
    })
}
