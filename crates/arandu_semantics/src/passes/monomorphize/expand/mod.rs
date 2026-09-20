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
            HirStmtKind::For { body, .. } => {
                visit_block(hir, *body, tc, bump, template_funcs, enqueue);
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
            HirStmtKind::Defer(b) | HirStmtKind::ErrDefer(b) | HirStmtKind::Unsafe(b) => {
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
            HirExprKind::AsyncBlock { block } | HirExprKind::UnsafeBlock { block } => {
                visit_block(hir, *block, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::StructLiteral { fields, .. } => {
                for f in hir.pool.field_inits_list(*fields) {
                    visit_expr(hir, f.value, tc, bump, template_funcs, enqueue);
                }
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
            .any(|ps| ps.contains(&id)),
        ArType::ConstParam(id) => tc
            .type_info
            .generic_params
            .values()
            .any(|ps| ps.contains(&id)),
        _ => false,
    }
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
    let body_id = template.body.ok_or_else(|| {
        Diagnostic::error(
            DiagCode::G001GenericInstantiationCycle,
            "monomorphize: template has no body".to_string(),
            template.span,
        )
    })?;

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
    // Idempotent: same mangling may appear via dual keys (rare); reuse symbol.
    let new_func_sym =
        match tc
            .symbols_mut()
            .define(global, &mangled, SymbolKind::Func, template.span)
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
                        return Ok((existing, b));
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
        let global = tc.symbols.global_scope();
        let new_sym = tc
            .symbols_mut()
            .define(
                global,
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
    let new_body = clone_block(hir, body_id, &subst, &mut symbol_map, tc, &mangled)?;

    let specialized = HirFunc {
        symbol: new_func_sym,
        params: new_params_range,
        return_type: ret_ty,
        body: Some(new_body),
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
