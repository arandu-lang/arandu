use super::super::{ArType, LowerCtx, TypeId};
use super::{InterfaceInfo, InterfaceMethod};
use crate::passes::type_checker::TypeChecker;
use crate::type_checker::info::InterfaceConstraint;
use crate::type_checker::types::lower::{lower_result_type_ctx, lower_type_expr_ctx};
use crate::{ScopeId, SymbolId, SymbolKind};
use arandu_lexer::Span;
use arandu_parser::{FuncSignature, GenericParam, WhereItem};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

/// Collect interface method signatures and per-type-parameter trait constraints.
pub fn collect_interfaces_and_constraints(
    checker: &mut TypeChecker,
    program: &arandu_parser::Program,
) {
    program.for_each_decl_recursive(|_decl_id, decl| {
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
    });
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
    let type_param_symbols =
        super::super::extract_generic_param_symbols(checker, &decl.generic_params);
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
        let generic_params =
            super::super::extract_generic_param_symbols(checker, &member.generic_params);
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
            sealed: decl.sealed,
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
    for param in &sig.params {
        crate::type_checker::check::validate_const_arguments(checker, param.ty);
    }
    if let Some(result) = &sig.result {
        crate::type_checker::check::validate_const_result(checker, result);
    }
    let ctx = LowerCtx {
        pool: checker.pool,
        symbols: &checker.symbols,
        scope,
        resolved: &checker.resolved,
    };
    let mut param_types = Vec::new();
    for param in &sig.params {
        let ty = lower_type_expr_ctx(param.ty, &ctx, &mut checker.type_info.type_interner);
        let bare = checker.type_info.type_interner.intern(ty);
        let param_ty = match param.ownership {
            Some(arandu_parser::Ownership::Shared) => {
                checker.type_info.type_interner.intern(ArType::Ref(bare))
            }
            Some(arandu_parser::Ownership::Mut) => {
                checker.type_info.type_interner.intern(ArType::RefMut(bare))
            }
            _ => bare,
        };
        param_types.push(param_ty);
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
    let param_symbols = super::super::extract_generic_param_symbols(checker, generic_params);

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
        // Header defaults/constraints are not pre-body staging environments.
        // Report computed arguments before an Error type can masquerade as an
        // accepted default. This validator is pure and never calls the VM.
        for ty in gp
            .const_ty
            .iter()
            .chain(gp.default.iter())
            .chain(gp.constraints.iter())
        {
            crate::type_checker::check::validate_const_arguments(checker, *ty);
        }
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
            if !matches!(&declared, ArType::Primitive(primitive) if primitive.is_integer() || *primitive == super::super::Primitive::Bool)
                && !matches!(
                    &declared,
                    ArType::Named(..)
                        | ArType::Tuple(_)
                        | ArType::Array(..)
                        | ArType::Option(_)
                        | ArType::Result(..)
                )
            {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T011GenericConstraintNotSatisfied,
                    "const generic parameters require an integer, bool, or closed Copy aggregate"
                        .to_string(),
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
        let unresolved_import = name
            .path
            .first()
            .is_some_and(|root| checker.symbols.unresolved_module_aliases.contains(root));
        if !unresolved_import
            && !checker.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == crate::DiagCode::M002UndefinedNamespaceMember
                    && diagnostic.span == name.span
            })
        {
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::N002UndefinedType,
                format!("unknown constraint type '{}'", name.path.join(".")),
                name.span,
            ));
        }
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
    for &arg in &arg_expr_ids {
        crate::type_checker::check::validate_const_arguments(checker, arg);
    }
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
