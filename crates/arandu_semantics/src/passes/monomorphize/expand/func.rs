use super::super::graph::InstantiationKey;
use super::clone::clone_block;
use arandu_diagnostics::{DiagCode, Diagnostic};
use arandu_lexer::Span;
use arandu_middle::hir::{HirBlockId, HirDecl, HirFunc, HirParam, HirProgram};
use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_middle::types::{ArType, build_subst_ids, substitute_type_id};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

pub(super) fn specialize_free_func(
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

pub(super) fn specialize_func(
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

    let mangled =
        super::super::demangle::mangle_symbol(key, &tc.type_info.type_interner, &tc.symbols);

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
