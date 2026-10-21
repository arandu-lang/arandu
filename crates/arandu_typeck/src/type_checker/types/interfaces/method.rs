use super::super::unify;
use super::super::{ArType, TypeId};
use super::super::{GenericSubst, build_subst, substitute_type};
use crate::passes::type_checker::TypeChecker;
use crate::{SymbolId, SymbolKind};

pub(crate) fn peel_base_type(checker: &TypeChecker<'_>, mut ty: ArType) -> ArType {
    for _ in 0..4 {
        match ty {
            ArType::Nullable(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Ptr(inner) => {
                ty = checker.resolve(inner);
            }
            other => return other,
        }
    }
    ty
}

pub(crate) fn instantiate_interface_method(
    checker: &TypeChecker<'_>,
    iface_sym: SymbolId,
    bound_type_args: &[TypeId],
    receiver_ty: &ArType,
    method_ty: &ArType,
) -> ArType {
    let base_recv = peel_base_type(checker, receiver_ty.clone());
    let mut subst = GenericSubst::default();
    if let Some(iface_params) = checker.type_info.generic_params.get(&iface_sym) {
        if !bound_type_args.is_empty() {
            for (&param_sym, &arg_tid) in iface_params.iter().zip(bound_type_args.iter()) {
                subst.push((param_sym, checker.resolve(arg_tid)));
            }
        } else {
            let concrete_subst = interface_subst_for_concrete(checker, iface_sym, &base_recv);
            subst.extend(concrete_subst);
        }
    }
    if let Some(iface_info) = checker.type_info.interfaces.get(&iface_sym)
        && let Some(self_sym) = iface_info.self_param
    {
        subst.push((self_sym, base_recv));
    }
    substitute_type(method_ty, &subst, &checker.type_info.type_interner)
}

fn interface_subst_for_concrete(
    checker: &TypeChecker,
    iface_sym: SymbolId,
    concrete: &ArType,
) -> GenericSubst {
    let Some(iface_params) = checker.type_info.generic_params.get(&iface_sym) else {
        return GenericSubst::default();
    };
    if iface_params.is_empty() {
        return GenericSubst::default();
    }
    if let ArType::Named(_, args) = concrete
        && args.len as usize == iface_params.len()
    {
        let arg_ids = checker.type_info.type_interner.type_args(*args);
        let resolved_args: Vec<ArType> = arg_ids
            .iter()
            .map(|&a| checker.type_info.type_interner.resolve(a))
            .collect();
        return build_subst(iface_params, &resolved_args);
    }
    GenericSubst::default()
}

pub(super) fn lookup_method_type(
    checker: &TypeChecker,
    type_id: SymbolId,
    method: &str,
) -> Option<ArType> {
    let sym = checker.symbols.lookup_associated_member(type_id, method)?;
    checker.decl_type(sym)
}

/// Drop leading `Self` / `&Self` / `&mut Self` (or concrete receiver type) from an interface method formal.
pub(super) fn strip_interface_receiver(
    ty: ArType,
    checker: &TypeChecker<'_>,
    concrete_base: Option<&ArType>,
    self_param: Option<SymbolId>,
) -> ArType {
    let ArType::Func(params, ret) = ty else {
        return ty;
    };
    let params_ids = checker.type_info.type_interner.type_args(params);
    if params_ids.is_empty() {
        return ArType::func(&params_ids, ret, &checker.type_info.type_interner);
    }
    let first_is_recv = is_self_type(checker, params_ids[0], self_param)
        || concrete_base.is_some_and(|base| {
            let p_base = peel_base_type(checker, checker.resolve(params_ids[0]));
            unify(&p_base, base, &checker.type_info.type_interner)
        });
    if first_is_recv {
        ArType::func(&params_ids[1..], ret, &checker.type_info.type_interner)
    } else {
        ArType::func(&params_ids, ret, &checker.type_info.type_interner)
    }
}

/// Drop the concrete method receiver (`T` / `&T` / `&mut T`) — always first formal.
pub(super) fn strip_impl_receiver(ty: ArType, checker: &TypeChecker<'_>) -> ArType {
    let ArType::Func(params, ret) = ty else {
        return ty;
    };
    let params_ids = checker.type_info.type_interner.type_args(params);
    if params_ids.is_empty() {
        return ArType::func(&params_ids, ret, &checker.type_info.type_interner);
    }
    let _ = checker;
    ArType::func(&params_ids[1..], ret, &checker.type_info.type_interner)
}

fn is_self_type(checker: &TypeChecker<'_>, tid: TypeId, self_param: Option<SymbolId>) -> bool {
    let Some(self_sym) = self_param else {
        return false;
    };
    match checker.resolve(tid) {
        ArType::Named(id, _) => id == self_sym,
        ArType::Ref(inner) | ArType::RefMut(inner) => is_self_type(checker, inner, self_param),
        _ => false,
    }
}

pub(super) fn method_types_compatible(
    required: &ArType,
    provided: &ArType,
    checker: &TypeChecker<'_>,
) -> bool {
    let interner = &checker.type_info.type_interner;
    match (required, provided) {
        (ArType::Func(req_params, req_ret), ArType::Func(prov_params, prov_ret)) => {
            let req_ids = interner.type_args(*req_params);
            let prov_ids = interner.type_args(*prov_params);
            if req_ids.len() != prov_ids.len() {
                return false;
            }
            req_ids.iter().zip(prov_ids.iter()).all(|(&a, &b)| {
                if a == b {
                    return true;
                }
                let ty_a = interner.resolve(a);
                let ty_b = interner.resolve(b);
                type_compat_or_unify(&ty_a, &ty_b, checker)
            }) && (*req_ret == *prov_ret || {
                let ty_a = interner.resolve(*req_ret);
                let ty_b = interner.resolve(*prov_ret);
                type_compat_or_unify(&ty_a, &ty_b, checker)
            })
        }
        _ => type_compat_or_unify(required, provided, checker),
    }
}

fn type_compat_or_unify(a: &ArType, b: &ArType, checker: &TypeChecker<'_>) -> bool {
    let interner = &checker.type_info.type_interner;
    if unify(a, b, interner) {
        return true;
    }
    match (a, b) {
        (ArType::Named(id_a, _), ArType::Named(id_b, _)) => {
            let is_param_a = checker
                .symbols
                .try_get(*id_a)
                .map(|s| s.kind == SymbolKind::TypeParam)
                .unwrap_or(true);
            let is_param_b = checker
                .symbols
                .try_get(*id_b)
                .map(|s| s.kind == SymbolKind::TypeParam)
                .unwrap_or(true);
            is_param_a && is_param_b
        }
        (ArType::Ref(x), ArType::Ref(y)) => {
            type_compat_or_unify(&checker.resolve(*x), &checker.resolve(*y), checker)
        }
        (ArType::RefMut(x), ArType::RefMut(y)) => {
            type_compat_or_unify(&checker.resolve(*x), &checker.resolve(*y), checker)
        }
        (ArType::Ptr(x), ArType::Ptr(y)) => {
            type_compat_or_unify(&checker.resolve(*x), &checker.resolve(*y), checker)
        }
        _ => false,
    }
}
