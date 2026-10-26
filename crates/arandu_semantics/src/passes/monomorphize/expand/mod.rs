//! HIR monomorphization expand — free-function and method specialization.
//!
//! Pipeline step after [`super::analyze_instantiations`]:
//! 1. **Worklist** of concrete keys `(F, [T1, …])` (from the analysis graph,
//!    then nested callees discovered inside each specialization — same idea as
//!    rustc's monomorphization collector: specialize → scan body → enqueue).
//! 2. Clone each template body with type-parameter substitution and a mangled
//!    symbol.
//! 3. Rewrite call sites:
//!    - `Call(Generic(Path(F), [T1,…]), args)` → `Call(Path(F_spec), args)`
//!    - `Call(Generic(Field(recv, m), [T1,…]), args)` → same Path rewrite
//!      (receiver is already the first arg from HIR method lowering)
//! 4. Generic **templates** remain in the HIR for diagnostics/pretty-print but
//!    are skipped by AMIR lowering (see `lower_to_amir`).
//!
//! Nested free-func calls (`push_t<int>` → `ensure_cap<int>`) are discovered
//! only after the outer body is specialized (type args become concrete). A
//! single-pass expand over the static graph misses those — hence the worklist.

mod clone;
mod concrete;
mod discover;
mod func;
mod instantiate;
mod rewrite;

use super::graph::{InstantiationGraph, InstantiationKey};
use arandu_diagnostics::{DiagCode, Diagnostic};
use arandu_lexer::Span;
use arandu_middle::hir::{HirBlockId, HirDecl, HirExprId, HirProgram};
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::{ArType, TypeId};
use arandu_typeck::TypeCheckResult;
use concrete::{instance_args_fully_concrete, is_identity_instantiation, type_args_fully_concrete};
use discover::discover_nested_keys;
use func::{specialize_free_func, specialize_func};
pub use instantiate::{InstantiatedFunction, instantiate_function};
use rewrite::rewrite_block_calls;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;

/// Bind generic calls of an isolated CTFE root using concrete signatures only.
/// No callee body is traversed and no callable symbol is allocated for the root.
pub fn specialize_root_callees(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    root: super::collect::InstantiationRoot,
) -> Result<Vec<(SymbolId, arandu_middle::types::FunctionInstance)>, Vec<Diagnostic>> {
    use arandu_middle::types::{FunctionInstance, TypeShape};
    let bump = bumpalo::Bump::new();
    let graph = super::collect::analyze_root_instantiations(tc, hir, root, &bump)?;
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
    let mut specialized = FxHashMap::default();
    let mut instances = Vec::new();
    for node in graph.iter() {
        let original = node.key;
        let arguments = original
            .type_args
            .iter()
            .map(|&ty| {
                let mut shape = TypeShape::from_id(ty, &tc.type_info.type_interner)?;
                shape.default_numeric_literals()?;
                shape.intern(&tc.type_info.type_interner)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                vec![Diagnostic::error(
                    DiagCode::G002GenericInstantiationLimit,
                    "root callee arguments exceed the structural type bounds",
                    hir.span,
                )]
            })?;
        let key = InstantiationKey {
            symbol: original.symbol,
            type_args: bump.alloc_slice_copy(&arguments),
        };
        if !templates.contains_key(&key.symbol)
            || is_identity_instantiation(tc, key.symbol, key.type_args)
        {
            continue;
        }
        if !instance_args_fully_concrete(tc, key.type_args) {
            return Err(vec![Diagnostic::error(
                DiagCode::G002GenericInstantiationLimit,
                "compile-time root callee retains unresolved type arguments",
                hir.span,
            )]);
        }
        let symbol = if let Some(&symbol) = specialized.get(&key) {
            symbol
        } else {
            let (symbol, _) = specialize_func(tc, hir, &key, &templates, false, true)
                .map_err(|error| vec![error])?;
            let arguments = key
                .type_args
                .iter()
                .map(|&ty| TypeShape::from_id(ty, &tc.type_info.type_interner))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    vec![Diagnostic::error(
                        DiagCode::G002GenericInstantiationLimit,
                        "root callee identity exceeds the structural type bounds",
                        hir.span,
                    )]
                })?;
            instances.push((
                symbol,
                FunctionInstance {
                    definition: key.symbol,
                    arguments,
                },
            ));
            specialized.insert(key, symbol);
            symbol
        };
        specialized.insert(original, symbol);
    }
    match root {
        super::collect::InstantiationRoot::Expression(expression) => {
            rewrite::rewrite_expr_calls(hir, expression, &specialized, tc, &bump)
        }
        super::collect::InstantiationRoot::Block(block) => {
            rewrite_block_calls(hir, block, &specialized, tc, &bump)
        }
    }
    Ok(instances)
}

/// Specialize an isolated staging expression in its own HIR/type domain. This
/// shares the ordinary monomorphizer's substitution and fresh-binding rules;
/// it does not request the enclosing runtime function or any callee body.
pub fn specialize_root_expression(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    expression: HirExprId,
    substitution: &arandu_middle::types::GenericSubst,
) -> Result<HirExprId, Diagnostic> {
    clone::clone_expr(
        hir,
        expression,
        substitution,
        &mut FxHashMap::default(),
        tc,
        "$ctfe",
    )
}

/// Block counterpart with an independent return target and fresh local IDs.
pub fn specialize_root_block(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    block: HirBlockId,
    substitution: &arandu_middle::types::GenericSubst,
) -> Result<HirBlockId, Diagnostic> {
    clone::clone_block(
        hir,
        block,
        substitution,
        &mut FxHashMap::default(),
        tc,
        "$ctfe",
    )
}

/// Expand free-function and method specializations; rewrite call sites in-place.
///
/// Returns the number of specialized functions appended to `hir`.
#[tracing::instrument(level = "debug", target = "arandu_semantics::mono", skip_all)]
pub fn expand_specializations<'bump>(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    graph: &InstantiationGraph<'bump>,
    bump: &'bump bumpalo::Bump,
) -> Result<usize, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();

    // Collect template free funcs: SymbolId → decl index in hir.decls
    let mut template_funcs: FxHashMap<SymbolId, usize> = FxHashMap::default();
    for (i, &decl_id) in hir.decls.iter().enumerate() {
        if let HirDecl::Func(f) = hir.pool.decl(decl_id)
            && f.body.is_some()
            && tc.type_info.generic_params.contains_key(&f.symbol)
        {
            template_funcs.insert(f.symbol, i);
        }
    }

    // Seed worklist from the analysis graph (concrete keys only).
    let mut worklist: VecDeque<InstantiationKey<'bump>> = graph
        .iter()
        .map(|n| n.key)
        .filter(|key| {
            template_funcs.contains_key(&key.symbol)
                && !is_identity_instantiation(tc, key.symbol, key.type_args)
        })
        .collect();

    // Destruction is compiler-inserted, so it has no source call for the
    // ordinary call collector to discover. Seed generic destructors from every
    // concrete nominal type observed by type checking. Sorting by interned ID
    // keeps specialization order independent from hash-map iteration.
    let mut observed_types: Vec<TypeId> = tc
        .type_info
        .expr_types
        .iter()
        .flatten()
        .copied()
        .chain(tc.type_info.decl_types.values().copied())
        .collect();
    observed_types.sort_by_key(|ty| ty.0);
    observed_types.dedup();
    for ty in observed_types {
        let ArType::Named(nominal, args) = tc.type_info.resolve_type_id(ty) else {
            continue;
        };
        let Some(&destructor) = tc.type_info.destructors.get(&nominal) else {
            continue;
        };
        if args.is_empty() || !template_funcs.contains_key(&destructor) {
            continue;
        }
        let arg_ids = tc.type_info.type_interner.type_args(args);
        let key = InstantiationKey {
            symbol: destructor,
            type_args: bump.alloc_slice_copy(&arg_ids),
        };
        if !worklist.iter().any(|existing| existing == &key) {
            worklist.push_back(key);
        }
    }

    if worklist.is_empty() {
        return Ok(0);
    }

    // key → specialized function symbol
    let mut specialized: FxHashMap<InstantiationKey<'bump>, SymbolId> = FxHashMap::default();
    let mut created = 0usize;
    // Cap nested discovery (same order as graph recursion limit).
    const MAX_SPECIALIZATIONS: usize = 4096;

    while let Some(key) = worklist.pop_front() {
        if specialized.contains_key(&key) {
            continue;
        }
        if !template_funcs.contains_key(&key.symbol)
            || is_identity_instantiation(tc, key.symbol, key.type_args)
            || !type_args_fully_concrete(tc, key.type_args)
        {
            continue;
        }
        if created >= MAX_SPECIALIZATIONS {
            diagnostics.push(Diagnostic::error(
                DiagCode::G002GenericInstantiationLimit,
                format!(
                    "monomorphize: specialization limit ({MAX_SPECIALIZATIONS}) exceeded while expanding nested free-func calls"
                ),
                Span::new(0, 0, 0),
            ));
            break;
        }

        match specialize_free_func(tc, hir, &key, &template_funcs) {
            Ok((sym, body_id)) => {
                specialized.insert(key, sym);
                created += 1;
                // Nested callees only become concrete after this clone+subst.
                discover_nested_keys(hir, body_id, tc, bump, &template_funcs, |nested| {
                    if !specialized.contains_key(&nested) && !worklist.iter().any(|k| k == &nested)
                    {
                        worklist.push_back(nested);
                    }
                });
            }
            Err(d) => diagnostics.push(d),
        }
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    // Rewrite call sites in every function body (templates + monomorphized + monomorphic).
    let decl_ids: Vec<_> = hir.decls.clone();
    for &decl_id in &decl_ids {
        let body = match hir.pool.decl(decl_id) {
            HirDecl::Func(f) => f.body,
            _ => None,
        };
        if let Some(body) = body {
            rewrite_block_calls(hir, body, &specialized, tc, bump);
        }
    }

    Ok(created)
}
