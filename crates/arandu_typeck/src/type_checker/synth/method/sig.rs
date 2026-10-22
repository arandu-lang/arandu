use super::super::super::TypeChecker;
use super::super::super::types::{ArType, TypeId};
use super::bind_receiver_type_arg;

/// Substitute struct type parameters in a method signature using the concrete
/// receiver type (`BoxG<int>` → replace `T` with `int` in params/return).
pub(super) fn instantiate_method_sig_for_receiver(
    checker: &mut TypeChecker<'_>,
    struct_id: arandu_middle::SymbolId,
    actual_base_ty_id: TypeId,
    params: Vec<TypeId>,
    ret: TypeId,
    method_sym: Option<arandu_middle::SymbolId>,
    call_span: arandu_lexer::Span,
) -> (Vec<TypeId>, TypeId) {
    use crate::type_checker::types::{build_subst, substitute_type};

    // Peel ptr/ref so specialization works when the receiver is already a ref
    // (or when only the formal is `&T` after auto-ref at the call site).
    let mut base_id = actual_base_ty_id;
    for _ in 0..4 {
        match checker.resolve(base_id) {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                base_id = inner;
            }
            _ => break,
        }
    }
    let (recv_args_ids, recv_args) = match checker.resolve(base_id) {
        ArType::Named(id, args) if id == struct_id => {
            let arg_vec = checker.type_info.type_interner.type_args(args);
            (
                arg_vec.clone(),
                arg_vec
                    .iter()
                    .map(|&a| checker.resolve(a))
                    .collect::<Vec<_>>(),
            )
        }
        _ => return (params, ret),
    };
    if recv_args.is_empty() {
        return (params, ret);
    }

    // Match the formal receiver parameter's type arguments against `recv_args`
    // so both unspecialized (`StaticMatrix<T, M, N>`) and specialized
    // (`StaticMatrix<float, M, N>`, `StaticMatrix<float, N, N>`, `Vec<Option<T>>`)
    // receivers bind their generic parameters from the corresponding positions.
    let mut param_syms: Vec<arandu_middle::SymbolId> = Vec::new();
    let mut bound_args: Vec<ArType> = Vec::new();
    let mut matched_formal = false;
    if let Some(&first_param) = params.first() {
        let mut formal_base = first_param;
        for _ in 0..4 {
            match checker.resolve(formal_base) {
                ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                    formal_base = inner;
                }
                _ => break,
            }
        }
        if let ArType::Named(fid, fargs) = checker.resolve(formal_base)
            && fid == struct_id
            && fargs.len as usize == recv_args_ids.len()
        {
            matched_formal = true;
            let mut candidate_gp: Vec<arandu_middle::SymbolId> = Vec::new();
            if let Some(sym) = method_sym
                && let Some(gp) = checker.type_info.generic_params.get(&sym)
            {
                candidate_gp.extend(gp.iter().copied());
            }
            if let Some(gp) = checker.type_info.generic_params.get(&struct_id) {
                for &p in gp.iter() {
                    if !candidate_gp.contains(&p) {
                        candidate_gp.push(p);
                    }
                }
            }
            let formal_arg_ids = checker.type_info.type_interner.type_args(fargs);
            for (&f_id, &actual_id) in formal_arg_ids.iter().zip(recv_args_ids.iter()) {
                bind_receiver_type_arg(
                    checker,
                    f_id,
                    actual_id,
                    &candidate_gp,
                    &mut param_syms,
                    &mut bound_args,
                );
            }
        }
    }
    if !matched_formal {
        param_syms = if let Some(sym) = method_sym
            && let Some(gp) = checker.type_info.generic_params.get(&sym)
        {
            let n = recv_args.len().min(gp.len());
            gp.iter().copied().take(n).collect()
        } else if let Some(gp) = checker.type_info.generic_params.get(&struct_id) {
            gp.iter().copied().take(recv_args.len()).collect()
        } else {
            return (params, ret);
        };
        if param_syms.len() != recv_args.len() {
            return (params, ret);
        }
        bound_args = recv_args;
    }
    if param_syms.is_empty() {
        return (params, ret);
    }

    let has_method_specific_bounds = checker
        .type_info
        .generic_params
        .get(&struct_id)
        .is_some_and(|struct_gp| param_syms.iter().any(|p| !struct_gp.contains(p)));
    if has_method_specific_bounds {
        crate::type_checker::types::interfaces::check_instantiation_constraints(
            checker,
            &param_syms,
            &bound_args,
            call_span,
        );
    }

    let key_sym = method_sym.unwrap_or(struct_id);
    let cache_key = (key_sym, recv_args_ids);
    if let Some(cached) = checker.type_info.variant_instantiations.get(&cache_key) {
        return cached.clone();
    }

    let subst = build_subst(&param_syms, &bound_args);
    let new_params: Vec<TypeId> = params
        .iter()
        .map(|&pid| {
            let ty = checker.resolve(pid);
            let inst = substitute_type(&ty, &subst, &checker.type_info.type_interner);
            checker.intern(inst)
        })
        .collect();
    let ret_ty = checker.resolve(ret);
    let ret_inst = substitute_type(&ret_ty, &subst, &checker.type_info.type_interner);
    let new_ret = checker.intern(ret_inst);
    let res = (new_params, new_ret);
    checker
        .type_info
        .variant_instantiations
        .insert(cache_key, res.clone());
    res
}
