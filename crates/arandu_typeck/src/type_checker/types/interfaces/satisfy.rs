use super::super::{ArType, TypeId};
use super::super::{build_subst, substitute_type};
use super::method::{
    instantiate_interface_method, lookup_method_type, method_types_compatible, strip_impl_receiver,
    strip_interface_receiver,
};
use crate::passes::type_checker::TypeChecker;
use crate::{SymbolId, SymbolKind};
use arandu_lexer::Span;

pub(super) fn missing_methods_note(
    checker: &mut TypeChecker,
    concrete: &ArType,
    iface_sym: SymbolId,
    bound_type_args: &[TypeId],
) -> String {
    if let ArType::Named(id, _) = concrete
        && checker
            .symbols
            .try_get(*id)
            .is_some_and(|symbol| symbol.kind == SymbolKind::TypeParam)
    {
        return "type parameter does not declare the required instantiated interface bound"
            .to_string();
    }
    if checker.symbols.try_get(iface_sym).is_some_and(|symbol| {
        matches!(
            symbol.lang_item,
            Some(
                arandu_middle::symbol_table::LangItem::Send
                    | arandu_middle::symbol_table::LangItem::Sync
                    | arandu_middle::symbol_table::LangItem::Copy
            )
        )
    }) {
        return "thread transfer/sharing is not proven for this storage: views, pointers, coroutine state and resources require additional contracts; structural analysis is bounded".to_string();
    }
    let missing = missing_interface_methods(checker, concrete, iface_sym, bound_type_args);
    if missing.is_empty() {
        "required method signatures are incompatible".to_string()
    } else {
        format!("missing or incompatible methods: {}", missing.join(", "))
    }
}

pub(crate) fn type_satisfies_interface(
    checker: &mut TypeChecker,
    concrete: &ArType,
    iface_sym: SymbolId,
    bound_type_args: &[TypeId],
    _span: Span,
) -> bool {
    // Free type parameters are not concrete types. A param `A: Allocator` is an
    // obligation on instantiations, not something we can structural-check here.
    // Treating `Named(A, [])` as a concrete type caused T025 on every method that
    // restates `A` (Vec, GenArena) even when constraints were well-formed.
    if let ArType::Named(id, args) = concrete
        && args.is_empty()
        && checker
            .symbols
            .try_get(*id)
            .is_some_and(|s| s.kind == SymbolKind::TypeParam)
    {
        // Interface identity alone is insufficient: Job<bool> does not prove
        // Job<int>. Arguments are canonical IDs in the same type interner.
        if let Some(cs) = checker.type_info.param_constraints.get(id) {
            return cs
                .iter()
                .any(|b| b.iface_sym == iface_sym && b.type_args.as_slice() == bound_type_args);
        }
        return false;
    }

    let Some(iface) = checker.type_info.interfaces.get(&iface_sym) else {
        return false;
    };
    if iface.sealed {
        let Some(type_id) = concrete_type_id(concrete) else {
            return false;
        };
        // Sealed interfaces require an explicit `impl Type: Interface` edge.
        // Structural conformance would otherwise let foreign types bypass the
        // package boundary enforced by name resolution.
        if !checker
            .symbols
            .interface_implementations
            .contains(&(type_id, iface_sym))
        {
            return false;
        }
    }
    if let Some(
        capability @ (arandu_middle::symbol_table::LangItem::Send
        | arandu_middle::symbol_table::LangItem::Sync
        | arandu_middle::symbol_table::LangItem::Copy),
    ) = checker
        .symbols
        .try_get(iface_sym)
        .and_then(|symbol| symbol.lang_item)
    {
        return bound_type_args.is_empty()
            && super::super::transfer::satisfies(checker, concrete, capability);
    }
    let Some(type_id) = concrete_type_id(concrete) else {
        return false;
    };

    let method_specs: Vec<_> = iface.methods.clone();
    for m in method_specs {
        let method = m.name;
        let required_id = m.sig_id;
        let required = checker.resolve(required_id);
        let required_inst =
            instantiate_interface_method(checker, iface_sym, bound_type_args, concrete, &required);
        let Some(provided) = lookup_method_type(checker, type_id, &method) else {
            return false;
        };
        // Interface may list `self: Self`; impl methods always have a receiver.
        // Compare payloads only (TYP.2).
        let self_param = iface.self_param;
        let required_stripped =
            strip_interface_receiver(required_inst, checker, Some(concrete), self_param);
        let provided_inst = instantiate_impl_method(checker, type_id, concrete, &method, provided);
        let provided_stripped = strip_impl_receiver(provided_inst, checker);
        if !method_types_compatible(&required_stripped, &provided_stripped, checker) {
            return false;
        }
    }
    true
}

#[cold]
pub(super) fn missing_interface_methods(
    checker: &mut TypeChecker,
    concrete: &ArType,
    iface_sym: SymbolId,
    bound_type_args: &[TypeId],
) -> Vec<String> {
    let Some(iface) = checker.type_info.interfaces.get(&iface_sym) else {
        return vec!["<interface not collected>".to_string()];
    };
    let Some(type_id) = concrete_type_id(concrete) else {
        return vec!["<non-nominal type>".to_string()];
    };

    let self_param = iface.self_param;
    let mut missing = Vec::new();
    let method_specs: Vec<_> = iface.methods.clone();
    for m in method_specs {
        let method = m.name;
        let required_id = m.sig_id;
        let required = checker.resolve(required_id);
        let required_inst =
            instantiate_interface_method(checker, iface_sym, bound_type_args, concrete, &required);
        let Some(provided) = lookup_method_type(checker, type_id, &method) else {
            let mut similar = Vec::new();
            let max_distance = if method.len() <= 4 { 2 } else { 3 };
            for (type_sym, prov_name) in checker.symbols.associated_members.keys() {
                if *type_sym != type_id {
                    continue;
                }
                let dist = if prov_name.to_lowercase() == method.to_lowercase() {
                    0
                } else {
                    strsim::levenshtein(method.as_str(), prov_name)
                };
                if dist <= max_distance {
                    similar.push(prov_name.to_string());
                }
            }
            if !similar.is_empty() {
                missing.push(format!(
                    "{method} (did you mean `{}`?)",
                    similar.join("`, `")
                ));
            } else {
                missing.push(method.to_string());
            }
            continue;
        };
        let required_stripped =
            strip_interface_receiver(required_inst, checker, Some(concrete), self_param);
        let provided_inst = instantiate_impl_method(checker, type_id, concrete, &method, provided);
        let provided_stripped = strip_impl_receiver(provided_inst, checker);
        if !method_types_compatible(&required_stripped, &provided_stripped, checker) {
            missing.push(format!("{method} (signature mismatch)"));
        }
    }
    missing
}

fn instantiate_impl_method(
    checker: &TypeChecker<'_>,
    type_id: SymbolId,
    concrete: &ArType,
    method_name: &str,
    method_ty: ArType,
) -> ArType {
    let ArType::Named(cid, args) = concrete else {
        return method_ty;
    };
    if *cid != type_id || args.len == 0 {
        return method_ty;
    }
    let method_sym = checker
        .symbols
        .lookup_associated_member(type_id, method_name);
    let arg_vec = checker.type_info.type_interner.type_args(*args);
    let recv_args: Vec<ArType> = arg_vec.iter().map(|&a| checker.resolve(a)).collect();

    let param_syms: Vec<SymbolId> = if let Some(sym) = method_sym
        && let Some(gp) = checker.type_info.generic_params.get(&sym)
    {
        let n = recv_args.len().min(gp.len());
        gp.iter().copied().take(n).collect()
    } else if let Some(gp) = checker.type_info.generic_params.get(&type_id) {
        gp.iter().copied().take(recv_args.len()).collect()
    } else {
        return method_ty;
    };
    if param_syms.len() != recv_args.len() {
        return method_ty;
    }
    let subst = build_subst(&param_syms, &recv_args);
    substitute_type(&method_ty, &subst, &checker.type_info.type_interner)
}

fn concrete_type_id(ty: &ArType) -> Option<SymbolId> {
    match ty {
        ArType::Named(id, _) => Some(*id),
        _ => None,
    }
}
