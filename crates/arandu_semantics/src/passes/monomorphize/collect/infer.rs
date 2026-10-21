use super::CallSiteInfo;
use arandu_middle::hir::HirExprKind;
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::{ArType, TypeId};
use arandu_typeck::TypeCheckResult;

pub(super) fn is_identity_args(
    tc: &TypeCheckResult,
    symbol: SymbolId,
    type_args: &[arandu_middle::types::TypeId],
) -> bool {
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

/// Infer function/method type arguments by matching formal param types against
/// arg expr types, plus the specialized callee type typeck recorded on the
/// callee expr (covers `join<T>(h)` where `T` only appears in the return type).
pub(super) fn infer_call_type_args(
    tc: &TypeCheckResult,
    symbol: SymbolId,
    params: &[SymbolId],
    pool: &arandu_middle::hir::HirPool,
    call: &CallSiteInfo<'_>,
) -> Option<Vec<arandu_middle::types::TypeId>> {
    let func_ty = tc.type_info.decl_type(symbol)?;
    let ArType::Func(formals, ret) = func_ty else {
        return None;
    };
    let arg_ids = pool.expr_list(call.args);
    let interner = &tc.type_info.type_interner;
    let formals_vec = interner.type_args(formals);
    let receiver_expr = match &pool.expr(call.callee_id).kind {
        HirExprKind::Field { base, .. } | HirExprKind::SafeField { base, .. }
            if formals_vec.len() == arg_ids.len() + 1 =>
        {
            Some(*base)
        }
        _ => None,
    };
    if receiver_expr.is_none() && formals_vec.len() != arg_ids.len() {
        return None;
    }

    // param_sym → concrete TypeId
    let mut bindings: rustc_hash::FxHashMap<SymbolId, arandu_middle::types::TypeId> =
        rustc_hash::FxHashMap::default();

    if let Some(base_eid) = receiver_expr {
        let formal = interner.resolve(formals_vec[0]);
        let arg_ty_id = pool.expr(base_eid).ty;
        collect_param_bindings(interner, params, &formal, arg_ty_id, &mut bindings);
        for (&formal_id, &arg_eid) in formals_vec[1..].iter().zip(arg_ids.iter()) {
            let formal = interner.resolve(formal_id);
            let arg_ty_id = pool.expr(arg_eid).ty;
            collect_param_bindings(interner, params, &formal, arg_ty_id, &mut bindings);
        }
    } else {
        for (&formal_id, &arg_eid) in formals_vec.iter().zip(arg_ids.iter()) {
            let formal = interner.resolve(formal_id);
            let arg_ty_id = pool.expr(arg_eid).ty;
            collect_param_bindings(interner, params, &formal, arg_ty_id, &mut bindings);
        }
    }

    // Specialized Func type on the callee (typeck inference → HIR .ty).
    let cal_ty = interner.resolve(pool.expr(call.callee_id).ty);
    if let ArType::Func(spec_formals, spec_ret) = cal_ty {
        let spec_formals_vec = interner.type_args(spec_formals);
        for (&orig, &spec) in formals_vec.iter().zip(spec_formals_vec.iter()) {
            let formal = interner.resolve(orig);
            collect_param_bindings(interner, params, &formal, spec, &mut bindings);
        }
        let ret_formal = interner.resolve(ret);
        collect_param_bindings(interner, params, &ret_formal, spec_ret, &mut bindings);
    }

    // Call expression result type (e.g. `return join(h)` expects int).
    {
        let ret_formal = interner.resolve(ret);
        collect_param_bindings(interner, params, &ret_formal, call.result_ty, &mut bindings);
    }

    for (&param_sym, &concrete_tid) in params.iter().zip(call.recv_args.iter()) {
        bindings.entry(param_sym).or_insert(concrete_tid);
    }

    // Deduce unbound type parameters from constraints of bound parameters.
    for &p in params {
        if bindings.contains_key(&p) {
            continue;
        }
        let bound_pairs: Vec<(SymbolId, arandu_middle::types::TypeId)> =
            bindings.iter().map(|(&k, &v)| (k, v)).collect();
        for (b_sym, b_tid) in bound_pairs {
            let Some(constraints) = tc.type_info.param_constraints.get(&b_sym) else {
                continue;
            };
            for bound in constraints.iter() {
                let Some(iface_params) = tc.type_info.generic_params.get(&bound.iface_sym) else {
                    continue;
                };
                for (&iface_param_sym, &arg_tid) in iface_params.iter().zip(bound.type_args.iter())
                {
                    if let ArType::Named(arg_sym, _) = interner.resolve(arg_tid)
                        && arg_sym == p
                        && let Some(deduced) =
                            deduce_mono_iface_param(tc, b_tid, bound.iface_sym, iface_param_sym)
                    {
                        bindings.insert(p, deduced);
                        break;
                    }
                }
                if bindings.contains_key(&p) {
                    break;
                }
            }
            if bindings.contains_key(&p) {
                break;
            }
        }
    }

    let mut out = Vec::with_capacity(params.len());
    for &p in params {
        let tid = *bindings.get(&p)?;
        if matches!(interner.resolve(tid), ArType::Error) {
            return None;
        }
        out.push(tid);
    }
    Some(out)
}

fn deduce_mono_iface_param(
    tc: &TypeCheckResult,
    concrete_tid: arandu_middle::types::TypeId,
    iface_sym: SymbolId,
    iface_param_sym: SymbolId,
) -> Option<arandu_middle::types::TypeId> {
    let interner = &tc.type_info.type_interner;
    let concrete = interner.resolve(concrete_tid);
    let type_id = match concrete {
        ArType::Named(id, _) => id,
        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
            match interner.resolve(inner) {
                ArType::Named(id, _) => id,
                _ => return None,
            }
        }
        _ => return None,
    };
    let iface = tc.type_info.interfaces.get(&iface_sym)?;
    for m in &iface.methods {
        let method_name = &m.name;
        let required_id = m.sig_id;
        let required_ty = interner.resolve(required_id);
        let Some(prov_sym) = tc.symbols.lookup_associated_member(type_id, method_name) else {
            continue;
        };
        let Some(mut prov_ty) = tc.type_info.decl_type(prov_sym) else {
            continue;
        };
        if let Some(prov_gp) = tc.type_info.generic_params.get(&prov_sym)
            && let ArType::Func(prov_p, _) = &prov_ty
        {
            let prov_args = tc.type_info.type_interner.type_args(*prov_p);
            if let Some(&self_fid) = prov_args.first() {
                let self_formal = interner.resolve(self_fid);
                let mut prov_bindings = rustc_hash::FxHashMap::default();
                collect_param_bindings(
                    interner,
                    prov_gp,
                    &self_formal,
                    concrete_tid,
                    &mut prov_bindings,
                );
                let prov_concrete: Vec<TypeId> = prov_gp
                    .iter()
                    .map(|p| {
                        prov_bindings
                            .get(p)
                            .copied()
                            .unwrap_or_else(|| interner.intern(ArType::named(*p, &[], interner)))
                    })
                    .collect();
                let subst =
                    arandu_middle::types::build_subst_ids(prov_gp, &prov_concrete, interner);
                if let Some(decl_tid) = tc.type_info.decl_type_id(prov_sym) {
                    let inst = arandu_middle::types::substitute_type_id(decl_tid, &subst, interner);
                    prov_ty = interner.resolve(inst);
                }
            }
        }
        let mut iface_bindings = rustc_hash::FxHashMap::default();
        if let (ArType::Func(req_p, req_ret), ArType::Func(prov_p, prov_ret)) =
            (&required_ty, &prov_ty)
        {
            collect_param_bindings(
                interner,
                &[iface_param_sym],
                &interner.resolve(*req_ret),
                *prov_ret,
                &mut iface_bindings,
            );
            let req_args = tc.type_info.type_interner.type_args(*req_p);
            let prov_args = tc.type_info.type_interner.type_args(*prov_p);
            for (&r, &pr) in req_args.iter().zip(prov_args.iter()) {
                collect_param_bindings(
                    interner,
                    &[iface_param_sym],
                    &interner.resolve(r),
                    pr,
                    &mut iface_bindings,
                );
            }
        }
        if let Some(&deduced) = iface_bindings.get(&iface_param_sym) {
            return Some(deduced);
        }
    }
    None
}

fn collect_param_bindings(
    interner: &arandu_middle::types::TypeInterner,
    type_params: &[SymbolId],
    formal: &ArType,
    actual_id: arandu_middle::types::TypeId,
    bindings: &mut rustc_hash::FxHashMap<SymbolId, arandu_middle::types::TypeId>,
) {
    match formal {
        ArType::ConstParam(id) if type_params.contains(id) => {
            bindings.entry(*id).or_insert(actual_id);
        }
        ArType::Named(id, args) if args.is_empty() && type_params.contains(id) => {
            bindings.entry(*id).or_insert(actual_id);
        }
        ArType::Named(_, args) => {
            let mut peeled_id = actual_id;
            for _ in 0..4 {
                match interner.resolve(peeled_id) {
                    ArType::Ref(i) | ArType::RefMut(i) | ArType::Ptr(i) | ArType::Nullable(i) => {
                        peeled_id = i;
                    }
                    _ => break,
                }
            }
            let actual = interner.resolve(peeled_id);
            let actual_args: Option<Vec<arandu_middle::types::TypeId>> = match actual {
                ArType::Named(_, a) if args.len == a.len => Some(interner.type_args(a)),
                ArType::Option(inner) if args.len == 1 => Some(vec![inner]),
                ArType::Result(ok, err) if args.len == 2 => Some(vec![ok, err]),
                _ => None,
            };
            if let Some(actual_args) = actual_args {
                let formal_args = interner.type_args(*args);
                for (&fa, &aa) in formal_args.iter().zip(actual_args.iter()) {
                    let fty = interner.resolve(fa);
                    collect_param_bindings(interner, type_params, &fty, aa, bindings);
                }
            }
        }
        // Auto-ref: bare `Vec<int>` matches formal `&Vec<T>` / `&mut Vec<T>`.
        ArType::Ref(inner) | ArType::RefMut(inner) => {
            let actual = interner.resolve(actual_id);
            let act_inner = match actual {
                ArType::Ref(i) | ArType::RefMut(i) | ArType::Ptr(i) => i,
                _ => actual_id,
            };
            let fty = interner.resolve(*inner);
            collect_param_bindings(interner, type_params, &fty, act_inner, bindings);
        }
        ArType::Ptr(inner)
        | ArType::Nullable(inner)
        | ArType::Slice(inner)
        | ArType::Option(inner)
        | ArType::Array(_, inner)
        | ArType::Coroutine(inner)
        | ArType::Poll(inner)
        | ArType::Range(inner) => {
            let actual = interner.resolve(actual_id);
            let act_inner = match actual {
                ArType::Ptr(i)
                | ArType::Nullable(i)
                | ArType::Slice(i)
                | ArType::Option(i)
                | ArType::Array(_, i)
                | ArType::Coroutine(i)
                | ArType::Poll(i)
                | ArType::Range(i) => Some(i),
                _ => None,
            };
            if let Some(ai) = act_inner {
                let fty = interner.resolve(*inner);
                collect_param_bindings(interner, type_params, &fty, ai, bindings);
            }
        }
        ArType::ConstArray(param, inner) => {
            if let ArType::Array(length, actual_inner) = interner.resolve(actual_id) {
                if type_params.contains(param) {
                    let length_id = interner.intern(ArType::Const(length));
                    bindings.entry(*param).or_insert(length_id);
                }
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*inner),
                    actual_inner,
                    bindings,
                );
            }
        }
        ArType::Result(ok, err) => {
            if let ArType::Result(aok, aerr) = interner.resolve(actual_id) {
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*ok),
                    aok,
                    bindings,
                );
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*err),
                    aerr,
                    bindings,
                );
            }
        }
        ArType::Func(fps, fret) => {
            if let ArType::Func(aps, aret) = interner.resolve(actual_id)
                && fps.len == aps.len
            {
                let formal_fps = interner.type_args(*fps);
                let actual_aps = interner.type_args(aps);
                for (&fp, &ap) in formal_fps.iter().zip(actual_aps.iter()) {
                    collect_param_bindings(
                        interner,
                        type_params,
                        &interner.resolve(fp),
                        ap,
                        bindings,
                    );
                }
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*fret),
                    aret,
                    bindings,
                );
            }
        }
        ArType::Tuple(items) => {
            if let ArType::Tuple(acts) = interner.resolve(actual_id)
                && items.len == acts.len
            {
                let formal_items = interner.type_args(*items);
                let actual_items = interner.type_args(acts);
                for (&fi, &ai) in formal_items.iter().zip(actual_items.iter()) {
                    collect_param_bindings(
                        interner,
                        type_params,
                        &interner.resolve(fi),
                        ai,
                        bindings,
                    );
                }
            }
        }
        _ => {}
    }
}
