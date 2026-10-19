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

use arandu_diagnostics::{DiagCode, Diagnostic};
use arandu_lexer::Span;
use arandu_middle::hir::{
    HirBlockId, HirCatchHandler, HirCondition, HirDecl, HirExprId, HirExprKind, HirFunc,
    HirLambdaBody, HirMatchArmBody, HirParam, HirProgram, HirStmtKind,
};
use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_middle::types::{ArType, TypeId, build_subst_ids, substitute_type_id};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;

use super::graph::{InstantiationGraph, InstantiationKey};

mod clone;
mod rewrite;

use clone::clone_block;
use rewrite::rewrite_block_calls;

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
    super::collect::analyze_instantiations(tc, hir, &bump)?;
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
    let graph = super::collect::analyze_instantiations(tc, hir, &bump)?;
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

/// Walk a specialized body and report concrete instantiation keys for nested
/// free-func / method calls (Generic nodes or inferred mono).
fn discover_nested_keys<'bump>(
    hir: &HirProgram,
    block_id: HirBlockId,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
    template_funcs: &FxHashMap<SymbolId, usize>,
    mut enqueue: impl FnMut(InstantiationKey<'bump>),
) {
    fn visit_block<'bump>(
        hir: &HirProgram,
        block_id: HirBlockId,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        let blk = hir.pool.block(block_id);
        for &sid in hir.pool.stmt_list(blk.statements) {
            visit_stmt(hir, sid, tc, bump, template_funcs, enqueue);
        }
    }

    fn visit_stmt<'bump>(
        hir: &HirProgram,
        stmt_id: arandu_middle::hir::HirStmtId,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        let kind = &hir.pool.stmt(stmt_id).kind;
        match kind {
            HirStmtKind::VarDecl { value, .. }
            | HirStmtKind::Expr(value)
            | HirStmtKind::Free(value)
            | HirStmtKind::Set { value, .. } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
            }
            HirStmtKind::Return { values } => {
                for &e in hir.pool.expr_list(*values) {
                    visit_expr(hir, e, tc, bump, template_funcs, enqueue);
                }
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                visit_block(hir, *then_block, tc, bump, template_funcs, enqueue);
                if let Some(b) = else_block {
                    visit_block(hir, *b, tc, bump, template_funcs, enqueue);
                }
            }
            HirStmtKind::While { condition, body } => {
                visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                visit_block(hir, *body, tc, bump, template_funcs, enqueue);
            }
            HirStmtKind::For {
                body,
                comptime_bodies,
                ..
            } => {
                if let Some(bodies) = comptime_bodies {
                    for body in bodies {
                        visit_block(hir, *body, tc, bump, template_funcs, enqueue);
                    }
                } else {
                    visit_block(hir, *body, tc, bump, template_funcs, enqueue);
                }
            }
            HirStmtKind::Match { value, arms } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
                for arm in hir.pool.match_arms_list(*arms) {
                    if let Some(g) = arm.guard {
                        visit_expr(hir, g, tc, bump, template_funcs, enqueue);
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            visit_expr(hir, *e, tc, bump, template_funcs, enqueue)
                        }
                        HirMatchArmBody::Block(b) => {
                            visit_block(hir, *b, tc, bump, template_funcs, enqueue)
                        }
                    }
                }
            }
            HirStmtKind::Defer(b)
            | HirStmtKind::ErrDefer(b)
            | HirStmtKind::Unsafe(b)
            | HirStmtKind::Scope(b) => {
                visit_block(hir, *b, tc, bump, template_funcs, enqueue);
            }
            HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
        }
    }

    fn visit_condition<'bump>(
        hir: &HirProgram,
        cond: &HirCondition,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        match cond {
            HirCondition::Expr(e) | HirCondition::Is { expr: e, .. } => {
                visit_expr(hir, *e, tc, bump, template_funcs, enqueue);
            }
            HirCondition::And(conditions) => {
                for condition in conditions {
                    visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                }
            }
        }
    }

    fn maybe_enqueue<'bump>(
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        symbol: SymbolId,
        type_args: &[TypeId],
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        if !template_funcs.contains_key(&symbol) {
            return;
        }
        if is_identity_instantiation(tc, symbol, type_args)
            || !type_args_fully_concrete(tc, type_args)
        {
            return;
        }
        let type_args = bump.alloc_slice_copy(type_args);
        enqueue(InstantiationKey { symbol, type_args });
    }

    fn visit_expr<'bump>(
        hir: &HirProgram,
        expr_id: HirExprId,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        let expr = hir.pool.expr(expr_id);
        match &expr.kind {
            HirExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                visit_expr(hir, *callee, tc, bump, template_funcs, enqueue);
                for &a in hir.pool.expr_list(*args) {
                    visit_expr(hir, a, tc, bump, template_funcs, enqueue);
                }
                if let Some(b) = trailing_block {
                    visit_block(hir, *b, tc, bump, template_funcs, enqueue);
                }
                // Explicit Generic type args on the callee.
                if let HirExprKind::Generic {
                    callee: inner,
                    args: type_args,
                } = &hir.pool.expr(*callee).kind
                {
                    let symbol = match &hir.pool.expr(*inner).kind {
                        HirExprKind::Path { symbol } => Some(*symbol),
                        HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                        _ => None,
                    };
                    if let Some(symbol) = symbol {
                        maybe_enqueue(tc, bump, template_funcs, symbol, type_args, enqueue);
                    }
                } else if let Some((symbol, type_args_vec)) =
                    super::collect::instantiation_key_for_call(
                        hir, tc, *callee, *args, expr.ty, expr.span,
                    )
                {
                    maybe_enqueue(tc, bump, template_funcs, symbol, &type_args_vec, enqueue);
                }
            }
            HirExprKind::Generic { callee, args } => {
                visit_expr(hir, *callee, tc, bump, template_funcs, enqueue);
                if let Some(symbol) = match &hir.pool.expr(*callee).kind {
                    HirExprKind::Path { symbol } => Some(*symbol),
                    HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                    _ => None,
                } {
                    maybe_enqueue(tc, bump, template_funcs, symbol, args, enqueue);
                }
            }
            HirExprKind::Field { base, .. }
            | HirExprKind::SafeField { base, .. }
            | HirExprKind::Alloc { expr: base }
            | HirExprKind::Try { expr: base }
            | HirExprKind::Cast { expr: base, .. }
            | HirExprKind::Unary { expr: base, .. }
            | HirExprKind::ToStr { value: base }
            | HirExprKind::ResultCtor { value: base, .. } => {
                visit_expr(hir, *base, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::Index { base, index }
            | HirExprKind::SafeIndex { base, index }
            | HirExprKind::Binary {
                left: base,
                right: index,
                ..
            }
            | HirExprKind::NullCoalesce {
                left: base,
                right: index,
            } => {
                visit_expr(hir, *base, tc, bump, template_funcs, enqueue);
                visit_expr(hir, *index, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                visit_block(hir, *then_block, tc, bump, template_funcs, enqueue);
                visit_block(hir, *else_block, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::Match { value, arms } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
                for arm in hir.pool.match_arms_list(*arms) {
                    if let Some(g) = arm.guard {
                        visit_expr(hir, g, tc, bump, template_funcs, enqueue);
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            visit_expr(hir, *e, tc, bump, template_funcs, enqueue)
                        }
                        HirMatchArmBody::Block(b) => {
                            visit_block(hir, *b, tc, bump, template_funcs, enqueue)
                        }
                    }
                }
            }
            HirExprKind::Catch { expr, handler } => {
                visit_expr(hir, *expr, tc, bump, template_funcs, enqueue);
                match handler {
                    HirCatchHandler::Expr(e) => {
                        visit_expr(hir, *e, tc, bump, template_funcs, enqueue)
                    }
                    HirCatchHandler::Block { block, .. } => {
                        visit_block(hir, *block, tc, bump, template_funcs, enqueue)
                    }
                }
            }
            HirExprKind::Lambda { body, .. } => match body {
                HirLambdaBody::Expr(e) => visit_expr(hir, *e, tc, bump, template_funcs, enqueue),
                HirLambdaBody::Block(b) => visit_block(hir, *b, tc, bump, template_funcs, enqueue),
            },
            HirExprKind::AsyncBlock { block }
            | HirExprKind::UnsafeBlock { block }
            | HirExprKind::ValueBlock { block } => {
                visit_block(hir, *block, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::StructLiteral { fields, .. } => {
                for f in hir.pool.field_inits_list(*fields) {
                    visit_expr(hir, f.value, tc, bump, template_funcs, enqueue);
                }
            }
            HirExprKind::ArrayRepeat { value } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::Array { items } => {
                for &e in hir.pool.expr_list(*items) {
                    visit_expr(hir, e, tc, bump, template_funcs, enqueue);
                }
            }
            HirExprKind::StringInterp { parts } => {
                for p in parts {
                    if let arandu_middle::hir::HirStringPart::Expr(e) = p {
                        visit_expr(hir, *e, tc, bump, template_funcs, enqueue);
                    }
                }
            }
            _ => {}
        }
    }

    visit_block(hir, block_id, tc, bump, template_funcs, &mut enqueue);
}

fn is_identity_instantiation(tc: &TypeCheckResult, symbol: SymbolId, type_args: &[TypeId]) -> bool {
    let Some(params) = tc.type_info.generic_params.get(&symbol) else {
        return false;
    };
    if params.len() != type_args.len() {
        return false;
    }
    let interner = &tc.type_info.type_interner;
    params.iter().zip(type_args.iter()).all(|(&param, &tid)| {
        matches!(
            interner.resolve(tid),
            ArType::Named(id, ref args) if id == param && args.is_empty()
        ) || matches!(interner.resolve(tid), ArType::ConstParam(id) if id == param)
    })
}

/// True if `tid` is still a free type-parameter (not a concrete type).
fn type_arg_still_param(tc: &TypeCheckResult, tid: TypeId) -> bool {
    match tc.type_info.type_interner.resolve(tid) {
        ArType::Named(id, ref args) if args.is_empty() => tc
            .type_info
            .generic_params
            .values()
            .any(|params| params.contains(&id)),
        ArType::ConstParam(id) => tc
            .type_info
            .generic_params
            .values()
            .any(|params| params.contains(&id)),
        _ => false,
    }
}

/// Independent units must not publish free parameters or unresolved numeric
/// pseudo-types as concrete keys. Inferred literals are defaulted before this
/// check; the legacy pure producer retains its existing admission policy.
fn instance_args_fully_concrete(tc: &TypeCheckResult, type_args: &[TypeId]) -> bool {
    use arandu_middle::types::TypeShape;
    fn unresolved(tc: &TypeCheckResult, shape: &TypeShape) -> bool {
        match shape {
            TypeShape::Named(id, args) => {
                tc.symbols.try_get(*id).is_none()
                    || tc
                        .type_info
                        .generic_params
                        .values()
                        .any(|params| params.contains(id))
                    || args.iter().any(|arg| unresolved(tc, arg))
            }
            TypeShape::ConstArray(_, _)
            | TypeShape::ConstParam(_)
            | TypeShape::IntLiteral
            | TypeShape::FloatLiteral
            | TypeShape::Error => true,
            TypeShape::Func(args, ret) => {
                args.iter().any(|arg| unresolved(tc, arg)) || unresolved(tc, ret)
            }
            TypeShape::Tuple(args) => args.iter().any(|arg| unresolved(tc, arg)),
            TypeShape::Nullable(inner)
            | TypeShape::Slice(inner)
            | TypeShape::MutSlice(inner)
            | TypeShape::Array(_, inner)
            | TypeShape::Ptr(inner)
            | TypeShape::Ref(inner)
            | TypeShape::RefMut(inner)
            | TypeShape::Option(inner)
            | TypeShape::Coroutine(inner)
            | TypeShape::Poll(inner)
            | TypeShape::Range(inner) => unresolved(tc, inner),
            TypeShape::Result(ok, error) => unresolved(tc, ok) || unresolved(tc, error),
            TypeShape::Primitive(_)
            | TypeShape::Vector(_)
            | TypeShape::FrozenConst(_)
            | TypeShape::Const(_)
            | TypeShape::GenRef
            | TypeShape::Err
            | TypeShape::Void => false,
        }
    }
    type_args.iter().all(|&tid| {
        arandu_middle::types::TypeShape::from_id(tid, &tc.type_info.type_interner)
            .is_ok_and(|shape| !unresolved(tc, &shape))
    })
}

fn type_args_fully_concrete(tc: &TypeCheckResult, type_args: &[TypeId]) -> bool {
    type_args.iter().all(|&tid| !type_arg_still_param(tc, tid))
}

fn specialize_free_func(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    key: &InstantiationKey<'_>,
    template_funcs: &FxHashMap<SymbolId, usize>,
) -> Result<(SymbolId, HirBlockId), Diagnostic> {
    let (symbol, body) = specialize_func(tc, hir, key, template_funcs, true, false)?;
    body.map(|body| (symbol, body)).ok_or_else(|| {
        Diagnostic::ice(
            DiagCode::ICEL001,
            "concrete function specialization lost its body",
            Span::new(0, 0, 0),
        )
    })
}

fn specialize_func(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
    key: &InstantiationKey<'_>,
    template_funcs: &FxHashMap<SymbolId, usize>,
    body_required: bool,
    private_scope: bool,
) -> Result<(SymbolId, Option<HirBlockId>), Diagnostic> {
    let &decl_idx = template_funcs.get(&key.symbol).ok_or_else(|| {
        Diagnostic::error(
            DiagCode::G001GenericInstantiationCycle,
            "monomorphize: template function missing from HIR".to_string(),
            Span::new(0, 0, 0),
        )
    })?;
    let template_decl_id = hir.decls[decl_idx];
    let template = match hir.pool.decl(template_decl_id) {
        HirDecl::Func(f) => f.clone_shallow(),
        _ => {
            return Err(Diagnostic::error(
                DiagCode::G001GenericInstantiationCycle,
                "monomorphize: expected function template".to_string(),
                Span::new(0, 0, 0),
            ));
        }
    };
    if body_required && template.body.is_none() {
        return Err(Diagnostic::error(
            DiagCode::G001GenericInstantiationCycle,
            "monomorphize: template has no body".to_string(),
            template.span,
        ));
    }
    let body_id = template.body;

    let params_list = tc
        .type_info
        .generic_params
        .get(&key.symbol)
        .cloned()
        .ok_or_else(|| {
            Diagnostic::error(
                DiagCode::G001GenericInstantiationCycle,
                "monomorphize: missing generic_params".to_string(),
                template.span,
            )
        })?;

    if params_list.len() != key.type_args.len() {
        return Err(Diagnostic::error(
            DiagCode::G002GenericInstantiationLimit,
            format!(
                "generic argument count mismatch for `{}`: expected {}, found {}",
                tc.symbols.get(key.symbol).name,
                params_list.len(),
                key.type_args.len()
            ),
            template.span,
        ));
    }

    let mangled = super::demangle::mangle_symbol(key, &tc.type_info.type_interner, &tc.symbols);

    let global = tc.symbols.global_scope();
    let allocation_scope = if private_scope {
        tc.symbols_mut().new_scope(global)
    } else {
        global
    };
    // Idempotent: same mangling may appear via dual keys (rare); reuse symbol.
    let new_func_sym =
        match tc
            .symbols_mut()
            .define(allocation_scope, &mangled, SymbolKind::Func, template.span)
        {
            Ok(s) => s,
            Err(existing) => {
                // Same mangling already specialized (dual keys / re-entry). Reuse
                // the existing specialized body for nested discovery — not the template.
                for &decl_id in &hir.decls {
                    if let HirDecl::Func(f) = hir.pool.decl(decl_id)
                        && f.symbol == existing
                        && let Some(b) = f.body
                    {
                        return Ok((existing, Some(b)));
                    }
                }
                return Ok((existing, body_id));
            }
        };

    // Register a stable, qualified host name so runtime, JIT, and backends resolve
    // this generic instance deterministically without relying on sym.name fallback.
    let parent_host_name = tc.symbols.host_func_name(tc.symbols.get(key.symbol));
    let instance_host_name = arandu_middle::SmolStr::new(format!("{parent_host_name}.{mangled}"));
    tc.symbols_mut()
        .host_function_names
        .insert(new_func_sym, instance_host_name);
    if tc.symbols.tooling_entrypoints.contains(&key.symbol) {
        tc.symbols_mut().tooling_entrypoints.insert(new_func_sym);
    }

    // Subst and specialized return type
    let subst = build_subst_ids(&params_list, key.type_args, &tc.type_info.type_interner);
    let ret_ty = substitute_type_id(template.return_type, &subst, &tc.type_info.type_interner);

    // Register specialized function type Func(params, ret) for decl_type lookup
    let mut param_tids = Vec::new();
    let mut symbol_map: FxHashMap<SymbolId, SymbolId> = FxHashMap::default();
    let old_params: Vec<HirParam> = hir.pool.params_list(template.params).to_vec();
    let mut new_params = Vec::with_capacity(old_params.len());
    for (i, p) in old_params.iter().enumerate() {
        let new_ty = substitute_type_id(p.ty, &subst, &tc.type_info.type_interner);
        param_tids.push(new_ty);
        let pname = format!("${i}_{}", tc.symbols.get(p.symbol).name);
        let new_sym = tc
            .symbols_mut()
            .define(
                allocation_scope,
                &format!("{mangled}{pname}"),
                SymbolKind::Param,
                p.span,
            )
            .unwrap_or_else(|existing| existing);
        symbol_map.insert(p.symbol, new_sym);
        tc.type_info_mut().record_decl_type(new_sym, new_ty);
        new_params.push(HirParam {
            symbol: new_sym,
            ty: new_ty,
            span: p.span,
            is_receiver: p.is_receiver,
            receiver_kind: p.receiver_kind,
        });
    }
    let func_ty = ArType::func(&param_tids, ret_ty, &tc.type_info.type_interner);
    let func_ty_id = tc.type_info.type_interner.intern(func_ty);
    tc.type_info_mut()
        .record_decl_type(new_func_sym, func_ty_id);
    if let Some(summary) = tc
        .type_info
        .return_borrow_summaries
        .get(&key.symbol)
        .cloned()
    {
        tc.type_info_mut()
            .return_borrow_summaries
            .insert(new_func_sym, summary);
    }
    let specializes_destructor = tc
        .type_info
        .destructors
        .values()
        .any(|symbol| *symbol == template.symbol);
    if specializes_destructor && let Some(receiver) = new_params.first() {
        tc.type_info_mut()
            .destructor_instances
            .insert(receiver.ty, new_func_sym);
    }

    let new_params_range = hir.pool.alloc_param_list(&new_params);
    let new_body = if body_required {
        body_id
            .map(|body| clone_block(hir, body, &subst, &mut symbol_map, tc, &mangled))
            .transpose()?
    } else {
        None
    };

    let specialized = HirFunc {
        symbol: new_func_sym,
        params: new_params_range,
        return_type: ret_ty,
        body: new_body,
        span: template.span,
        is_async: template.is_async,
        no_fallback: template.no_fallback,
    };
    let decl_id = hir.pool.alloc_decl(HirDecl::Func(specialized));
    hir.decls.push(decl_id);
    Ok((new_func_sym, new_body))
}

/// Clone a template [`HirFunc`] fields we need without cloning the whole pool.
trait CloneShallow {
    fn clone_shallow(&self) -> Self;
}

impl CloneShallow for HirFunc {
    fn clone_shallow(&self) -> Self {
        Self {
            symbol: self.symbol,
            params: self.params,
            return_type: self.return_type,
            body: self.body,
            span: self.span,
            is_async: self.is_async,
            no_fallback: self.no_fallback,
        }
    }
}
