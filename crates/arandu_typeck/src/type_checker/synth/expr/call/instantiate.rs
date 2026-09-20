use crate::type_checker::TypeChecker;
use crate::type_checker::types::{self, ArType};

use arandu_middle::types::type_interner::TypeId;

pub(crate) fn infer_and_instantiate_func(
    checker: &mut TypeChecker<'_>,
    type_params: &[arandu_middle::SymbolId],
    formals: &[TypeId],
    ret: TypeId,
    arg_tys: &[TypeId],
    expected_ret: Option<TypeId>,
    call_span: arandu_lexer::Span,
) -> Option<(Vec<TypeId>, TypeId)> {
    if formals.len() != arg_tys.len() {
        return None;
    }
    let mut bindings: rustc_hash::FxHashMap<arandu_middle::SymbolId, TypeId> =
        rustc_hash::FxHashMap::default();
    for (&formal_id, &arg_id) in formals.iter().zip(arg_tys.iter()) {
        let formal = checker.resolve(formal_id);
        bind_type_params(checker, type_params, &formal, arg_id, &mut bindings);
    }
    // `join<T>(handle)` has no `T` in args — infer from expected return type.
    if let Some(exp) = expected_ret {
        let ret_ty = checker.resolve(ret);
        bind_type_params(checker, type_params, &ret_ty, exp, &mut bindings);
    }
    // Deduce unbound type parameters from constraints of bound parameters.
    deduce_unbound_from_constraints(checker, type_params, &mut bindings);

    let mut concrete = Vec::with_capacity(type_params.len());
    for &p in type_params {
        let tid = bindings.get(&p).copied()?;
        if checker.resolve(tid).is_error() {
            return None;
        }
        concrete.push(checker.resolve(tid));
    }
    types::interfaces::check_instantiation_constraints(checker, type_params, &concrete, call_span);
    let subst = types::build_subst(type_params, &concrete);
    let new_params: Vec<TypeId> = formals
        .iter()
        .map(|&fid| {
            let ty = checker.resolve(fid);
            let inst = types::substitute_type(&ty, &subst, &checker.type_info.type_interner);
            checker.intern(inst)
        })
        .collect();
    let ret_ty = checker.resolve(ret);
    let ret_inst = types::substitute_type(&ret_ty, &subst, &checker.type_info.type_interner);
    Some((new_params, checker.intern(ret_inst)))
}

fn deduce_unbound_from_constraints(
    checker: &TypeChecker<'_>,
    type_params: &[arandu_middle::SymbolId],
    bindings: &mut rustc_hash::FxHashMap<arandu_middle::SymbolId, TypeId>,
) {
    for &p in type_params {
        if bindings.contains_key(&p) {
            continue;
        }
        let bound_pairs: Vec<(arandu_middle::SymbolId, TypeId)> =
            bindings.iter().map(|(&k, &v)| (k, v)).collect();
        for (b_sym, b_tid) in bound_pairs {
            let Some(constraints) = checker.type_info.param_constraints.get(&b_sym).cloned() else {
                continue;
            };
            for bound in constraints.iter() {
                let Some(iface_params) = checker
                    .type_info
                    .generic_params
                    .get(&bound.iface_sym)
                    .cloned()
                else {
                    continue;
                };
                for (&iface_param_sym, &arg_tid) in iface_params.iter().zip(bound.type_args.iter())
                {
                    if let ArType::Named(arg_sym, _) = checker.resolve(arg_tid)
                        && arg_sym == p
                        && let Some(deduced) =
                            deduce_iface_param(checker, b_tid, bound.iface_sym, iface_param_sym)
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
}

fn deduce_iface_param(
    checker: &TypeChecker<'_>,
    concrete_tid: TypeId,
    iface_sym: arandu_middle::SymbolId,
    iface_param_sym: arandu_middle::SymbolId,
) -> Option<TypeId> {
    let concrete = checker.resolve(concrete_tid);
    let type_id = match concrete {
        ArType::Named(id, _) => id,
        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
            match checker.resolve(inner) {
                ArType::Named(id, _) => id,
                _ => return None,
            }
        }
        _ => return None,
    };
    let iface = checker.type_info.interfaces.get(&iface_sym)?;
    for m in &iface.methods {
        let method_name = &m.name;
        let required_id = m.sig_id;
        let required_ty = checker.resolve(required_id);
        let Some(prov_sym) = checker
            .symbols
            .lookup_associated_member(type_id, method_name)
        else {
            continue;
        };
        let Some(mut prov_ty) = checker.decl_type(prov_sym) else {
            continue;
        };
        if let Some(prov_gp) = checker.type_info.generic_params.get(&prov_sym)
            && let ArType::Func(prov_p, _) = &prov_ty
        {
            let prov_args = checker.type_info.type_interner.type_args(*prov_p);
            if let Some(&self_fid) = prov_args.first() {
                let self_formal = checker.resolve(self_fid);
                let mut prov_bindings = rustc_hash::FxHashMap::default();
                bind_type_params(
                    checker,
                    prov_gp,
                    &self_formal,
                    concrete_tid,
                    &mut prov_bindings,
                );
                let prov_concrete: Vec<ArType> = prov_gp
                    .iter()
                    .map(|p| {
                        prov_bindings.get(p).map_or_else(
                            || ArType::named(*p, &[], &checker.type_info.type_interner),
                            |&tid| checker.resolve(tid),
                        )
                    })
                    .collect();
                let subst = types::build_subst(prov_gp, &prov_concrete);
                prov_ty =
                    types::substitute_type(&prov_ty, &subst, &checker.type_info.type_interner);
            }
        }
        let mut iface_bindings = rustc_hash::FxHashMap::default();
        if let (ArType::Func(req_p, req_ret), ArType::Func(prov_p, prov_ret)) =
            (&required_ty, &prov_ty)
        {
            bind_type_params(
                checker,
                &[iface_param_sym],
                &checker.resolve(*req_ret),
                *prov_ret,
                &mut iface_bindings,
            );
            let req_args = checker.type_info.type_interner.type_args(*req_p);
            let prov_args = checker.type_info.type_interner.type_args(*prov_p);
            for (&r, &pr) in req_args.iter().zip(prov_args.iter()) {
                bind_type_params(
                    checker,
                    &[iface_param_sym],
                    &checker.resolve(r),
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

pub(crate) fn bind_type_params(
    checker: &TypeChecker<'_>,
    type_params: &[arandu_middle::SymbolId],
    formal: &ArType,
    actual_id: TypeId,
    bindings: &mut rustc_hash::FxHashMap<arandu_middle::SymbolId, TypeId>,
) {
    let interner = &checker.type_info.type_interner;
    match formal {
        ArType::ConstParam(id) if type_params.contains(id) => {
            bindings.entry(*id).or_insert(actual_id);
        }
        ArType::Named(id, args) if args.is_empty() && type_params.contains(id) => {
            bindings.entry(*id).or_insert(actual_id);
        }
        ArType::Named(_, args) => {
            if let ArType::Named(_, act_args) = interner.resolve(actual_id)
                && args.len == act_args.len
            {
                let formals = interner.type_args(*args);
                let actuals = interner.type_args(act_args);
                for (&fa, &aa) in formals.iter().zip(actuals.iter()) {
                    bind_type_params(checker, type_params, &interner.resolve(fa), aa, bindings);
                }
            }
        }
        // `&T` / `&mut T` formals: peel matching actual refs, or treat a bare
        // actual as the referent (auto-ref at the call site — same as rustc
        // type param inference for `fn f(v: &Vec<T>)` called with `Vec<int>`).
        ArType::Ref(inner) | ArType::RefMut(inner) => {
            let actual = interner.resolve(actual_id);
            let act_inner = match actual {
                ArType::Ref(i) | ArType::RefMut(i) | ArType::Ptr(i) => i,
                // Bare value will be auto-ref'd; bind against the value type.
                _ => actual_id,
            };
            bind_type_params(
                checker,
                type_params,
                &interner.resolve(*inner),
                act_inner,
                bindings,
            );
        }
        ArType::Ptr(inner)
        | ArType::Nullable(inner)
        | ArType::Slice(inner)
        | ArType::Option(inner)
        | ArType::Array(_, inner)
        | ArType::Coroutine(inner)
        | ArType::Poll(inner) => {
            let act_inner = match interner.resolve(actual_id) {
                ArType::Ptr(i)
                | ArType::Nullable(i)
                | ArType::Slice(i)
                | ArType::Option(i)
                | ArType::Array(_, i)
                | ArType::Coroutine(i)
                | ArType::Poll(i) => Some(i),
                _ => None,
            };
            if let Some(ai) = act_inner {
                bind_type_params(
                    checker,
                    type_params,
                    &interner.resolve(*inner),
                    ai,
                    bindings,
                );
            }
        }
        ArType::ConstArray(param, inner) => {
            if let ArType::Array(length, actual_inner) = interner.resolve(actual_id) {
                if type_params.contains(param) {
                    let length_id = interner.intern(ArType::Const(length));
                    bindings.entry(*param).or_insert(length_id);
                }
                bind_type_params(
                    checker,
                    type_params,
                    &interner.resolve(*inner),
                    actual_inner,
                    bindings,
                );
            }
        }
        ArType::Result(ok, err) => {
            if let ArType::Result(aok, aerr) = interner.resolve(actual_id) {
                bind_type_params(checker, type_params, &interner.resolve(*ok), aok, bindings);
                bind_type_params(
                    checker,
                    type_params,
                    &interner.resolve(*err),
                    aerr,
                    bindings,
                );
            }
        }
        _ => {}
    }
}
