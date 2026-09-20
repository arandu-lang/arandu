use rustc_hash::FxHashMap;

use arandu_lexer::Span;
use arandu_parser::{FuncSignature, GenericParam, WhereItem};
use smallvec::SmallVec;

use super::unify;
use super::{ArType, LowerCtx, TypeId};
use super::{GenericSubst, build_subst, substitute_type};
use crate::passes::type_checker::TypeChecker;
use crate::type_checker::info::InterfaceConstraint;
use crate::{ScopeId, SymbolId, SymbolKind};
use arandu_middle::types::lower::{lower_result_type_ctx, lower_type_expr_ctx};

#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: smol_str::SmolStr,
    pub sig_id: crate::type_checker::types::TypeId,
    pub generic_params: Vec<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub self_param: Option<SymbolId>,
    /// Method specifications for the interface.
    pub methods: Vec<InterfaceMethod>,
}

/// Collect interface method signatures and per-type-parameter trait constraints.
pub fn collect_interfaces_and_constraints(
    checker: &mut TypeChecker,
    program: &arandu_parser::Program,
) {
    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        use arandu_parser::TopLevelDecl;
        match decl {
            TopLevelDecl::Interface(iface) => collect_interface(checker, iface),
            TopLevelDecl::Struct(s) => {
                if let Some(sym) = checker
                    .resolved
                    .definitions
                    .get(&crate::NodeKey::from(s.span))
                {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &s.generic_params,
                        &s.where_clause,
                        s.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            TopLevelDecl::Enum(e) => {
                if let Some(sym) = checker
                    .resolved
                    .definitions
                    .get(&crate::NodeKey::from(e.span))
                {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &e.generic_params,
                        &e.where_clause,
                        e.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            TopLevelDecl::Func(f) => {
                let key = match &f.name {
                    arandu_parser::FuncName::Free { span, .. } => crate::NodeKey::from(*span),
                    arandu_parser::FuncName::Method { span, .. } => crate::NodeKey::from(*span),
                };
                if let Some(sym) = checker.resolved.definitions.get(&key) {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &f.generic_params,
                        &f.where_clause,
                        f.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            TopLevelDecl::TypeAlias(a) => {
                if let Some(sym) = checker
                    .resolved
                    .definitions
                    .get(&crate::NodeKey::from(a.span))
                {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &a.generic_params,
                        &[],
                        a.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            _ => {}
        }
    }
}

fn collect_interface(checker: &mut TypeChecker, decl: &arandu_parser::InterfaceDecl) {
    let Some(iface_sym) = checker
        .resolved
        .definitions
        .get(&crate::NodeKey::from(decl.span))
        .copied()
    else {
        return;
    };
    let iface_scope = checker.symbols.get(iface_sym).scope;
    let type_param_symbols = super::extract_generic_param_symbols(checker, &decl.generic_params);
    if !type_param_symbols.is_empty() {
        checker
            .type_info
            .generic_params
            .insert(iface_sym, std::sync::Arc::new(type_param_symbols));
    }

    let self_span = arandu_lexer::Span::new(decl.span.file_id, decl.span.start, decl.span.start);
    let self_param = checker
        .resolved
        .definitions
        .get(&crate::NodeKey::from(self_span))
        .copied();

    let mut methods = Vec::new();
    for member in &decl.members {
        let sig_ty = lower_func_signature(checker, member, iface_scope);
        let sig_id = checker.intern(sig_ty);
        let generic_params = super::extract_generic_param_symbols(checker, &member.generic_params);
        methods.push(InterfaceMethod {
            name: member.name.clone(),
            sig_id,
            generic_params,
        });
        collect_decl_constraints(
            checker,
            &member.generic_params,
            &member.where_clause,
            member.span,
            None,
            iface_scope,
        );
    }

    checker.type_info.interfaces.insert(
        iface_sym,
        InterfaceInfo {
            self_param,
            methods,
        },
    );

    collect_decl_constraints(
        checker,
        &decl.generic_params,
        &decl.where_clause,
        decl.span,
        Some(iface_sym),
        iface_scope,
    );
}

fn lower_func_signature(checker: &mut TypeChecker, sig: &FuncSignature, scope: ScopeId) -> ArType {
    let ctx = LowerCtx {
        pool: checker.pool,
        symbols: &checker.symbols,
        scope,
        resolved: &checker.resolved,
    };
    let mut param_types = Vec::new();
    for param in &sig.params {
        let ty = lower_type_expr_ctx(param.ty, &ctx, &mut checker.type_info.type_interner);
        param_types.push(checker.type_info.type_interner.intern(ty));
    }
    let ret = if let Some(result) = &sig.result {
        lower_result_type_ctx(result, &ctx, &mut checker.type_info.type_interner)
    } else {
        ArType::Void
    };
    let ret_id = checker.type_info.type_interner.intern(ret);
    ArType::func(&param_types, ret_id, &checker.type_info.type_interner)
}

fn collect_decl_constraints(
    checker: &mut TypeChecker,
    generic_params: &[GenericParam],
    where_clause: &[WhereItem],
    decl_span: Span,
    decl_symbol: Option<SymbolId>,
    scope: ScopeId,
) {
    let param_symbols = super::extract_generic_param_symbols(checker, generic_params);

    if !param_symbols.is_empty()
        && let Some(decl_sym) = decl_symbol
    {
        checker
            .type_info
            .generic_params
            .entry(decl_sym)
            .or_insert_with(|| std::sync::Arc::new(param_symbols.clone()));
    }

    let name_to_sym: FxHashMap<smol_str::SmolStr, SymbolId> = generic_params
        .iter()
        .zip(param_symbols.iter())
        .map(|(gp, sym)| (gp.name.clone(), *sym))
        .collect();

    for gp in generic_params {
        let Some(&param_sym) = name_to_sym.get(&gp.name) else {
            continue;
        };
        if let Some(const_ty) = gp.const_ty {
            let ctx = LowerCtx {
                pool: checker.pool,
                symbols: &checker.symbols,
                scope,
                resolved: &checker.resolved,
            };
            let declared =
                lower_type_expr_ctx(const_ty, &ctx, &mut checker.type_info.type_interner);
            let declared_id = checker.type_info.type_interner.intern(declared.clone());
            checker.type_info.record_decl_type(param_sym, declared_id);
            if !matches!(&declared, ArType::Primitive(primitive) if primitive.is_integer()) {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T011GenericConstraintNotSatisfied,
                    "const generic parameters require a scalar integer type".to_string(),
                    checker.pool.type_expr_span(const_ty),
                ));
            }
        }
        // T2.1: register default type arg for this type parameter.
        if let Some(def_ty_id) = gp.default {
            let ctx = LowerCtx {
                pool: checker.pool,
                symbols: &checker.symbols,
                scope,
                resolved: &checker.resolved,
            };
            let def_ty = lower_type_expr_ctx(def_ty_id, &ctx, &mut checker.type_info.type_interner);
            let tid = checker.type_info.type_interner.intern(def_ty);
            checker.type_info.generic_defaults.insert(param_sym, tid);
        }
        for &constraint in &gp.constraints {
            if let Some(bound) = resolve_interface_constraint(checker, constraint, scope) {
                let entry = checker
                    .type_info
                    .param_constraints
                    .entry(param_sym)
                    .or_insert_with(|| std::sync::Arc::new(Vec::new()));
                std::sync::Arc::make_mut(entry).push(bound);
            }
        }
    }

    for item in where_clause {
        let Some(&param_sym) = name_to_sym.get(&item.name) else {
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T011GenericConstraintNotSatisfied,
                format!(
                    "where clause '{}' does not name a generic parameter of this declaration",
                    item.name
                ),
                item.span,
            ));
            continue;
        };
        for &constraint in &item.constraints {
            if let Some(bound) = resolve_interface_constraint(checker, constraint, scope) {
                let entry = checker
                    .type_info
                    .param_constraints
                    .entry(param_sym)
                    .or_insert_with(|| std::sync::Arc::new(Vec::new()));
                std::sync::Arc::make_mut(entry).push(bound);
            }
        }
    }

    let _ = decl_span;
}

fn resolve_interface_constraint(
    checker: &mut TypeChecker,
    constraint_id: arandu_parser::TypeExprId,
    scope: ScopeId,
) -> Option<InterfaceConstraint> {
    let expr = checker.pool.type_expr(constraint_id);
    let (name, args) = match expr {
        arandu_parser::TypeExpr::Named { name, args, .. } => (name, *args),
        _ => {
            let span = checker.pool.type_expr_span(constraint_id);
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T011GenericConstraintNotSatisfied,
                "expected interface name in constraint".to_string(),
                span,
            ));
            return None;
        }
    };
    let key = crate::NodeKey::from(name.span);
    let Some(sym) = checker.resolved.type_refs.get(&key).copied() else {
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::N002UndefinedType,
            format!("unknown constraint type '{}'", name.path.join(".")),
            name.span,
        ));
        return None;
    };
    if checker.symbols.get(sym).kind != SymbolKind::Interface {
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::T011GenericConstraintNotSatisfied,
            format!(
                "'{}' is not an interface and cannot be used as a type constraint",
                name.path.join(".")
            ),
            name.span,
        ));
        return None;
    }
    let arg_expr_ids = checker.pool.type_expr_list(args).to_vec();
    let ctx = LowerCtx {
        pool: checker.pool,
        symbols: &checker.symbols,
        scope,
        resolved: &checker.resolved,
    };
    let mut type_args = SmallVec::<[TypeId; 2]>::new();
    for arg_expr in arg_expr_ids {
        let ty = lower_type_expr_ctx(arg_expr, &ctx, &mut checker.type_info.type_interner);
        type_args.push(checker.type_info.type_interner.intern(ty));
    }
    Some(InterfaceConstraint {
        iface_sym: sym,
        type_args,
    })
}

/// After monomorphic instantiation, verify each type argument satisfies its constraints.
pub(crate) fn check_instantiation_constraints(
    checker: &mut TypeChecker,
    param_symbols: &[SymbolId],
    arg_types: &[ArType],
    span: Span,
) {
    for (&param_sym, arg_ty) in param_symbols.iter().zip(arg_types) {
        let Some(parameter) = checker.symbols.try_get(param_sym) else {
            continue;
        };
        let valid_kind = match parameter.kind {
            SymbolKind::ConstParam => matches!(arg_ty, ArType::Const(_) | ArType::ConstParam(_)),
            SymbolKind::TypeParam => !matches!(arg_ty, ArType::Const(_) | ArType::ConstParam(_)),
            _ => true,
        };
        if !valid_kind {
            let expected = if parameter.kind == SymbolKind::ConstParam {
                "a compile-time scalar value"
            } else {
                "a type"
            };
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T011GenericConstraintNotSatisfied,
                format!("generic parameter '{}' expects {expected}", parameter.name),
                span,
            ));
        }
    }

    // Bounds may reference another parameter of this declaration (J: Job<R>).
    // Instantiate that obligation in the caller's type environment before
    // comparing it with the caller's declared bounds. Build only when needed.
    let mut substitution = None;
    for (param_sym, arg_ty) in param_symbols.iter().zip(arg_types) {
        let constraints = checker.type_info.param_constraints.get(param_sym).cloned();
        let Some(constraints) = constraints else {
            continue;
        };
        for bound in constraints.iter() {
            let bound_args: SmallVec<[TypeId; 2]> = if bound.type_args.is_empty() {
                SmallVec::new()
            } else {
                let subst =
                    substitution.get_or_insert_with(|| build_subst(param_symbols, arg_types));
                bound
                    .type_args
                    .iter()
                    .map(|&id| {
                        arandu_middle::types::subst::substitute_type_id(
                            id,
                            subst,
                            &checker.type_info.type_interner,
                        )
                    })
                    .collect()
            };
            if !type_satisfies_interface(checker, arg_ty, bound.iface_sym, &bound_args, span) {
                let mut iface_name = checker
                    .symbols
                    .try_get(bound.iface_sym)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "Interface".to_string());
                if !bound_args.is_empty() {
                    iface_name.push('<');
                    for (index, &argument) in bound_args.iter().enumerate() {
                        if index != 0 {
                            iface_name.push_str(", ");
                        }
                        iface_name.push_str(
                            &checker
                                .resolve(argument)
                                .display(&checker.symbols, &checker.type_info.type_interner),
                        );
                    }
                    iface_name.push('>');
                }
                let ty_display = arg_ty.display(&checker.symbols, &checker.type_info.type_interner);
                let detail = missing_methods_note(checker, arg_ty, bound.iface_sym, &bound_args);
                // Put the method-level root cause in the primary message (notes are easy to miss).
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T025InterfaceNotSatisfied,
                    format!(
                        "type '{ty_display}' does not satisfy interface '{iface_name}': {detail}"
                    ),
                    span,
                )
                .with_note(detail);
                checker.diagnostics.push(diag);
            }
        }
    }
}

fn missing_methods_note(
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
            && super::transfer::satisfies(checker, concrete, capability);
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
        let provided_stripped = strip_impl_receiver(provided, checker);
        if !method_types_compatible(&required_stripped, &provided_stripped, checker) {
            return false;
        }
    }
    true
}

#[cold]
fn missing_interface_methods(
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
        let provided_stripped = strip_impl_receiver(provided, checker);
        if !method_types_compatible(&required_stripped, &provided_stripped, checker) {
            missing.push(format!("{method} (signature mismatch)"));
        }
    }
    missing
}

fn concrete_type_id(ty: &ArType) -> Option<SymbolId> {
    match ty {
        ArType::Named(id, _) => Some(*id),
        _ => None,
    }
}

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

fn lookup_method_type(checker: &TypeChecker, type_id: SymbolId, method: &str) -> Option<ArType> {
    let sym = checker.symbols.lookup_associated_member(type_id, method)?;
    checker.decl_type(sym)
}

/// Drop leading `Self` / `&Self` / `&mut Self` (or concrete receiver type) from an interface method formal.
fn strip_interface_receiver(
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
fn strip_impl_receiver(ty: ArType, checker: &TypeChecker<'_>) -> ArType {
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

fn method_types_compatible(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_checker::ResolvedNames;
    use crate::type_checker::types::Primitive;
    use arandu_lexer::Span;
    use arandu_middle::symbol_table::{Symbol, SymbolTable};
    use arandu_middle::{ScopeId, SymbolId, SymbolKind};
    use arandu_parser::Program;
    use arandu_parser::ast_pool::AstPool;

    #[test]
    fn test_collect_interfaces_and_constraints_empty() {
        let pool = AstPool::default();
        let symbols = SymbolTable::new(0);
        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let program = Program {
            span: Span::new(0, 0, 0),
            module: None,
            imports: Vec::new(),
            decls: Vec::new(),
            docs: Vec::new(),
            pool: AstPool::default(),
        };

        collect_interfaces_and_constraints(&mut checker, &program);
        assert!(checker.type_info.interfaces.is_empty());
    }

    #[test]
    fn test_type_satisfies_interface() {
        let pool = AstPool::default();
        let mut symbols = SymbolTable::new(0);

        let iface_sym = SymbolId::new(1, 0);
        let iface_symbol = Symbol {
            id: iface_sym,
            name: "Reader".into(),
            kind: SymbolKind::Interface,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
            lang_item: None,
        };
        symbols.register_imported_symbol(iface_symbol);

        let struct_sym_id = SymbolId::new(1, 1);
        let struct_symbol = Symbol {
            id: struct_sym_id,
            name: "MyStruct".into(),
            kind: SymbolKind::Struct,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
            lang_item: None,
        };
        symbols.register_imported_symbol(struct_symbol);

        let self_sym_id = SymbolId::new(1, 99);
        let self_symbol = Symbol {
            id: self_sym_id,
            name: "Self".into(),
            kind: SymbolKind::TypeParam,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
            lang_item: None,
        };
        symbols.register_imported_symbol(self_symbol);

        let method_sym_id = SymbolId::new(1, 2);
        let method_symbol = Symbol {
            id: method_sym_id,
            name: "read".into(),
            kind: SymbolKind::Func,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
            lang_item: None,
        };
        symbols.register_imported_symbol(method_symbol);

        let mut associated = rustc_hash::FxHashMap::default();
        associated.insert((struct_sym_id, "read".into()), method_sym_id);
        symbols.associated_members = associated;

        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let self_type_id = checker.intern(ArType::named(
            struct_sym_id,
            &[],
            &checker.type_info.type_interner,
        ));
        let self_interface_type_id = checker.intern(ArType::named(
            self_sym_id,
            &[],
            &checker.type_info.type_interner,
        ));
        let req_method_type = ArType::func(
            &[self_interface_type_id],
            checker.intern(ArType::Primitive(Primitive::Int)),
            &checker.type_info.type_interner,
        );
        let prov_method_type = ArType::func(
            &[self_type_id],
            checker.intern(ArType::Primitive(Primitive::Int)),
            &checker.type_info.type_interner,
        );
        let req_method_type_id = checker.intern(req_method_type);
        let prov_method_type_id = checker.intern(prov_method_type);

        checker
            .type_info
            .decl_types
            .insert(method_sym_id, prov_method_type_id);

        let iface_info = InterfaceInfo {
            self_param: Some(self_sym_id),
            methods: vec![InterfaceMethod {
                name: "read".into(),
                sig_id: req_method_type_id,
                generic_params: Vec::new(),
            }],
        };
        checker.type_info.interfaces.insert(iface_sym, iface_info);

        let concrete = ArType::named(struct_sym_id, &[], &checker.type_info.type_interner);
        assert!(type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            &[],
            Span::new(0, 0, 0)
        ));

        checker.symbols.associated_members.clear();
        assert!(!type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            &[],
            Span::new(0, 0, 0)
        ));

        let missing = missing_interface_methods(&mut checker, &concrete, iface_sym, &[]);
        assert_eq!(missing, vec!["read".to_string()]);
    }
}
